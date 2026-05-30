use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::config::Config;
use crate::db::ShortcutDatabase;
use crate::types::{ElementRole, Platform, RawActivationEvent, ResolutionSource, ShortcutMatch};

pub struct ShortcutResolver {
    db:       Arc<ShortcutDatabase>,
    config:   Arc<RwLock<Config>>,
    platform: Platform,
}

impl ShortcutResolver {
    pub fn new(
        db:       Arc<ShortcutDatabase>,
        config:   Arc<RwLock<Config>>,
        platform: Platform,
    ) -> Self {
        Self { db, config, platform }
    }

    /// Consume raw events until the sender is closed.
    pub async fn run(
        self,
        mut rx: mpsc::Receiver<RawActivationEvent>,
        tx:     mpsc::Sender<ShortcutMatch>,
    ) {
        while let Some(event) = rx.recv().await {
            if let Some(m) = self.resolve(&event).await {
                let _ = tx.send(m).await;
            }
        }
    }

    async fn resolve(&self, event: &RawActivationEvent) -> Option<ShortcutMatch> {
        {
            let config = self.config.read().await;
            if config.behavior.paused {
                return None;
            }
            if config.behavior.excluded_apps.iter()
                .any(|e| e.eq_ignore_ascii_case(&event.app.executable))
            {
                return None;
            }
        }

        match &event.role {
            ElementRole::MenuItem
            | ElementRole::ToolbarButton
            | ElementRole::ContextMenuItem => {
                // Tier 1: use the shortcut the app already advertised, if present.
                if let Some(raw) = &event.discovered_shortcut {
                    let shortcut_keys = normalize_shortcut(raw);
                    let action_name   = event.label_path.last().cloned().unwrap_or_default();

                    // When the accessibility API gave us a shortcut but no label
                    // (e.g. gnome-text-editor omits accessible names), fill in the
                    // action name and description from the database reverse index.
                    if action_name.is_empty() {
                        if let Some(db) = self.db.lookup_by_shortcut(&event.app, &shortcut_keys, self.platform) {
                            let m = ShortcutMatch {
                                shortcut_keys,
                                action_name:  db.action_name,
                                description:  db.description,
                                menu_path:    db.menu_path,
                                source:       ResolutionSource::Dynamic,
                            };
                            eprintln!("[resolve] Tier1+DB shortcut={:?} action={:?}", m.shortcut_keys, m.action_name);
                            return Some(m);
                        }
                    }

                    let m = ShortcutMatch {
                        shortcut_keys,
                        action_name,
                        description:   String::new(),
                        menu_path:     event.label_path.join(" › "),
                        source:        ResolutionSource::Dynamic,
                    };
                    eprintln!("[resolve] Tier1 shortcut={:?} action={:?}", m.shortcut_keys, m.action_name);
                    return Some(m);
                }
                // Tier 2: database fallback.
                let result = self.db.lookup_menu(&event.app, &event.label_path, self.platform);
                eprintln!("[resolve] app={:?} path={:?} → {:?}",
                    event.app.executable, event.label_path,
                    result.as_ref().map(|m| &m.shortcut_keys));
                result
            }

            ElementRole::SemanticElement { role, context } => {
                // Semantic actions have no Tier 1 path.
                self.db.lookup_semantic(
                    &event.app,
                    role,
                    context.as_deref(),
                    self.platform,
                )
            }
        }
    }
}

// ── Shortcut normalization ─────────────────────────────────────────────────────

