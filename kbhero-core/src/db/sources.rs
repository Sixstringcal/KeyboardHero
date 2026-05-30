/// All shortcut TOML files bundled into the binary at compile time.
/// Tuple: (source name for error messages, TOML content).
pub(crate) const SOURCES: &[(&str, &str)] = &[
    (
        "global/common",
        include_str!("../../../shortcuts/global/common.toml"),
    ),
    (
        "apps/firefox",
        include_str!("../../../shortcuts/apps/firefox.toml"),
    ),
    (
        "apps/chromium",
        include_str!("../../../shortcuts/apps/chromium.toml"),
    ),
    (
        "apps/vscode",
        include_str!("../../../shortcuts/apps/vscode.toml"),
    ),
    (
        "apps/gnome-text-editor",
        include_str!("../../../shortcuts/apps/gnome-text-editor.toml"),
    ),
    (
        "apps/nautilus",
        include_str!("../../../shortcuts/apps/nautilus.toml"),
    ),
];
