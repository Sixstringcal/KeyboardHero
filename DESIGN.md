# Design Document: KeyboardHero

**Author:** Calvin Nielson  
**Status:** Approved

---

## Table of Contents

1. [Overview](#1-overview)
2. [Background and Motivation](#2-background-and-motivation)
3. [System Architecture](#3-system-architecture)
4. [Component Design](#4-component-design)
   - 4.1 [Detection Engine](#41-detection-engine)
   - 4.2 [Shortcut Database](#42-shortcut-database)
   - 4.3 [Shortcut Resolver](#43-shortcut-resolver)
   - 4.4 [Callout Renderer](#44-callout-renderer)
   - 4.5 [Daemon Lifecycle Manager](#45-daemon-lifecycle-manager)
   - 4.6 [Configuration Manager](#46-configuration-manager)
5. [Platform-Specific Design](#5-platform-specific-design)
   - 5.1 [Linux — AT-SPI2](#51-linux--at-spi2)
   - 5.2 [Linux — Overlay Rendering](#52-linux--overlay-rendering)
   - 5.3 [Windows — UIAutomation + WinEventHook](#53-windows--uiautomation--wineventhook)
   - 5.4 [macOS — AXObserver](#54-macos--axobserver)
6. [Data Model](#6-data-model)
7. [Event Flow](#7-event-flow)
8. [Concurrency Model](#8-concurrency-model)
9. [Error Handling Strategy](#9-error-handling-strategy)
10. [Testing Strategy](#10-testing-strategy)
11. [Performance Analysis](#11-performance-analysis)
12. [Security Considerations](#12-security-considerations)
13. [Packaging and Distribution](#13-packaging-and-distribution)
14. [Open Questions and Risks](#14-open-questions-and-risks)
15. [Alternatives Considered](#15-alternatives-considered)
16. [Implementation Roadmap](#16-implementation-roadmap)

---

## TL;DR

**What it is:** A Rust background daemon that watches OS accessibility events and pops up a transient overlay showing the keyboard shortcut for whatever the user just did with the mouse.

**Language & process model:** Single Rust process, single tokio runtime (2 worker threads), all components communicating via bounded in-process `mpsc` channels. No IPC sockets, no subprocesses.

**Event detection — per platform:**
- Linux: AT-SPI2 over D-Bus (`Object:Activated` / `StateChanged:selected`)
- Windows: `SetWinEventHook` (`EVENT_OBJECT_INVOKED`) + MSAA / UIAutomation
- macOS: `AXObserver` (`kAXMenuItemSelectedNotification`, `kAXPressedNotification`)

**Shortcut resolution — two tiers + semantic fallback:**
1. **Tier 1 (dynamic):** Read the shortcut directly from the accessibility API attribute on the activated element. Works for any standard-toolkit menu item with zero database involvement.
2. **Tier 2 (database):** Fall back to a TOML shortcut database bundled into the binary at compile time (`include_bytes!`), indexed as a `HashMap` for O(1) lookup.
3. **Semantic actions:** For non-menu elements (tabs, nav buttons) where neither tier applies, match on element `role` + `context` string declared in `[[semantic_actions]]` TOML entries.

**Shortcut database:** TOML files under `shortcuts/`, parsed and indexed at startup into a flat `HashMap<(AppId, Platform, MenuPath), ShortcutEntry>`. `AppMatcher` maps executable name + optional title regex → `AppId`. Global fallback table covers universal shortcuts (Ctrl+Z, Ctrl+C, etc.) for any app.

**Overlay rendering — per platform:**
- Linux: GTK4 (`gtk4-rs`), `DrawingArea` + Cairo/Pango; X11 via override-redirect, Wayland via `gtk4-layer-shell` (KDE) or programmatic positioning (GNOME)
- Windows: `WS_EX_LAYERED | WS_EX_TRANSPARENT` Win32 window, Direct2D + DirectWrite
- macOS: `NSPanel` at `NSPopUpMenuWindowLevel`, Core Graphics + `NSAttributedString`

**Animation:** HIDDEN → FADE\_IN → VISIBLE → FADE\_OUT state machine, ≤ 150ms fade, 3s default auto-dismiss. New event while visible resets timer and replaces content immediately — no queuing.

**Configuration:** TOML file at platform-standard paths, live-reloaded via `notify` file watcher (500ms debounce). `Arc<RwLock<Config>>` shared across tasks.

**Resource targets:** < 1% CPU steady-state, < 50 MB RSS, < 500ms P99 callout latency (resolver alone is ~1μs).

**Key risks:** GNOME Wayland overlay positioning (no `wlr-layer-shell`; GNOME Shell extension is likely V1 path), AT-SPI2 disabled by default on some distros, macOS Accessibility permission UX friction.

---

## 1. Overview

KeyboardHero is a cross-platform background daemon written in **Rust** that monitors OS accessibility events, resolves the keyboard shortcut for whatever the user just did with the mouse, and displays a transient overlay callout with that shortcut.

Shortcut resolution uses a **two-tier strategy**. Tier 1 reads the shortcut directly from the accessibility API at event time — most standard menu items already advertise their shortcut this way, giving us universal coverage with no database required. Tier 2 falls back to a bundled TOML shortcut database for apps and elements that don't expose shortcut data through the API. A third detection mode, **semantic actions**, handles non-menu UI elements (tabs, navigation buttons, scroll controls) whose keyboard equivalents cannot be read from an accessibility attribute but can be inferred from the element's role and context.

This document describes the internal architecture, the platform-specific mechanisms, the data model, and the concurrency strategy that make those guarantees possible within the resource envelope defined in the PRD (< 1% CPU, < 50 MB RSS, < 500ms P99 callout latency).

### Why Rust

| Criterion | Rust | Go | Python |
|-----------|------|----|--------|
| Sustained CPU overhead | Minimal — zero-cost abstractions | Low — GC pauses possible | High — interpreter overhead |
| RSS footprint | Minimal — no runtime | Moderate — goroutine stacks, GC | High — CPython overhead |
| Safe FFI to C accessibility APIs | Yes — `bindgen`, thin wrappers | Yes — `cgo`, heavier ABI | Yes — `ctypes`, fragile |
| OS-native GUI (overlay) | Via gtk4-rs, windows-rs, objc2 | Via CGo or no first-class bindings | Via PyGObject, tkinter — limited |
| Single static binary | Yes | Yes | No |
| Memory safety without GC | Yes | No | No |

Rust gives us the lowest runtime footprint, deterministic memory management, and ergonomic bindings to each platform's native accessibility and GUI frameworks.

---

## 2. Background and Motivation

The core challenge is **OS-level event interception without root privilege**. Each platform exposes a different accessibility API:

- **Linux**: AT-SPI2 (Assistive Technology Service Provider Interface, v2) over D-Bus.
- **Windows**: UIAutomation (COM-based) + `SetWinEventHook` for real-time event streaming.
- **macOS**: AXObserver (CoreFoundation-based) + `CGEventTap` for low-level event correlation.

These APIs are designed for screen readers, but they provide exactly what we need: which UI element was activated, its role (menu item, button, tab, etc.), its text label, and — critically — the keyboard shortcut the application itself has registered for that element. The accessibility API is the same channel through which apps expose shortcut text in their own menus, so reading it back is not a hack; it is the intended mechanism.

---

## 3. System Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        keyboardhero daemon                           │
│                                                                       │
│  ┌──────────────┐    HintEvent    ┌──────────────┐                   │
│  │  Detection   │ ─────────────► │   Shortcut   │                   │
│  │   Engine     │                │   Resolver   │                   │
│  │  (platform)  │                │              │                   │
│  └──────────────┘                └──────┬───────┘                   │
│                                         │ ShortcutMatch             │
│  ┌──────────────┐                ┌──────▼───────┐                   │
│  │  Daemon      │                │   Callout    │                   │
│  │  Lifecycle   │◄───────────────│   Renderer   │                   │
│  │  Manager     │  pause/resume  │  (platform)  │                   │
│  └──────┬───────┘                └──────────────┘                   │
│         │                                                             │
│  ┌──────▼───────┐                ┌──────────────┐                   │
│  │ Config       │                │  Shortcut    │                   │
│  │ Manager      │◄───────────────│  Database    │                   │
│  └──────────────┘  read-only     │  (TOML)      │                   │
│                                  └──────────────┘                   │
└─────────────────────────────────────────────────────────────────────┘
         │ tray icon / signals
         ▼
  OS system tray / status area
```

All components live in the same process. Inter-component communication uses **in-process channels** (Rust `tokio::sync::mpsc`), not sockets or shared memory. This keeps latency minimal and eliminates any IPC attack surface.

---

## 4. Component Design

### 4.1 Detection Engine

The Detection Engine is a thin platform abstraction layer. Its sole responsibility is to translate raw OS accessibility events into `RawActivationEvent` values and push them onto a channel. It knows nothing about shortcuts — but it does attempt to read the shortcut the application has registered for an activated element, because every platform's accessibility API exposes this at no extra cost.

```rust
/// A raw activation event from the OS accessibility layer.
pub struct RawActivationEvent {
    /// Wall-clock time of the underlying OS event.
    pub timestamp: Instant,
    /// The foreground application at event time.
    pub app: AppIdentity,
    /// The activated element's role as reported by the accessibility API.
    pub role: ElementRole,
    /// The label(s) of the element and its ancestors, forming a path.
    /// Example: ["Edit", "Copy"] or ["File", "Save As…"]
    pub label_path: Vec<String>,
    /// The keyboard shortcut string read directly from the accessibility API,
    /// if the element exposed one (Tier 1 resolution). `None` means the API
    /// did not advertise a shortcut; the resolver will attempt Tier 2 lookup.
    pub discovered_shortcut: Option<String>,
}

pub enum ElementRole {
    MenuItem,
    ToolbarButton,
    ContextMenuItem,
    /// A non-menu element whose keyboard equivalent must be inferred from
    /// its role and context rather than read from an accessibility attribute.
    SemanticElement {
        /// The element's accessibility role string (e.g., "tab", "button").
        role: String,
        /// Optional context that disambiguates the action within that role
        /// (e.g., "tab-unselected", "navigation-back"). Populated by the
        /// platform implementation when context can be determined cheaply.
        context: Option<String>,
    },
}

pub struct AppIdentity {
    /// The process executable name (e.g., "firefox", "code.exe").
    pub executable: String,
    /// The window title, if available. Used for disambiguation.
    pub window_title: Option<String>,
    /// The process ID, used only for deduplication.
    pub pid: u32,
}
```

The Detection Engine exposes a single trait:

```rust
pub trait DetectionEngine: Send + 'static {
    /// Block (or async-await) until the next activation event.
    /// Returns `None` when the engine is shutting down.
    fn next_event(&mut self) -> impl Future<Output = Option<RawActivationEvent>>;
}
```

Platform implementations live in `src/platform/{linux,windows,macos}/detection.rs`. The main event loop calls `next_event()` and immediately sends the result on the channel without doing any matching — keeping the detection hot-path minimal.

#### Shortcut extraction in the hot path

Each platform implementation reads the shortcut attribute immediately after resolving the activated element, before emitting the event. This is a single attribute fetch on an already-resolved object — it adds < 0.1ms to the hot path and eliminates the need for any database round-trip for the vast majority of menu items.

#### Why a single trait, not an enum dispatch

Encoding platform behavior as an enum with a match arm on every method would leak platform-specific fields across the codebase. A trait bound keeps each platform's event model entirely private to its own module.

### 4.2 Shortcut Database

The database is a Tier 2 fallback and semantic action registry. It handles two cases Tier 1 dynamic discovery cannot: apps or toolkits that don't advertise shortcuts via accessibility attributes, and non-menu UI elements (tabs, navigation buttons) whose keyboard equivalents must be explicitly declared.

The database is a collection of TOML files bundled into the binary at compile time via `include_bytes!`. This means no file-system dependency at runtime and zero disk reads during steady-state operation (satisfying FR-13 and NFR disk-write constraint).

#### File layout

```
shortcuts/
  _schema.toml          # schema version declaration
  global/
    common.toml         # shortcuts valid for almost any app (e.g., Ctrl+Z)
  apps/
    firefox.toml
    chromium.toml
    vscode.toml
    gnome-text-editor.toml
    libreoffice-writer.toml
    nautilus.toml
    terminal.toml       # covers gnome-terminal, kitty, alacritty where menus exist
    ...                 # ≥ 15 apps at launch (see FR-10)
```

#### TOML schema

```toml
# shortcuts/apps/firefox.toml

schema_version = 1

[app]
id          = "firefox"
executables = ["firefox", "firefox-esr", "firefox-bin"]
title_pattern = ""   # empty = match any window title

[[shortcuts]]
menu_path   = ["Edit", "Copy"]
keys        = [{ linux = "Ctrl+C", windows = "Ctrl+C", macos = "⌘C" }]
description = "Copy selection to clipboard"
category    = "editing"

[[shortcuts]]
menu_path   = ["Edit", "Paste"]
keys        = [{ linux = "Ctrl+V", windows = "Ctrl+V", macos = "⌘V" }]
description = "Paste from clipboard"
category    = "editing"

[[shortcuts]]
menu_path   = ["File", "New Window"]
keys        = [{ linux = "Ctrl+N", windows = "Ctrl+N", macos = "⌘N" }]
description = "Open a new browser window"
category    = "navigation"

# Semantic actions: non-menu elements whose keyboard equivalent is inferred
# from role + context, not read from an accessibility attribute.
[[semantic_actions]]
role        = "tab"
context     = "tab-not-selected"
keys        = [{ linux = "Ctrl+Tab", windows = "Ctrl+Tab", macos = "⌃Tab" }]
description = "Switch to next/previous tab"
category    = "navigation"

[[semantic_actions]]
role        = "button"
context     = "navigation-back"
keys        = [{ linux = "Alt+Left", windows = "Alt+Left", macos = "⌘[" }]
description = "Navigate back"
category    = "navigation"

[[semantic_actions]]
role        = "button"
context     = "navigation-forward"
keys        = [{ linux = "Alt+Right", windows = "Alt+Right", macos = "⌘]" }]
description = "Navigate forward"
category    = "navigation"
```

#### In-memory representation

At startup, all TOML files are parsed and indexed into a flat HashMap for O(1) lookup:

```rust
/// Composite key for the shortcut lookup table.
#[derive(Hash, PartialEq, Eq)]
struct ShortcutKey {
    app_id:    AppId,       // interned string index
    platform:  Platform,
    menu_path: MenuPath,    // Vec<InternedStr> stored as a SmallVec<[u32; 4]>
}

pub struct ShortcutDatabase {
    table:    HashMap<ShortcutKey, ShortcutEntry>,
    app_map:  AppMatcher,   // maps (executable, title_pattern) → AppId
    interner: StringInterner,
}
```

`MenuPath` comparison is case-insensitive and strips trailing ellipsis characters (`…`, `...`) to handle platform-specific label formatting inconsistencies.

#### AppMatcher

```rust
pub struct AppMatcher {
    /// Ordered list; first match wins.
    entries: Vec<AppMatchEntry>,
}

struct AppMatchEntry {
    app_id:        AppId,
    executables:   Vec<String>,      // exact match, case-insensitive
    title_pattern: Option<Regex>,    // None = match any title
}

impl AppMatcher {
    pub fn resolve(&self, identity: &AppIdentity) -> Option<AppId> {
        self.entries.iter().find_map(|entry| {
            let exe_matches = entry.executables.iter()
                .any(|e| e.eq_ignore_ascii_case(&identity.executable));
            if !exe_matches {
                return None;
            }
            match &entry.title_pattern {
                Some(re) => identity.window_title.as_deref()
                    .filter(|t| re.is_match(t))
                    .map(|_| entry.app_id),
                None => Some(entry.app_id),
            }
        })
    }
}
```

#### Fallback resolution

If no app-specific entry matches, the resolver falls back to the `global/common.toml` table. This ensures universal shortcuts (Ctrl+Z, Ctrl+C, Ctrl+S) are covered for any application, even those not explicitly listed.

### 4.3 Shortcut Resolver

The Shortcut Resolver is an async task that consumes `RawActivationEvent` values and emits `ShortcutMatch` values. It owns the two-tier resolution strategy.

```rust
pub struct ShortcutResolver {
    db:     Arc<ShortcutDatabase>,
    config: Arc<RwLock<Config>>,
}

impl ShortcutResolver {
    pub async fn run(
        mut self,
        mut rx: mpsc::Receiver<RawActivationEvent>,
        tx:     mpsc::Sender<ShortcutMatch>,
    ) {
        while let Some(event) = rx.recv().await {
            if let Some(m) = self.resolve(event) {
                let _ = tx.send(m).await;
            }
        }
    }

    fn resolve(&self, event: RawActivationEvent) -> Option<ShortcutMatch> {
        let config = self.config.read();

        // Drop events for excluded apps immediately.
        if config.excluded_apps.iter().any(|e| e.eq_ignore_ascii_case(&event.app.executable)) {
            return None;
        }

        match &event.role {
            ElementRole::MenuItem | ElementRole::ToolbarButton | ElementRole::ContextMenuItem => {
                // Tier 1: use the shortcut the app already advertised via the
                // accessibility API, if it exposed one.
                if let Some(raw) = &event.discovered_shortcut {
                    return Some(ShortcutMatch {
                        shortcut_keys: normalize_shortcut(raw),
                        action_name:   event.label_path.last().cloned().unwrap_or_default(),
                        description:   String::new(),   // not available from API; shown empty
                        menu_path:     format_path(&event.label_path),
                        source:        ResolutionSource::Dynamic,
                    });
                }
                // Tier 2: fall back to the TOML database.
                self.db.lookup_menu(&event.app, &event.label_path)
            }

            ElementRole::SemanticElement { role, context } => {
                // Semantic actions: look up role + context in the database.
                // There is no Tier 1 path here — the API has no shortcut
                // attribute for these elements.
                self.db.lookup_semantic(&event.app, role, context.as_deref())
            }
        }
    }
}
```

#### Resolution source tracking

`ShortcutMatch` carries a `ResolutionSource` tag (`Dynamic` vs `Database`) so that telemetry and tests can measure the Tier 1 hit rate and Tier 2 fallback rate independently. This is not displayed to the user.

#### Semantic context population

The Detection Engine populates `SemanticElement.context` by inspecting cheap, already-resolved attributes of the activated element — for example, comparing `selected` state to determine whether a tab is the active one. Context strings are defined in the TOML `[[semantic_actions]]` schema; the platform code uses the same strings so they match at lookup time.

### 4.4 Callout Renderer

The Callout Renderer manages a single overlay window. It receives `ShortcutMatch` events and handles all animation state internally. It never blocks the event pipeline.

```rust
pub struct ShortcutMatch {
    pub shortcut_keys: String,    // OS-formatted, e.g. "Ctrl + C"
    pub action_name:   String,    // e.g. "Copy"
    pub description:   String,    // e.g. "Copy selection to clipboard"
    pub menu_path:     String,    // e.g. "Edit › Copy"
}
```

#### Window properties (all platforms)

| Property | Value |
|----------|-------|
| Always-on-top | Yes |
| Focusable | No — must never receive focus |
| Mouse passthrough | Yes — `WS_EX_TRANSPARENT` on Windows; `input-passthrough` on GTK4; `ignoresMouseEvents = YES` on macOS |
| Opacity | Animated 0 → 1 → 0 |
| Position | Bottom-center, 16px above taskbar |
| Max width | 400px logical pixels |

#### Animation state machine

```
           ┌──────────────────────────────────────────────────────┐
           │                                                        │
           ▼                                                        │
        HIDDEN ──show()──► FADE_IN ──complete──► VISIBLE ──timer──► FADE_OUT
           ▲                                                        │
           └────────────────────complete──────────────────────────┘

        Any state: new show() event ──► reset timer, re-FADE_IN if needed
```

The animation duration is ≤ 150ms (PRD FR-23). The auto-dismiss timer is configurable (default 3s). A new event arriving while the callout is visible resets the timer and replaces the displayed content immediately — there is no queuing of hint events.

#### Rendering approach

Keycap badges are rendered as filled rounded rectangles with a 1px border, using the OS font metrics for sizing. The color palette follows the OS light/dark mode:

```
Light mode:  background #FFFFFF, border #D0D0D0, keycap fill #F0F0F0, text #1A1A1A
Dark mode:   background #2C2C2E, border #48484A, keycap fill #3A3A3C, text #F2F2F7
```

These values are derived from the OS system palette at window-creation time and re-queried on theme-change signals.

### 4.5 Daemon Lifecycle Manager

The Lifecycle Manager owns the process entry point and coordinates startup, shutdown, and the system tray.

```
main()
  │
  ├── parse config (ConfigManager::load)
  ├── load shortcut database (ShortcutDatabase::build)
  ├── spawn DetectionEngine (platform-specific)
  ├── spawn ShortcutResolver task
  ├── spawn CalloutRenderer (on GUI thread)
  ├── register system tray icon + menu
  ├── install signal handler (SIGTERM, SIGINT)
  └── run event loop until shutdown signal
```

#### Single-instance enforcement

On Linux/macOS: a lock file at `$XDG_RUNTIME_DIR/keyboardhero.lock` (Linux) or `$TMPDIR/keyboardhero.lock` (macOS), locked with `flock(2)`. If the lock is held, the new instance prints a message and exits 1.

On Windows: a named kernel mutex (`Global\KeyboardHeroSingleInstance`). `CreateMutex` returns `ERROR_ALREADY_EXISTS` if another instance is running.

#### System tray menu

```
KeyboardHero  ●  (green dot = running)
─────────────────
  Pause
  Settings…
─────────────────
  Quit
```

"Pause" toggles to "Resume" and sends a `Command::Pause` / `Command::Resume` on the control channel. The Detection Engine checks this flag before emitting events.

### 4.6 Configuration Manager

Configuration lives at:

| Platform | Path |
|----------|------|
| Linux | `$XDG_CONFIG_HOME/keyboardhero/config.toml` |
| macOS | `$HOME/Library/Application Support/keyboardhero/config.toml` |
| Windows | `%APPDATA%\keyboardhero\config.toml` |

#### Schema

```toml
# config.toml — full example with defaults

[display]
callout_duration_ms = 3000   # 1000–10000
font_size_pt        = 13     # 10–24

[behavior]
paused              = false
excluded_apps       = []     # e.g. ["kdenlive", "blender"]

[autostart]
enabled             = true   # manages the OS autostart entry
```

`ConfigManager` wraps the TOML file with a `notify`-based file watcher so settings changes take effect immediately without restarting the daemon. Configuration writes go through a debounce (500ms) to avoid write storms during rapid slider adjustments in the settings UI.

---

## 5. Platform-Specific Design

### 5.1 Linux — AT-SPI2

AT-SPI2 is the standard Linux accessibility bus. It sits on top of D-Bus (the session bus) and exposes a tree of `Accessible` objects for every running application's UI.

#### Event subscription

```
D-Bus session bus
  └── org.a11y.Bus               ← AT-SPI2 bus address endpoint
        └── /org/a11y/bus        ← GetAddress() → AT-SPI2 socket path
              └── org.a11y.atspi.Event.Object:StateChanged:focused
              └── org.a11y.atspi.Event.Object:Activated         ← menu items
              └── org.a11y.atspi.Event.Window:Activate          ← foreground app
```

We subscribe to `Object:StateChanged:selected` and `Object:Activated` on the AT-SPI2 bus using the `atspi` Rust crate (wraps `libatspi`/D-Bus directly). The event payload includes:

- The sender object path (which encodes the app's D-Bus name and accessible object ID).
- The event role (filtered to `ROLE_MENU_ITEM`, `ROLE_PUSH_BUTTON` within toolbars).
- The accessible name (the label text, e.g., "Copy").

To reconstruct the full `label_path`, we walk the `parent` relation of the activated element up to the root, collecting names. This walk is synchronous and bounded (menu hierarchies are rarely deeper than 4 levels), so it adds < 1ms of latency.

#### AT-SPI2 availability check

Some distributions ship with AT-SPI2 disabled by default. At startup:

```rust
fn check_atspi_available() -> Result<(), DaemonError> {
    // Query org.a11y.Status.IsEnabled via D-Bus.
    // If false, emit a first-run guidance notification and attempt
    // to enable via org.a11y.Status.IsEnabled setter (GNOME supports this).
    // If the setter fails, surface a user-readable error in the tray menu.
}
```

#### Wayland overlay window

GNOME Wayland does not implement `wlr-layer-shell`, so we cannot use `zwlr_layer_shell_v1` to anchor a window to the screen edge. Instead we use a **GTK4 window** with:

```
gtk4::Window {
  decorated: false,
  resizable: false,
  can-focus: false,
  app-paintable: true          // for RGBA transparency
}
```

We set `_NET_WM_WINDOW_TYPE_NOTIFICATION` via the X11 protocol through XWayland if running under XWayland, or request a `gtk_layer_shell` surface type if the compositor supports it. On pure GNOME Wayland without layer-shell, we fall back to a `GDK_WINDOW_TYPE_HINT_SPLASHSCREEN` window, positioned programmatically. This is the primary Wayland engineering risk (see §14).

For KDE Plasma (which implements `wlr-layer-shell`), we use `gtk4-layer-shell` bindings to anchor the window at `ZWLR_LAYER_SHELL_V1_LAYER_OVERLAY`, anchored to the bottom edge, horizontally centered.

#### X11 overlay

On X11 (and XWayland), the overlay is an `override-redirect` window with `_NET_WM_STATE_ABOVE` and `_NET_WM_WINDOW_TYPE_NOTIFICATION`. It is created as a compositing-aware RGBA window so the rounded rectangle background renders correctly with a compositor running.

### 5.2 Linux — Overlay Rendering

We use **GTK4** (via `gtk4-rs`) as the primary rendering toolkit on Linux for both Wayland and X11. GTK4's `DrawingArea` widget gives us full Cairo/Pango access for custom keycap badge rendering and precise text layout.

The GTK4 main loop runs on a dedicated thread (the "GUI thread"). All other components communicate with it through `glib::MainContext::channel`, which is the GTK-safe cross-thread message-passing primitive.

### 5.3 Windows — UIAutomation + WinEventHook

#### Event capture

Windows UIAutomation provides `IUIAutomation::AddAutomationEventHandler` for `UIA_Invoke_InvokedEventId` on menu items. However, subscription must happen per-element, which is impractical system-wide.

Instead, we use `SetWinEventHook` with:

```
EVENT_SYSTEM_MENUPOPUPSTART    // a menu opened
EVENT_OBJECT_INVOKED           // a menu item or button was invoked
EVENT_SYSTEM_MENUEND           // menu dismissed
```

These hooks run on a dedicated message-pump thread. When `EVENT_OBJECT_INVOKED` fires, we use `AccessibleObjectFromEvent` (MSAA) to retrieve the `IAccessible`, then `get_accName` and `get_accRole` to extract the label and role. For UIAutomation-native apps (WPF, WinUI 3), we additionally query `IUIAutomationElement` for richer role information.

The parent walk uses `IAccessible::get_accParent` (MSAA) or `IUIAutomationElement::GetCachedParent` (UIA).

#### Overlay window

```
CreateWindowEx(
  WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
  class_name,
  window_name,
  WS_POPUP,
  x, y, width, height,
  NULL, NULL, hinstance, NULL
)
```

`WS_EX_TRANSPARENT` ensures all mouse events pass through. `WS_EX_NOACTIVATE` prevents focus steal. Transparency is managed via `SetLayeredWindowAttributes` (per-window alpha) for the fade animation, updated by a `WM_TIMER` message at 60fps.

We use **Direct2D** (`windows-rs` bindings) for rendering the callout content: rounded rectangles for keycap badges, `IDWriteTextLayout` for text with system font fallback. This gives us HiDPI-correct rendering (DPI-aware window with `SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`).

### 5.4 macOS — AXObserver

#### Event capture

```rust
// Pseudocode; actual implementation uses objc2 / core-foundation crates.

// For each running application:
let observer = AXObserver::new(pid, callback)?;
observer.add_notification(kAXMenuItemSelectedNotification);
observer.add_notification(kAXPressedNotification);  // toolbar buttons
observer.schedule_on_run_loop(CFRunLoop::main(), kCFRunLoopDefaultMode);
```

`AXObserverAddNotification` is per-process, so we watch `NSWorkspace.sharedWorkspace().notificationCenter` for `NSWorkspaceDidActivateApplicationNotification` to track newly launched or focused applications and attach an observer dynamically.

When a menu item fires `kAXMenuItemSelectedNotification`, the callback receives an `AXUIElementRef`. We walk `kAXParentAttribute` to reconstruct the full menu path, then emit a `RawActivationEvent`.

#### Accessibility permission

macOS requires the user to grant "Accessibility" permission in System Settings > Privacy & Security > Accessibility. If permission is not granted, `AXIsProcessTrusted()` returns false. We handle this in first-run onboarding:

1. Call `AXIsProcessTrustedWithOptions({kAXTrustedCheckOptionPrompt: true})` to trigger the system prompt.
2. Poll `AXIsProcessTrusted()` at 1s intervals (on a background thread) until granted, then start the detection engine.
3. Show a persistent tray icon with "⚠ Accessibility permission required" until the permission is granted.

#### Overlay window

```objc
// Objective-C pseudocode; Rust implementation uses objc2 crate.

NSPanel *panel = [[NSPanel alloc]
    initWithContentRect: frame
    styleMask: NSWindowStyleMaskNonactivatingPanel
    backing: NSBackingStoreBuffered
    defer: NO];

[panel setLevel: NSPopUpMenuWindowLevel];
[panel setIgnoresMouseEvents: YES];
[panel setCollectionBehavior:
    NSWindowCollectionBehaviorCanJoinAllSpaces |
    NSWindowCollectionBehaviorStationary |
    NSWindowCollectionBehaviorIgnoresCycle];
[panel setOpaque: NO];
[panel setBackgroundColor: [NSColor clearColor]];
[panel setHasShadow: NO];
```

`NSPopUpMenuWindowLevel` places the window above all normal application windows but below the system menu bar. `NSWindowCollectionBehaviorStationary` keeps it visible during Exposé/Mission Control without it being a managed window.

Rendering uses **Core Graphics** (`objc2-app-kit` / `CGContext`) for the rounded rectangle and keycap badges, and `NSAttributedString` with `NSLayoutManager` for text. The window's `contentView` is a custom `CALayer`-backed `NSView` subclass.

---

## 6. Data Model

```
RawActivationEvent
  ├── timestamp:          Instant
  ├── app:                AppIdentity
  │     ├── executable:       String
  │     ├── window_title:     Option<String>
  │     └── pid:              u32
  ├── role:               ElementRole
  │     ├── MenuItem
  │     ├── ToolbarButton
  │     ├── ContextMenuItem
  │     └── SemanticElement
  │           ├── role:         String   ← e.g. "tab", "button"
  │           └── context:      Option<String>   ← e.g. "tab-not-selected"
  ├── label_path:         Vec<String>
  └── discovered_shortcut: Option<String>   ← Tier 1: from accessibility API; None → fall back to Tier 2

ShortcutEntry                         ← persisted in TOML; indexed at startup
  ├── app_id:      AppId
  ├── platform:    Platform { Linux | Windows | MacOS }
  ├── menu_path:   Vec<String>
  ├── keys:        String             ← pre-formatted, e.g. "Ctrl + C"
  ├── description: String
  └── category:    String

SemanticActionEntry                   ← persisted in TOML; indexed at startup
  ├── app_id:   AppId
  ├── role:     String
  ├── context:  Option<String>
  ├── keys:     String
  ├── description: String
  └── category: String

ShortcutMatch                         ← emitted to Renderer
  ├── shortcut_keys: String
  ├── action_name:   String
  ├── description:   String
  ├── menu_path:     String           ← display form, e.g. "Edit › Copy"
  └── source:        ResolutionSource { Dynamic | Database }

Config                                ← loaded from config.toml
  ├── callout_duration_ms: u32        (1000–10000)
  ├── font_size_pt:        u8         (10–24)
  ├── paused:              bool
  ├── excluded_apps:       Vec<String>
  └── autostart_enabled:   bool
```

---

## 7. Event Flow

```
OS Accessibility API
        │
        │  platform callback (OS thread)
        ▼
  DetectionEngine::poll_next()
        │
        │  RawActivationEvent
        │  (tokio::sync::mpsc::Sender)
        ▼
  ShortcutResolver (async task)
        │
        │  1. Normalize label_path (trim, lowercase, strip ellipsis)
        │  2. Resolve AppId from AppMatcher
        │  3. Check excluded_apps list → discard if excluded
        │  4. Tier 1: if discovered_shortcut is Some → use it directly
        │  5. Tier 2a (menu/toolbar): look up ShortcutDatabase by app + menu_path
        │  5. Tier 2b (semantic): look up SemanticAction table by app + role + context
        │  6. Emit ShortcutMatch or discard (no match in either tier)
        │
        │  ShortcutMatch
        │  (tokio::sync::mpsc::Sender)
        ▼
  CalloutRenderer (GUI thread)
        │
        │  1. Update window content
        │  2. Start/reset fade-in animation
        │  3. Arm auto-dismiss timer
        ▼
  OS Window System (screen)
```

Total path length from OS callback to screen update: **3 async hops** (OS callback → channel → resolver → channel → renderer). All three are non-blocking; the resolver's HashMap lookup is O(1). P99 latency target of < 500ms is achievable with wide margin.

---

## 8. Concurrency Model

```
Thread / Task                   Owns
─────────────────────────────────────────────────────────────────
OS callback thread (platform)   DetectionEngine internal state
tokio::runtime (2 worker threads)
  └── ShortcutResolver task     ShortcutDatabase (Arc<>, read-only)
      ControlCommand handler    ConfigManager (Arc<RwLock<Config>>)
GUI thread (GTK / Win32 / NSApp) CalloutRenderer, overlay window
```

`ShortcutDatabase` is built once at startup and never mutated; it is wrapped in `Arc<ShortcutDatabase>` for cheap cloning across tasks. `Config` is wrapped in `Arc<RwLock<Config>>` because both the file watcher and the control command handler may write it, while the resolver reads it on every event.

The tokio runtime is intentionally **small** (2 worker threads). KeyboardHero is I/O-bound, not compute-bound. Overprovisioning threads increases RSS for no gain.

#### Channel sizing

```
raw_event_tx (DetectionEngine → Resolver):   bounded(32)
hint_tx       (Resolver → Renderer):          bounded(8)
control_tx    (Tray/Signal → Lifecycle):      bounded(4)
```

Bounded channels provide natural backpressure. If the resolver is slow (it shouldn't be — it's O(1)), the detection engine's `send` will block rather than accumulating unbounded events. The renderer channel is small because the display replaces rather than queues hints.

---

## 9. Error Handling Strategy

KeyboardHero must never crash on unrecognized application structures (PRD NFR Reliability). The error model follows these rules:

1. **Detection errors are silently discarded.** An `AXUIElement` walk that returns an error produces no event — not a crash, not a log message.
2. **Database misses are silent.** No match in the shortcut table → no callout. This is the normal code path for actions without shortcuts.
3. **Config file parse errors use the last-known-good config.** A malformed config.toml does not stop the daemon; it continues with the previous (or default) configuration and logs a single warning to a bounded in-memory ring buffer.
4. **Platform API unavailability is surfaced to the user, not silently ignored.** If AT-SPI2 is missing or the macOS accessibility permission is denied, the tray icon shows a warning state. The daemon stays alive waiting for the condition to be resolved.
5. **Renderer errors are non-fatal.** If the overlay window fails to create (e.g., no display), the daemon continues running — the detection and resolver still work, they just have no output path.

```rust
// Error type hierarchy
pub enum DaemonError {
    PlatformUnavailable(PlatformError),   // surfaced to tray; daemon waits
    ConfigInvalid(toml::de::Error),       // use defaults; log to ring buffer
    SingleInstanceConflict,               // exit(1) immediately
    Shutdown,                             // clean exit
}

// Per-event errors: not in DaemonError; handled inline with ? or .ok()
```

---

## 10. Testing Strategy

### Unit tests

| Module | What is tested |
|--------|----------------|
| `shortcut_db` | TOML parse, AppMatcher resolution, menu path normalization, fallback to global shortcuts |
| `resolver` | Excluded app filtering, cross-platform key formatting, label_path → ShortcutMatch mapping |
| `config` | Deserialize valid and invalid TOML, clamp out-of-range values |
| `animation` | State machine transitions, timer reset on new event |

### Integration tests (requires display)

These run only when `DISPLAY` or `WAYLAND_DISPLAY` is set (Linux CI) or on macOS CI runners with accessibility granted.

| Test | Description |
|------|-------------|
| `detection_roundtrip` | Launch a test GTK4 app with a known menu; simulate a click; assert `RawActivationEvent` with correct label_path arrives within 500ms |
| `shortcut_resolved` | Feed a `RawActivationEvent` for `["Edit", "Copy"]` + `firefox` + `linux`; assert `ShortcutMatch { keys: "Ctrl + C" }` |
| `callout_displayed` | Full end-to-end: detection → resolve → render; assert overlay window appears and disappears |

### Performance regression tests

Run in CI on every commit:

```
cargo bench --bench latency
```

The benchmark simulates 10,000 `RawActivationEvent` values through the resolver and asserts P99 < 1ms (well under the 500ms budget, leaving ample margin for OS callback delivery time).

### Manual test corpus

A spreadsheet of ≥ 50 (app, menu path, expected shortcut) tuples is maintained in `tests/corpus.csv`. A companion script opens each application via `xdotool` / `AutoHotKey` / `osascript`, performs the menu action, and records whether the callout appeared. This is the basis for the ≥ 85% detection recall metric (PRD G1).

---

## 11. Performance Analysis

### CPU budget

| Activity | Estimated cost | Frequency |
|----------|---------------|-----------|
| AT-SPI2 D-Bus poll (idle) | < 0.1% | Continuous |
| RawActivationEvent parse + channel send | ~5μs | Per user action |
| ShortcutResolver lookup (HashMap O(1)) | ~1μs | Per user action |
| Callout render + composite | ~2ms (GPU-accelerated) | Per hint, ≤ 150ms |
| Config file watch (inotify/FSEvents) | < 0.01% | Continuous |

**Steady-state CPU** is dominated by D-Bus/IPC polling, estimated at < 0.2%. Well within the 1% budget.

### Memory budget

| Component | Estimated RSS |
|-----------|--------------|
| Rust binary + stdlib | ~5 MB |
| tokio runtime + 2 threads | ~2 MB |
| Shortcut database (all TOML, loaded) | ~3 MB |
| GTK4 / Win32 / AppKit runtime | ~15–25 MB |
| GUI thread stack + overlay window | ~2 MB |
| Config + misc heap | ~1 MB |
| **Total (estimated)** | **~30–38 MB** |

Comfortably within the 50 MB RSS budget.

### Startup time

Target: daemon ready to detect events within **1 second** of process start. The bottleneck is GTK4 initialization (~200ms on a cold system). The shortcut database parse and index build is < 50ms for ≥ 15 apps.

---

## 12. Security Considerations

### What the daemon observes

KeyboardHero reads:
- The **name** of the activated UI element (e.g., "Copy", "Paste").
- The **ancestry path** of that element (e.g., ["Edit", "Copy"]).
- The **application name** of the foreground window.

KeyboardHero **never** reads:
- Clipboard contents.
- Text field contents (not in the accessibility event; not requested).
- File paths or document names.
- Keystrokes.

This is architecturally enforced: the Detection Engine only subscribes to activation events, not text-change or value-change events. There is no code path that accesses clipboard or document content.

### Network

There are no outbound sockets. The `tokio` runtime is created with no I/O facilities beyond the channel primitives used internally. No DNS lookups, no HTTP clients, no update pings. This can be verified with `ss -tp` / `lsof -i` / Activity Monitor at runtime.

### Privilege

The daemon runs as the invoking user. It requests no elevated privileges at install time beyond:
- macOS: Accessibility permission (user-granted, scoped to this process).
- Linux: AT-SPI2 access (available to all session processes by default).
- Windows: No special permissions required; UIAutomation/MSAA work in user context.

### IPC attack surface

There is no IPC socket, pipe, or HTTP endpoint. The tray icon menu is the only external control surface. There is no way for another process to inject events or commands into the daemon.

---

## 13. Packaging and Distribution

### Linux

**AppImage**: A self-contained AppImage is built in CI via `cargo build --release` + `linuxdeploy`. Bundles GTK4 libraries if the host distro's version is older than required.

**AUR (Arch Linux)**: A `PKGBUILD` for `keyboardhero-bin` (binary package) and `keyboardhero` (build from source) will be maintained. AT-SPI2 is a `depends` entry, not `optdepends`.

**Autostart (Linux)**: An XDG autostart `.desktop` file is written to `$XDG_CONFIG_HOME/autostart/keyboardhero.desktop` on first run (after user approval). A systemd user unit (`keyboardhero.service`) is provided for users who prefer it.

### Windows

**NSIS installer**: A single `.exe` installer built with NSIS. Registers `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` for autostart. Includes a Visual C++ Redistributable check (bundled if absent). Provides an Add/Remove Programs entry.

### macOS

**DMG**: A standard `.dmg` with an `.app` bundle. Code-signed and notarized (required for Gatekeeper on macOS 13+). The LaunchAgent plist is written to `~/Library/LaunchAgents/io.keyboardhero.daemon.plist` on first run.

### Build pipeline (GitHub Actions)

```yaml
# Three matrix jobs:
jobs:
  build-linux:   runs-on: ubuntu-22.04
  build-windows: runs-on: windows-2022
  build-macos:   runs-on: macos-13

# Each job:
#   1. cargo test
#   2. cargo build --release
#   3. Package (AppImage / NSIS / DMG)
#   4. Upload artifact
```

---

## 14. Open Questions and Risks

### Risk 1 (High) — GNOME Wayland overlay

**Problem**: GNOME does not implement `wlr-layer-shell`. GTK4 window type hints for "notification" windows are compositor-discretionary — GNOME may reposition or decorate the window.

**Mitigation options** (in priority order):
1. GTK4 `GtkWindow` with `gtk_window_set_decorated(false)`, positioned programmatically to the bottom-center. Accept that GNOME may apply window chrome on some versions and file a bug upstream.
2. A minimal GNOME Shell extension that acts as a rendering backend — the daemon sends a D-Bus message with the hint content; the extension renders it. This is the cleanest architectural solution but adds packaging complexity (JS extension + daemon).
3. Fall back to XWayland unconditionally on GNOME Wayland (user opt-in or auto-detect).

**Decision point**: Alpha engineering spike, target: 2 weeks. The extension approach is the likely path for a polished V1 on GNOME Wayland.

**Open question**: Is it acceptable for V1 to ship the GNOME Shell extension as a required companion on GNOME Wayland? Or do we ship V1 Linux as X11/XWayland-only and add native Wayland in V1.1?

### Risk 2 (Medium) — AT-SPI2 disabled by default

Some Ubuntu, Fedora, and Debian configurations ship with AT-SPI2 bridge disabled. The `org.a11y.Status.IsEnabled` D-Bus property controls this.

**Mitigation**: At startup, query `IsEnabled`. If false, attempt to set it to `true` (works on GNOME). If that fails, show a one-time dialog guiding the user to enable it and restart, with a link to platform-specific instructions.

### Risk 3 (Medium) — macOS permission UX friction

The macOS Accessibility permission prompt is jarring and uncontextual.

**Mitigation**: A first-run onboarding screen (native `NSAlert` or minimal window) explains exactly what KeyboardHero observes and why the permission is needed, before the system prompt fires. Screenshots of the permission dialog are included.

### Risk 4 (Medium) — Shortcut database accuracy

Menu labels differ across application versions, locales, and user customizations.

**Mitigation**: The database applies case-insensitive matching and strips trailing `…`/`...`. For locale differences, V1 ships English labels only and notes this limitation. Community contributions via GitHub PRs address coverage gaps. The conservative-by-default policy (FR-06: only fire on unambiguous matches) means missed detections are preferable to wrong callouts.

---

## 15. Alternatives Considered

### Alternative A: Python daemon

**Considered**: Python with `pyatspi` (Linux), `pywinauto` (Windows), `pyobjc` (macOS).

**Rejected**: Python's interpreter overhead puts the steady-state RSS above 80 MB (CPython alone) and CPU above 1% in testing on comparable daemon workloads. The PRD resource requirements cannot be met.

### Alternative B: Electron app

**Considered**: Electron provides a cross-platform GUI framework and Node.js ecosystem.

**Rejected**: Electron's baseline RSS is ~150 MB. The PRD requires < 50 MB. Additionally, Electron does not provide direct access to OS accessibility APIs — a native addon would still be required.

### Alternative C: Go daemon

**Considered**: Go has a smaller footprint than Python/Electron and supports FFI via `cgo`.

**Not rejected outright**: Go could technically meet the resource requirements. However, Go's GC introduces unpredictable pause times that could spike latency above 500ms under memory pressure. Rust's zero-cost model is more predictable. Go was rated a close second and remains a viable fallback if Rust hiring proves difficult.

### Alternative D: Per-app scripts / hooks

**Considered**: App-specific plugins (VS Code extension, browser extension, etc.).

**Rejected**: This approach does not scale to "all applications" (PRD G5) and requires separate installation and maintenance per application. It directly contradicts the PRD's universal coverage requirement.

### Alternative E: Kernel-level input interception (e.g., `evdev`, `uinput`)

**Considered**: Intercept raw input events before they reach applications.

**Rejected**: This approach requires elevated privileges (input group on Linux, kernel extension on macOS) and cannot determine *which menu item* was activated — only that a mouse click occurred. It cannot satisfy the menu-path-to-shortcut resolution requirement.

---

## 16. Implementation Roadmap

### Milestone 0 — Project scaffold (Week 1)

- [ ] Cargo workspace with crates: `kbhero-core`, `kbhero-linux`, `kbhero-windows`, `kbhero-macos`, `kbhero-daemon`
- [ ] CI pipeline (GitHub Actions, 3-platform matrix)
- [ ] TOML shortcut database schema, parser, and index (with unit tests)
- [ ] `AppMatcher` with unit tests
- [ ] `Config` struct with `serde` deserialization and clamping

### Milestone 1 — Linux Alpha (Weeks 2–5)

- [ ] AT-SPI2 detection engine (`kbhero-linux`)
- [ ] GTK4 overlay window (X11 path; Wayland path follow-up)
- [ ] Fade animation + auto-dismiss
- [ ] System tray (via `ksni` or `libappindicator`)
- [ ] End-to-end test: Firefox menu → callout
- [ ] Shortcut database: 5 apps (Firefox, Chromium, GNOME Text Editor, Nautilus, GNOME Terminal)

### Milestone 2 — Windows Alpha (Weeks 6–9)

- [ ] `SetWinEventHook` + MSAA detection engine (`kbhero-windows`)
- [ ] Win32 layered window + Direct2D renderer
- [ ] System tray (Win32 `Shell_NotifyIcon`)
- [ ] End-to-end test: Notepad menu → callout
- [ ] Shortcut database: 5 apps (Chrome, Firefox, Notepad, Explorer, VS Code)

### Milestone 3 — macOS Alpha (Weeks 10–13)

- [ ] `AXObserver` detection engine (`kbhero-macos`)
- [ ] `NSPanel` overlay + Core Graphics renderer
- [ ] First-run accessibility onboarding
- [ ] System tray (`NSStatusBar`)
- [ ] End-to-end test: Safari menu → callout
- [ ] Shortcut database: 5 apps (Safari, Chrome, TextEdit, Finder, Terminal)

### Milestone 4 — Beta (Weeks 14–18)

- [ ] Settings UI (native per-platform or unified via `egui`)
- [ ] Application exclusion list
- [ ] Autostart management (all platforms)
- [ ] Shortcut database: ≥ 15 apps total
- [ ] GNOME Wayland overlay (extension approach, if spike confirms)
- [ ] Performance profiling; RAM/CPU regression tests pass
- [ ] Manual test corpus ≥ 50 entries; recall ≥ 85%
- [ ] Code-sign + notarize macOS build

### Milestone 5 — V1.0 Release (Weeks 19–20)

- [ ] AppImage, NSIS installer, DMG builds in CI
- [ ] AUR package
- [ ] Privacy statement + first-run onboarding copy finalized
- [ ] README, install documentation

---