/// Converts an AT-SPI2 / ARIA shortcut string into the display form used by
/// the callout renderer.
///
/// AT-SPI2 uses GLib key names ("Control+N", "shift+alt+F4").  The database
/// already stores shortcuts in display form ("Ctrl+C"), so this is only applied
/// to Tier 1 dynamically-discovered shortcuts.
fn normalize_shortcut(raw: &str) -> String {
    raw.split('+')
        .map(|part| match part.trim() {
            // Modifier keys
            "Control" | "control" => "Ctrl",
            "Shift"   | "shift"   => "Shift",
            "Alt"     | "alt"     => "Alt",
            "Super"   | "super"
            | "Meta"  | "meta"
            | "Win"   | "win"     => "Super",

            // Special keys
            "Return" | "KP_Enter"     => "Enter",
            "Escape"                  => "Esc",
            "Delete"                  => "Del",
            "BackSpace" | "Backspace" => "Backspace",
            "Tab"                     => "Tab",
            "space"                   => "Space",
            "Up"                      => "↑",
            "Down"                    => "↓",
            "Left"                    => "←",
            "Right"                   => "→",
            "Home"                    => "Home",
            "End"                     => "End",
            "Page_Up"                 => "PgUp",
            "Page_Down"               => "PgDn",

            // Everything else (letter keys, F-keys, symbols) passes through as-is.
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use crate::types::AppIdentity;

    fn make_resolver() -> ShortcutResolver {
        let db = Arc::new(ShortcutDatabase::build().unwrap());
        let config = Arc::new(RwLock::new(Config::default()));
        ShortcutResolver::new(db, config, Platform::Linux)
    }

    fn event(exe: &str, role: ElementRole, path: &[&str], discovered: Option<&str>) -> RawActivationEvent {
        RawActivationEvent {
            timestamp:           Instant::now(),
            app:                 AppIdentity { executable: exe.to_string(), window_title: None, pid: 1 },
            role,
            label_path:          path.iter().map(|s| s.to_string()).collect(),
            discovered_shortcut: discovered.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn tier1_takes_priority_over_database() {
        let r = make_resolver();
        let e = event("firefox", ElementRole::MenuItem, &["Edit", "Copy"], Some("Ctrl+C (dynamic)"));
        let m = r.resolve(&e).await.unwrap();
        assert_eq!(m.source, ResolutionSource::Dynamic);
        assert_eq!(m.shortcut_keys, "Ctrl+C (dynamic)");
    }

    #[tokio::test]
    async fn falls_back_to_database_when_no_discovered_shortcut() {
        let r = make_resolver();
        let e = event("firefox", ElementRole::MenuItem, &["Edit", "Copy"], None);
        let m = r.resolve(&e).await.unwrap();
        assert_eq!(m.source, ResolutionSource::Database);
        assert_eq!(m.shortcut_keys, "Ctrl+C");
    }

    #[tokio::test]
    async fn excluded_app_returns_none() {
        let db = Arc::new(ShortcutDatabase::build().unwrap());
        let mut cfg = Config::default();
        cfg.behavior.excluded_apps = vec!["firefox".into()];
        let config = Arc::new(RwLock::new(cfg));
        let r = ShortcutResolver::new(db, config, Platform::Linux);
        let e = event("firefox", ElementRole::MenuItem, &["Edit", "Copy"], Some("Ctrl+C"));
        assert!(r.resolve(&e).await.is_none());
    }

    #[tokio::test]
    async fn paused_returns_none() {
        let db = Arc::new(ShortcutDatabase::build().unwrap());
        let mut cfg = Config::default();
        cfg.behavior.paused = true;
        let config = Arc::new(RwLock::new(cfg));
        let r = ShortcutResolver::new(db, config, Platform::Linux);
        let e = event("firefox", ElementRole::MenuItem, &["Edit", "Copy"], Some("Ctrl+C"));
        assert!(r.resolve(&e).await.is_none());
    }

    #[tokio::test]
    async fn semantic_element_resolves() {
        let r = make_resolver();
        let e = event(
            "firefox",
            ElementRole::SemanticElement {
                role:    "tab".into(),
                context: Some("tab-not-selected".into()),
            },
            &[],
            None,
        );
        let m = r.resolve(&e).await.unwrap();
        assert_eq!(m.shortcut_keys, "Ctrl+Tab");
    }
}
