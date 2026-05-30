use std::future::Future;
use std::time::Instant;
use thiserror::Error;

/// Target platform, used when resolving shortcuts from the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Linux,
    Windows,
    MacOS,
}

impl Platform {
    /// Returns the platform this binary was compiled for.
    pub fn current() -> Self {
        #[cfg(target_os = "linux")]
        return Platform::Linux;
        #[cfg(target_os = "windows")]
        return Platform::Windows;
        #[cfg(target_os = "macos")]
        return Platform::MacOS;
        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        panic!("unsupported platform");
    }
}

/// Identifies the foreground application at event time.
#[derive(Debug, Clone)]
pub struct AppIdentity {
    /// Process executable name, e.g. "firefox" or "code.exe".
    pub executable: String,
    /// Window title, used for title-pattern disambiguation.
    pub window_title: Option<String>,
    /// Process ID — used only for deduplication, not for matching.
    pub pid: u32,
}

/// The role of a UI element as reported by the accessibility API.
#[derive(Debug, Clone)]
pub enum ElementRole {
    MenuItem,
    ToolbarButton,
    ContextMenuItem,
    /// A non-menu element whose keyboard equivalent must be inferred from
    /// role + context rather than read from an accessibility attribute.
    SemanticElement {
        /// Accessibility role string, e.g. "tab", "button".
        role: String,
        /// Optional context that disambiguates the action, e.g. "tab-not-selected".
        /// Populated by the platform implementation when determinable cheaply.
        context: Option<String>,
    },
}

/// A raw activation event from the OS accessibility layer.
#[derive(Debug, Clone)]
pub struct RawActivationEvent {
    /// Wall-clock time of the underlying OS event.
    pub timestamp: Instant,
    /// The foreground application at event time.
    pub app: AppIdentity,
    /// The activated element's role.
    pub role: ElementRole,
    /// Label path from menu root to the activated item, e.g. ["Edit", "Copy"].
    pub label_path: Vec<String>,
    /// Shortcut string read directly from the accessibility API (Tier 1).
    /// `None` means the API did not advertise one; resolver falls back to Tier 2.
    pub discovered_shortcut: Option<String>,
}

/// Records whether a `ShortcutMatch` came from Tier 1 (API) or Tier 2 (database).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionSource {
    /// Read directly from the accessibility API attribute on the element.
    Dynamic,
    /// Looked up in the bundled TOML shortcut database.
    Database,
}

/// A resolved shortcut, ready to be displayed by the renderer.
#[derive(Debug, Clone)]
pub struct ShortcutMatch {
    /// OS-formatted shortcut string, e.g. "Ctrl + C".
    pub shortcut_keys: String,
    /// The label of the activated action, e.g. "Copy".
    pub action_name: String,
    /// Full action description, e.g. "Copy selection to clipboard".
    pub description: String,
    /// Display-form menu path, e.g. "Edit › Copy".
    pub menu_path: String,
    /// Whether this came from Tier 1 or Tier 2.
    pub source: ResolutionSource,
}

/// Contract all platform detection backends must satisfy.
pub trait DetectionEngine: Send + 'static {
    /// Async-yields the next activation event.
    /// Returns `None` when the engine is shutting down.
    fn next_event(&mut self) -> impl Future<Output = Option<RawActivationEvent>> + Send;
}

/// Top-level errors that warrant a tray-icon warning or process exit.
#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("platform accessibility API unavailable: {0}")]
    PlatformUnavailable(String),
    #[error("config file invalid: {0}")]
    ConfigInvalid(#[from] crate::config::ConfigError),
    #[error("shortcut database failed to load: {0}")]
    DatabaseError(#[from] crate::db::DatabaseError),
    #[error("another instance is already running")]
    SingleInstanceConflict,
    #[error("shutdown requested")]
    Shutdown,
}
