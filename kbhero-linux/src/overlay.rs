#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use kbhero_core::types::ShortcutMatch;

/// Sent from the tokio background thread to the GTK main thread.
pub struct CalloutMsg {
    pub m:           ShortcutMatch,
    pub duration_ms: u32,
}

// Safety: ShortcutMatch contains only owned Strings and a Copy enum.
// The Sender end lives on the background thread; Receiver stays on the main thread.
unsafe impl Send for CalloutMsg {}

const CSS: &str = "
window.kbhero-callout {
    background-color: rgba(44, 44, 46, 0.96);
    border-radius: 12px;
    border: 1px solid #48484A;
}
.kbhero-box {
    padding: 14px 22px;
}
.kbhero-action {
    color: #F2F2F7;
    font-size: 15pt;
    font-weight: 500;
    margin-bottom: 8px;
}
.kbhero-keys-row {
    margin-bottom: 0px;
}
.kbhero-keycap {
    background-color: #3A3A3C;
    border-radius: 5px;
    border: 1px solid rgba(255, 255, 255, 0.22);
    padding: 4px 10px;
    color: #F2F2F7;
    font-size: 13pt;
    font-weight: 600;
    margin: 0 1px;
}
.kbhero-separator {
    color: rgba(242, 242, 247, 0.35);
    font-size: 11pt;
    font-weight: 300;
    margin: 0 3px;
}
.kbhero-path {
    color: rgba(242, 242, 247, 0.40);
    font-size: 9pt;
    margin-top: 7px;
}
";

// ── Backend detection ─────────────────────────────────────────────────────────

/// How the overlay window will be positioned.
///
/// Determined once after `gtk4::init()` and reused for every callout window.
/// Variants are ordered by preference: X11 is best-effort but universal;
/// Unpositioned is the graceful fallback when nothing else applies.
///
/// Future: `LayerShell` will be added here once `gtk4-layer-shell` support
/// lands (Milestone 2 / KDE + wlr-based compositors).
enum Positioner {
    /// GDK is using the X11 backend (native X11 or XWayland).
    /// `override-redirect` + `x11rb::ConfigureWindow` gives pixel-accurate placement.
    X11,

    /// GDK is using the Wayland backend without layer-shell support.
    /// The window position is entirely compositor-controlled; we emit a warning
    /// at startup so users know why the callout may not appear at bottom-center.
    Unpositioned,
}

impl Positioner {
    /// Inspect the live GDK display to decide which strategy to use.
    ///
    /// Must be called *after* `gtk4::init()` — that is when GDK has bound to a
    /// backend and its display type name is known.
    fn detect() -> Self {
        let type_name = gtk4::gdk::Display::default()
            .map(|d| d.type_().name().to_string())
            .unwrap_or_else(|| "none".into());

        eprintln!("[overlay] GDK display type: {type_name}");

        if type_name.contains("X11") {
            Positioner::X11
        } else {
            Positioner::Unpositioned
        }
    }

    fn position_window(&self, win: &gtk4::Window) {
        match self {
            Positioner::X11 => x11_position_bottom_center(win),
            Positioner::Unpositioned => {
                // No action: window appears wherever the compositor decides.
            }
        }
    }
}

// ── GTK main loop ─────────────────────────────────────────────────────────────

