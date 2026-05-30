#![allow(dead_code)]

use serde::Deserialize;

/// Top-level structure of every shortcut TOML file.
#[derive(Debug, Deserialize)]
pub(crate) struct AppFile {
    pub schema_version: u32,
    /// Absent in global (non-app-specific) files.
    pub app: Option<AppDef>,
    #[serde(default)]
    pub shortcuts: Vec<ShortcutDef>,
    #[serde(default)]
    pub semantic_actions: Vec<SemanticActionDef>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AppDef {
    pub id: String,
    pub executables: Vec<String>,
    /// Empty string means "match any window title".
    #[serde(default)]
    pub title_pattern: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ShortcutDef {
    pub menu_path: Vec<String>,
    /// Always exactly one element; array form allows future multi-shortcut entries.
    pub keys: Vec<PlatformKeys>,
    pub description: String,
    pub category: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SemanticActionDef {
    pub role: String,
    pub context: Option<String>,
    pub keys: Vec<PlatformKeys>,
    pub description: String,
    pub category: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PlatformKeys {
    pub linux: Option<String>,
    pub windows: Option<String>,
    pub macos: Option<String>,
}
