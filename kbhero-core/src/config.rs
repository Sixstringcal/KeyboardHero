use std::path::Path;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("I/O error reading config file: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
}

/// Top-level configuration, deserialized from `config.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub display:   DisplayConfig,
    pub behavior:  BehaviorConfig,
    pub autostart: AutostartConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            display:   DisplayConfig::default(),
            behavior:  BehaviorConfig::default(),
            autostart: AutostartConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// How long (ms) the callout stays on screen before fading out. Clamped 1000–10000.
    pub callout_duration_ms: u32,
    /// Callout font size in points. Clamped 10–24.
    pub font_size_pt: u8,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self { callout_duration_ms: 3000, font_size_pt: 13 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BehaviorConfig {
    /// When `true` the daemon is paused and emits no callouts.
    pub paused: bool,
    /// Executable names of apps that should never trigger callouts.
    pub excluded_apps: Vec<String>,
}

impl Default for BehaviorConfig {
    fn default() -> Self {
        Self { paused: false, excluded_apps: Vec::new() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AutostartConfig {
    /// Whether to register a login/autostart entry with the OS.
    pub enabled: bool,
}

impl Default for AutostartConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl Config {
    /// Load from a TOML file, or return defaults if the file does not exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        let mut cfg: Config = toml::from_str(&text)?;
        cfg.validate();
        Ok(cfg)
    }

    /// Clamp all numeric fields to their valid ranges.
    pub fn validate(&mut self) {
        self.display.callout_duration_ms =
            self.display.callout_duration_ms.clamp(1000, 10_000);
        self.display.font_size_pt =
            self.display.font_size_pt.clamp(10, 24);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let mut cfg = Config::default();
        cfg.validate();
        assert_eq!(cfg.display.callout_duration_ms, 3000);
        assert_eq!(cfg.display.font_size_pt, 13);
        assert!(!cfg.behavior.paused);
        assert!(cfg.behavior.excluded_apps.is_empty());
        assert!(cfg.autostart.enabled);
    }

    #[test]
    fn clamps_duration_below_minimum() {
        let mut cfg = Config::default();
        cfg.display.callout_duration_ms = 0;
        cfg.validate();
        assert_eq!(cfg.display.callout_duration_ms, 1000);
    }

    #[test]
    fn clamps_duration_above_maximum() {
        let mut cfg = Config::default();
        cfg.display.callout_duration_ms = 99_999;
        cfg.validate();
        assert_eq!(cfg.display.callout_duration_ms, 10_000);
    }

    #[test]
    fn clamps_font_size_below_minimum() {
        let mut cfg = Config::default();
        cfg.display.font_size_pt = 1;
        cfg.validate();
        assert_eq!(cfg.display.font_size_pt, 10);
    }

    #[test]
    fn clamps_font_size_above_maximum() {
        let mut cfg = Config::default();
        cfg.display.font_size_pt = 255;
        cfg.validate();
        assert_eq!(cfg.display.font_size_pt, 24);
    }

    #[test]
    fn parses_full_toml() {
        let toml = r#"
            [display]
            callout_duration_ms = 5000
            font_size_pt = 16

            [behavior]
            paused = true
            excluded_apps = ["blender", "kdenlive"]

            [autostart]
            enabled = false
        "#;
        let mut cfg: Config = toml::from_str(toml).unwrap();
        cfg.validate();
        assert_eq!(cfg.display.callout_duration_ms, 5000);
        assert_eq!(cfg.display.font_size_pt, 16);
        assert!(cfg.behavior.paused);
        assert_eq!(cfg.behavior.excluded_apps, ["blender", "kdenlive"]);
        assert!(!cfg.autostart.enabled);
    }

    #[test]
    fn parses_partial_toml_uses_defaults_for_missing_fields() {
        let toml = r#"
            [display]
            font_size_pt = 18
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.display.callout_duration_ms, 3000);
        assert_eq!(cfg.display.font_size_pt, 18);
        assert!(!cfg.behavior.paused);
    }

    #[test]
    fn empty_toml_uses_all_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.display.callout_duration_ms, 3000);
        assert_eq!(cfg.display.font_size_pt, 13);
    }
}