/// Initialize GTK4 and run the main loop.  Blocks until the process is killed.
///
/// `rx` delivers `CalloutMsg` values produced by the resolver on a background
/// thread.  We poll it every 16 ms via a glib timeout source — latency is fine
/// for a visual hint, and this avoids the thread-safety constraints of GTK
/// signal emission from outside the main loop.
pub fn run(rx: mpsc::Receiver<CalloutMsg>) {
    gtk4::init().expect("GTK4 init failed");

    let positioner = Positioner::detect();
    if matches!(positioner, Positioner::Unpositioned) {
        eprintln!(
            "[overlay] warning: running on Wayland without layer-shell support; \
             the callout window will be positioned by the compositor (not bottom-center). \
             Install gtk4-layer-shell or enable XWayland for proper placement."
        );
    }

    let provider = gtk4::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let active: Rc<RefCell<Option<gtk4::Window>>> = Rc::new(RefCell::new(None));
    let provider    = Rc::new(provider);
    let positioner  = Rc::new(positioner);
    let rx          = Rc::new(RefCell::new(rx));

    let active_poll     = Rc::clone(&active);
    let provider_poll   = Rc::clone(&provider);
    let positioner_poll = Rc::clone(&positioner);
    let rx_poll         = Rc::clone(&rx);

    glib::timeout_add_local(Duration::from_millis(16), move || {
        loop {
            match rx_poll.borrow().try_recv() {
                Ok(msg) => {
                    if let Some(old) = active_poll.borrow_mut().take() {
                        old.close();
                    }
                    let win = build_callout(&provider_poll, &positioner_poll, &msg);
                    let win_dismiss  = win.clone();
                    let active_close = Rc::clone(&active_poll);
                    glib::timeout_add_local_once(
                        Duration::from_millis(msg.duration_ms as u64),
                        move || {
                            win_dismiss.close();
                            *active_close.borrow_mut() = None;
                        },
                    );
                    *active_poll.borrow_mut() = Some(win);
                }
                Err(mpsc::TryRecvError::Empty)        => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });

    glib::MainLoop::new(None, false).run();
}

fn build_callout(
    _provider:  &gtk4::CssProvider,
    positioner: &Positioner,
    msg:        &CalloutMsg,
) -> gtk4::Window {
    let win = gtk4::Window::new();
    win.set_decorated(false);
    win.set_resizable(false);
    win.set_deletable(false);
    win.add_css_class("kbhero-callout");

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("kbhero-box");

    // Action name — what the shortcut does, shown at the top in the largest text.
    if !msg.m.action_name.is_empty() {
        let action = gtk4::Label::new(Some(&msg.m.action_name));
        action.add_css_class("kbhero-action");
        action.set_halign(gtk4::Align::Start);
        outer.append(&action);
    }

    // Keycap badges — one styled badge per key, separated by "+" labels.
    let keys_row = build_keycaps(&msg.m.shortcut_keys);
    outer.append(&keys_row);

    // Menu-path breadcrumb — provides context when there is a parent menu.
    // The path is "Parent › Item"; skip it when there is nothing above the item.
    if msg.m.menu_path.contains('\u{203A}') {
        let path = gtk4::Label::new(Some(&msg.m.menu_path));
        path.add_css_class("kbhero-path");
        path.set_halign(gtk4::Align::Start);
        outer.append(&path);
    }

    win.set_child(Some(&outer));

    // Realize creates the underlying surface without mapping (showing) the window.
    // Positioning must happen after realize but before present so that
    // override-redirect is set before the WM sees the window.
    gtk4::prelude::WidgetExt::realize(&win);
    positioner.position_window(&win);
    win.present();
    win
}

// ── Keycap badge row ──────────────────────────────────────────────────────────

/// Builds a horizontal row of keycap badge labels for `shortcut_keys`.
///
/// The shortcut string uses "+" as a separator (e.g. "Ctrl+Shift+Z").
/// Each token becomes a styled badge; tokens are separated by a faint "+" label.
fn build_keycaps(shortcut_keys: &str) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    row.add_css_class("kbhero-keys-row");
    row.set_halign(gtk4::Align::Start);

    for (i, part) in shortcut_keys.split('+').enumerate() {
        let key = part.trim();
        if key.is_empty() {
            continue;
        }
        if i > 0 {
            let sep = gtk4::Label::new(Some("+"));
            sep.add_css_class("kbhero-separator");
            row.append(&sep);
        }
        let badge = gtk4::Label::new(Some(key));
        badge.add_css_class("kbhero-keycap");
        row.append(&badge);
    }

    row
}

// ── X11 / XWayland positioning ────────────────────────────────────────────────

const MARGIN_BOTTOM_PX: i32 = 48;

/// Moves the realized (not yet presented) window to the bottom-center of the
/// primary monitor and sets `override-redirect` so the WM does not relocate or
/// decorate it.
///
/// Calling this with a non-X11 GDK surface is a logic error; the `Positioner`
/// ensures we only reach this path when the GDK backend is confirmed X11.
fn x11_position_bottom_center(win: &gtk4::Window) {
    let (x, y) = bottom_center_coords(win);

    // Set override-redirect BEFORE mapping so the WM never gets to manage
    // or decorate this window.
    if let Some(surface) = win.surface() {
        x11_set_override_redirect(&surface);
    }

    // Move the window when it is actually mapped.  `connect_map` fires after
    // GTK's entire map sequence — including any internal XConfigureWindow calls
    // — so our position is applied last and wins.
    win.connect_map(move |w| {
        if let Some(surface) = w.surface() {
            x11_move_window(&surface, x, y);
        }
    });
}

/// Computes the (x, y) position that places a window of the measured natural
/// size at the bottom-center of the primary monitor.
fn bottom_center_coords(win: &gtk4::Window) -> (i32, i32) {
    let (_, nat_w, _, _) = win.measure(gtk4::Orientation::Horizontal, -1);
    let (_, nat_h, _, _) = win.measure(gtk4::Orientation::Vertical, nat_w);

    let w = nat_w.max(200);
    let h = nat_h.max(60);

    let geometry = gtk4::gdk::Display::default()
        .as_ref()
        .and_then(|d| d.monitors().item(0))
        .and_then(|o| o.downcast::<gtk4::gdk::Monitor>().ok())
        .map(|m| m.geometry())
        .unwrap_or_else(|| gtk4::gdk::Rectangle::new(0, 0, 1920, 1080));

    let x = geometry.x() + (geometry.width() - w) / 2;
    let y = geometry.y() + geometry.height() - h - MARGIN_BOTTOM_PX;

    eprintln!("[overlay] measure={w}×{h} monitor={}×{} → ({x},{y})",
        geometry.width(), geometry.height());

    (x, y)
}

/// Sets `override-redirect` on the X11 window so the WM never manages it.
/// Must be called on a realized but not yet mapped surface.
fn x11_set_override_redirect(surface: &gtk4::gdk::Surface) {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ChangeWindowAttributesAux, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    let Some(xid) = x11_surface_xid(surface) else {
        eprintln!("[overlay] could not obtain XID for override-redirect; skipping");
        return;
    };
    let display_str = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    let Ok((conn, _)) = RustConnection::connect(Some(&display_str)) else {
        eprintln!("[overlay] X11 connect failed for override-redirect");
        return;
    };
    let _ = conn.change_window_attributes(
        xid,
        &ChangeWindowAttributesAux::new().override_redirect(1),
    );
    let _ = conn.flush();
}

/// Moves the X11 window to (`x`, `y`).  Safe to call before or after mapping.
fn x11_move_window(surface: &gtk4::gdk::Surface, x: i32, y: i32) {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    let Some(xid) = x11_surface_xid(surface) else {
        eprintln!("[overlay] could not obtain XID for move; skipping");
        return;
    };
    let display_str = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    let Ok((conn, _)) = RustConnection::connect(Some(&display_str)) else {
        eprintln!("[overlay] X11 connect failed for move");
        return;
    };
    let _ = conn.configure_window(xid, &ConfigureWindowAux::new().x(x).y(y));
    let _ = conn.flush();
    eprintln!("[overlay] x11_move_window: xid={xid} → ({x},{y})");
}

/// Extracts the X11 window ID from the given GDK surface via the GDK X11 helper
/// exported from `libgtk-4.so`.
///
/// Only call this when the GDK backend is confirmed to be X11 (`Positioner::X11`).
fn x11_surface_xid(surface: &gtk4::gdk::Surface) -> Option<u32> {
    use std::os::raw::c_ulong;

    extern "C" {
        // Exported by libgtk-4.so on all X11-enabled GTK4 builds.
        fn gdk_x11_surface_get_xid(
            surface: *mut gtk4::gdk::ffi::GdkSurface,
        ) -> c_ulong;
    }

    unsafe {
        use gtk4::glib::translate::ToGlibPtr;
        let raw: *mut gtk4::gdk::ffi::GdkSurface = surface.to_glib_none().0;
        if raw.is_null() {
            return None;
        }
        let xid = gdk_x11_surface_get_xid(raw);
        (xid != 0).then_some(xid as u32)
    }
}
