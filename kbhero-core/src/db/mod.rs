mod matcher;
mod schema;
mod sources;

use std::collections::HashMap;
use thiserror::Error;

use matcher::{AppMatcher, GLOBAL_APP_ID};
use crate::types::{AppIdentity, Platform, ResolutionSource, ShortcutMatch};

/// Opaque identifier for an app registered in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppId(u32);

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("TOML parse error in '{file}': {source}")]
    ParseError {
        file:   &'static str,
        #[source]
        source: toml::de::Error,
    },
    #[error("shortcut entry in '{file}' has an empty `keys` array")]
    EmptyKeys { file: &'static str },
}

// ── Internal index keys ──────────────────────────────────────────────────────

#[derive(Hash, PartialEq, Eq)]
struct MenuKey {
    app_id:   AppId,
    platform: Platform,
    /// Each segment is lowercased and has trailing ellipsis stripped.
    path:     Vec<String>,
}

#[derive(Hash, PartialEq, Eq)]
struct SemanticKey {
    app_id:   AppId,
    platform: Platform,
    role:     String,
    context:  Option<String>,
}

// ── Internal entry types ─────────────────────────────────────────────────────

struct MenuEntry {
    keys:              String,
    description:       String,
    action_name:       String,
    menu_path_display: String,
}

struct SemanticEntry {
    keys:        String,
    description: String,
    role:        String,
}

// ── Public database ──────────────────────────────────────────────────────────

pub struct ShortcutDatabase {
    matcher:        AppMatcher,
    menu_table:     HashMap<MenuKey, MenuEntry>,
    semantic_table: HashMap<SemanticKey, SemanticEntry>,
}

impl ShortcutDatabase {
    /// Parse and index all bundled TOML shortcut files.
    pub fn build() -> Result<Self, DatabaseError> {
        let mut db = ShortcutDatabase {
            matcher:        AppMatcher::new(),
            menu_table:     HashMap::new(),
            semantic_table: HashMap::new(),
        };

        let mut next_id: u32 = 1;

        for (file, content) in sources::SOURCES {
            let parsed: schema::AppFile = toml::from_str(content)
                .map_err(|source| DatabaseError::ParseError { file, source })?;

            let app_id = if let Some(app_def) = &parsed.app {
                let id = AppId(next_id);
                next_id += 1;
                let pattern = app_def.title_pattern.as_str();
                let _ = db.matcher.add(
                    id,
                    app_def.executables.clone(),
                    Some(pattern),
                );
                id
            } else {
                GLOBAL_APP_ID
            };

            for shortcut in &parsed.shortcuts {
                let keys_def = shortcut.keys.first()
                    .ok_or(DatabaseError::EmptyKeys { file })?;

                let normalized: Vec<String> = shortcut.menu_path.iter()
                    .map(|s| normalize_segment(s))
                    .collect();
                let action_name = shortcut.menu_path.last()
                    .cloned()
                    .unwrap_or_default();
                let display_path = shortcut.menu_path.join(" › ");

                for (platform, keys_opt) in platform_iter(keys_def) {
                    let Some(keys) = keys_opt else { continue };
                    db.menu_table.insert(
                        MenuKey { app_id, platform, path: normalized.clone() },
                        MenuEntry {
                            keys,
                            description:       shortcut.description.clone(),
                            action_name:       action_name.clone(),
                            menu_path_display: display_path.clone(),
                        },
                    );
                }
            }

            for action in &parsed.semantic_actions {
                let keys_def = action.keys.first()
                    .ok_or(DatabaseError::EmptyKeys { file })?;

                for (platform, keys_opt) in platform_iter(keys_def) {
                    let Some(keys) = keys_opt else { continue };
                    db.semantic_table.insert(
                        SemanticKey {
                            app_id,
                            platform,
                            role:    action.role.clone(),
                            context: action.context.clone(),
                        },
                        SemanticEntry {
                            keys,
                            description: action.description.clone(),
                            role:        action.role.clone(),
                        },
                    );
                }
            }
        }

        Ok(db)
    }

    /// Look up a menu or toolbar shortcut via Tier 2 (database).
    /// Tries app-specific entry first, then falls back to the global table.
    pub fn lookup_menu(
        &self,
        app:        &AppIdentity,
        label_path: &[String],
        platform:   Platform,
    ) -> Option<ShortcutMatch> {
        let path: Vec<String> = label_path.iter().map(|s| normalize_segment(s)).collect();

        if let Some(app_id) = self.matcher.resolve(app) {
            let key = MenuKey { app_id, platform, path: path.clone() };
            if let Some(entry) = self.menu_table.get(&key) {
                return Some(entry.to_match(ResolutionSource::Database));
            }
        }

        let global_key = MenuKey { app_id: GLOBAL_APP_ID, platform, path };
        self.menu_table.get(&global_key)
            .map(|e| e.to_match(ResolutionSource::Database))
    }

