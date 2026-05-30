use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use kbhero_core::{
    config::Config,
    db::ShortcutDatabase,
    resolver::ShortcutResolver,
    types::Platform,
};

fn main() {
    #[cfg(target_os = "linux")]
    linux_main();

    #[cfg(target_os = "windows")]
    eprintln!("Windows backend not yet implemented (Milestone 2)");

    #[cfg(target_os = "macos")]
    eprintln!("macOS backend not yet implemented (Milestone 3)");
}

// ── Linux ─────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn linux_main() {
    use kbhero_linux::{detection::AtSpi2Engine, overlay};
    use kbhero_linux::overlay::CalloutMsg;

    // Choose GDK backend before GTK initialises (and before any threads start).
    //
    // Preferred order:
    //   1. gtk4-layer-shell   — proper Wayland anchoring (KDE, Sway, Hyprland…)
    //                           TODO: implement when gtk4-layer-shell crate is available
    //   2. XWayland (X11 via GDK_BACKEND=x11)
    //                         — override-redirect positioning; works on GNOME Wayland and
    //                           any other compositor that runs XWayland
    //   3. Unpositioned       — pure Wayland without XWayland; window position is
    //                           compositor-controlled (graceful fallback, logged at startup)
    //
    // SAFETY: set_var is called before any threads are spawned.
    select_gdk_backend();

    let db = Arc::new(ShortcutDatabase::build().expect("shortcut database must be valid"));
    let cfg = load_config();
    let duration_ms = cfg.display.callout_duration_ms;
    let config = Arc::new(RwLock::new(cfg));

    // std mpsc: background tokio thread → GTK main thread (polled every 16ms)
    let (glib_tx, glib_rx) = std::sync::mpsc::channel::<CalloutMsg>();

    // AT-SPI2 detection + resolver run on a dedicated tokio runtime
    let config_bg = Arc::clone(&config);
    let db_bg = Arc::clone(&db);
    std::thread::Builder::new()
        .name("kbhero-engine".into())
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime")
                .block_on(async move {
                    eprintln!("[engine-thread] tokio runtime started");
                    let engine = match AtSpi2Engine::connect().await {
                        Ok(e) => e,
                        Err(e) => {
                            eprintln!("error: could not connect to AT-SPI2: {e}");
                            eprintln!("hint: ensure accessibility services are running");
                            return;
                        }
                    };

                    let (raw_tx, raw_rx) = mpsc::channel(32);
                    let (hint_tx, mut hint_rx) = mpsc::channel(8);

                    let resolver = ShortcutResolver::new(db_bg, config_bg, Platform::current());
                    tokio::spawn(resolver.run(raw_rx, hint_tx));
                    tokio::spawn(engine.run(raw_tx));

                    while let Some(m) = hint_rx.recv().await {
                        let msg = CalloutMsg { m, duration_ms };
                        if glib_tx.send(msg).is_err() {
                            break; // GTK main loop exited
                        }
                    }
                });
        })
        .expect("spawn engine thread");

    eprintln!("KeyboardHero started (platform=Linux)");
    eprintln!("Click any menu item in a running application to see its keyboard shortcut.");

    // GTK main loop — blocks until the process is killed or the window closes
    overlay::run(glib_rx);
}

// ── Backend selection ─────────────────────────────────────────────────────────

/// Sets `GDK_BACKEND` when needed, before `gtk4::init()` is called.
///
/// Must be called before any threads are spawned (std::env::set_var is not
/// thread-safe).
///
/// GNOME Wayland sessions export `GDK_BACKEND=wayland` into every process
/// environment.  We must override that value — the `GDK_BACKEND` we care
/// about respecting is an explicit user choice of `x11`, not the compositor's
/// ambient default.
#[cfg(target_os = "linux")]
fn select_gdk_backend() {
    let on_wayland  = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let xwayland    = std::env::var_os("DISPLAY").is_some();
    let gdk_backend = std::env::var("GDK_BACKEND").unwrap_or_default();

    eprintln!("[main] env: WAYLAND_DISPLAY={} DISPLAY={} GDK_BACKEND={:?}",
        if on_wayland { "set" } else { "unset" },
        if xwayland   { "set" } else { "unset" },
        gdk_backend,
    );

    // Already on native X11, or the user explicitly chose the X11 backend.
    if !on_wayland || gdk_backend.eq_ignore_ascii_case("x11") {
        return;
    }

    // TODO: when gtk4-layer-shell support is added, probe for zwlr_layer_shell_v1
    // here (via a raw wayland-client connection) and prefer the Wayland backend
    // with layer-shell over the XWayland fallback.

    if xwayland {
        // Override any compositor-injected GDK_BACKEND (e.g. GNOME sets
        // "wayland") and use the X11/XWayland backend for override-redirect
        // window positioning.
        // SAFETY: no threads have been spawned at this call site.
        unsafe { std::env::set_var("GDK_BACKEND", "x11"); }
        eprintln!("[main] forcing GDK_BACKEND=x11 (was {:?}) — XWayland overlay positioning", gdk_backend);
    } else {
        eprintln!("[main] pure Wayland (no XWayland) — overlay position controlled by compositor");
    }
    // Positioner::detect() will confirm the actual backend after gtk4::init().
}

// ── Config loading ────────────────────────────────────────────────────────────

fn load_config() -> Config {
    Config::load(&config_path()).unwrap_or_else(|e| {
        eprintln!("warning: could not read config ({e}); using defaults");
        Config::default()
    })
}

fn config_path() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_default();
                std::path::PathBuf::from(home).join(".config")
            });
        return base.join("keyboardhero/config.toml");
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        return std::path::PathBuf::from(appdata).join("keyboardhero\\config.toml");
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        return std::path::PathBuf::from(home)
            .join("Library/Application Support/keyboardhero/config.toml");
    }

    #[allow(unreachable_code)]
    std::path::PathBuf::from("config.toml")
}
