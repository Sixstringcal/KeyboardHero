use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use kbhero_core::{
    config::Config,
    db::ShortcutDatabase,
    resolver::ShortcutResolver,
    types::{Platform, ShortcutMatch},
};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let db = Arc::new(ShortcutDatabase::build().expect("shortcut database must be valid"));
    let config = Arc::new(RwLock::new(load_config()));

    let (raw_tx, raw_rx) = mpsc::channel(32);
    let (hint_tx, mut hint_rx) = mpsc::channel::<ShortcutMatch>(8);

    let resolver = ShortcutResolver::new(Arc::clone(&db), Arc::clone(&config), Platform::current());
    tokio::spawn(resolver.run(raw_rx, hint_tx));

    // Placeholder: real platform detection engines will feed raw_tx.
    // Until Milestone 1 the channel simply stays open.
    let _raw_tx_keep_alive = raw_tx;

    eprintln!("KeyboardHero daemon started (platform={:?})", Platform::current());

    while let Some(m) = hint_rx.recv().await {
        eprintln!("hint: {} — {}", m.menu_path, m.shortcut_keys);
    }
}

fn load_config() -> Config {
    let path = config_path();
    Config::load(&path).unwrap_or_else(|e| {
        eprintln!("warning: could not load config ({e}); using defaults");
        Config::default()
    })
}

fn config_path() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                std::path::PathBuf::from(home).join(".config")
            });
        base.join("keyboardhero/config.toml")
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        std::path::PathBuf::from(appdata).join("keyboardhero\\config.toml")
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        std::path::PathBuf::from(home)
            .join("Library/Application Support/keyboardhero/config.toml")
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        std::path::PathBuf::from("config.toml")
    }
}