    /// Look up a semantic action shortcut via Tier 2 (database).
    /// Tries app-specific entry first, then falls back to the global table.
    pub fn lookup_semantic(
        &self,
        app:      &AppIdentity,
        role:     &str,
        context:  Option<&str>,
        platform: Platform,
    ) -> Option<ShortcutMatch> {
        let context_owned = context.map(str::to_string);

        if let Some(app_id) = self.matcher.resolve(app) {
            let key = SemanticKey {
                app_id,
                platform,
                role:    role.to_string(),
                context: context_owned.clone(),
            };
            if let Some(entry) = self.semantic_table.get(&key) {
                return Some(entry.to_match(ResolutionSource::Database));
            }
        }

        let global_key = SemanticKey {
            app_id:  GLOBAL_APP_ID,
            platform,
            role:    role.to_string(),
            context: context_owned,
        };
        self.semantic_table.get(&global_key)
            .map(|e| e.to_match(ResolutionSource::Database))
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn normalize_segment(s: &str) -> String {
    s.trim()
        .trim_end_matches('…')
        .trim_end_matches("...")
        .trim()
        .to_lowercase()
}

fn platform_iter(keys: &schema::PlatformKeys) -> [(Platform, Option<String>); 3] {
    [
        (Platform::Linux,   keys.linux.clone()),
        (Platform::Windows, keys.windows.clone()),
        (Platform::MacOS,   keys.macos.clone()),
    ]
}

impl MenuEntry {
    fn to_match(&self, source: ResolutionSource) -> ShortcutMatch {
        ShortcutMatch {
            shortcut_keys: self.keys.clone(),
            action_name:   self.action_name.clone(),
            description:   self.description.clone(),
            menu_path:     self.menu_path_display.clone(),
            source,
        }
    }
}

impl SemanticEntry {
    fn to_match(&self, source: ResolutionSource) -> ShortcutMatch {
        ShortcutMatch {
            shortcut_keys: self.keys.clone(),
            action_name:   self.role.clone(),
            description:   self.description.clone(),
            menu_path:     String::new(),
            source,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> ShortcutDatabase {
        ShortcutDatabase::build().expect("bundled TOML files must be valid")
    }

    fn identity(exe: &str) -> AppIdentity {
        AppIdentity { executable: exe.to_string(), window_title: None, pid: 1 }
    }

    fn path(segments: &[&str]) -> Vec<String> {
        segments.iter().map(|s| s.to_string()).collect()
    }

    // ── build ────────────────────────────────────────────────────────────────

    #[test]
    fn builds_without_error() {
        db();
    }

    // ── global fallback ──────────────────────────────────────────────────────

    #[test]
    fn global_edit_copy_linux() {
        let m = db().lookup_menu(&identity("unknown-app"), &path(&["Edit", "Copy"]), Platform::Linux);
        assert!(m.is_some());
        assert_eq!(m.unwrap().shortcut_keys, "Ctrl+C");
    }

    #[test]
    fn global_edit_copy_macos() {
        let m = db().lookup_menu(&identity("unknown-app"), &path(&["Edit", "Copy"]), Platform::MacOS);
        assert_eq!(m.unwrap().shortcut_keys, "⌘C");
    }

    #[test]
    fn global_file_save_windows() {
        let m = db().lookup_menu(&identity("notepad"), &path(&["File", "Save"]), Platform::Windows);
        assert_eq!(m.unwrap().shortcut_keys, "Ctrl+S");
    }

    // ── app-specific lookups ─────────────────────────────────────────────────

    #[test]
    fn firefox_new_private_window() {
        let m = db().lookup_menu(
            &identity("firefox"),
            &path(&["File", "New Private Window"]),
            Platform::Linux,
        );
        assert_eq!(m.unwrap().shortcut_keys, "Ctrl+Shift+P");
    }

    #[test]
    fn chromium_incognito_window() {
        let m = db().lookup_menu(
            &identity("chromium"),
            &path(&["File", "New Incognito Window"]),
            Platform::Linux,
        );
        assert_eq!(m.unwrap().shortcut_keys, "Ctrl+Shift+N");
    }

    #[test]
    fn vscode_command_palette() {
        let m = db().lookup_menu(
            &identity("code"),
            &path(&["View", "Command Palette"]),
            Platform::Linux,
        );
        assert_eq!(m.unwrap().shortcut_keys, "Ctrl+Shift+P");
    }

    #[test]
    fn nautilus_new_folder() {
        let m = db().lookup_menu(
            &identity("nautilus"),
            &path(&["File", "New Folder"]),
            Platform::Linux,
        );
        assert_eq!(m.unwrap().shortcut_keys, "Ctrl+Shift+N");
    }

    // ── normalization ────────────────────────────────────────────────────────

    #[test]
    fn matches_case_insensitive_path() {
        let m = db().lookup_menu(&identity("unknown"), &path(&["EDIT", "COPY"]), Platform::Linux);
        assert!(m.is_some());
    }

    #[test]
    fn strips_trailing_ellipsis() {
        let m = db().lookup_menu(&identity("unknown"), &path(&["File", "Save As…"]), Platform::Linux);
        assert!(m.is_some(), "should match 'Save As' after stripping '…'");
    }

    #[test]
    fn strips_trailing_triple_dot() {
        let m = db().lookup_menu(&identity("unknown"), &path(&["File", "Save As..."]), Platform::Linux);
        assert!(m.is_some(), "should match 'Save As' after stripping '...'");
    }

    // ── misses ───────────────────────────────────────────────────────────────

    #[test]
    fn returns_none_for_unknown_path() {
        let m = db().lookup_menu(&identity("firefox"), &path(&["Edit", "Frobnicate"]), Platform::Linux);
        assert!(m.is_none());
    }

    // ── semantic actions ─────────────────────────────────────────────────────

    #[test]
    fn firefox_tab_semantic_action() {
        let m = db().lookup_semantic(
            &identity("firefox"),
            "tab",
            Some("tab-not-selected"),
            Platform::Linux,
        );
        assert_eq!(m.unwrap().shortcut_keys, "Ctrl+Tab");
    }

    #[test]
    fn semantic_returns_none_for_unknown_role() {
        let m = db().lookup_semantic(&identity("firefox"), "slider", None, Platform::Linux);
        assert!(m.is_none());
    }

    // ── resolution source ────────────────────────────────────────────────────

    #[test]
    fn source_is_database() {
        let m = db().lookup_menu(&identity("unknown"), &path(&["Edit", "Copy"]), Platform::Linux);
        assert_eq!(m.unwrap().source, ResolutionSource::Database);
    }
}
