# Product Requirements Document: KeyboardHero

**Author:** Calvin Nielson
**Last Updated:** 2026-05-29

---

## 1. Executive Summary

Knowledge workers spend thousands of hours every year performing repetitive tasks through graphical menus and mouse-driven UI when equivalent keyboard shortcuts exist. The efficiency gap compounds: the same user who ignores `Ctrl+S` performs that action hundreds of times daily. Existing solutions (cheat sheets, static tutorials) are disconnected from the user's live workflow and are universally ignored after the first week.

**KeyboardHero** is a lightweight, cross-platform background daemon that observes user interactions in real time, detects when a mouse- or menu-driven action has a keyboard equivalent the user did not invoke, and surfaces a brief callout with the shortcut and a one-line description. The callout appears at the bottom-center of the screen, requires no action from the user, and disappears automatically. It is intentionally persistent: if a user repeatedly ignores a shortcut they could be using, KeyboardHero keeps telling them.

The result is contextual, progressive keyboard fluency earned through real consequences — not passive suggestion.

---

## 2. Problem Statement

### 2.1 User Problem

Power users and professionals repeatedly perform actions through slow, attention-consuming paths (menus, right-click context menus, toolbar buttons) when keyboard shortcuts exist that would complete the same task in a fraction of the time. The friction is not ignorance of shortcuts in principle — it is the absence of a tight feedback loop between behavior and opportunity.

The three root causes:

1. **Discovery gap**: Users don't know which shortcuts exist for their specific applications.
2. **Recall gap**: Users know shortcuts exist but can't recall the binding at action time.
3. **Habit gap**: Users know the shortcut but default to the mouse out of ingrained habit.

No current tool addresses all three at the point of action, for any application, on any platform.

### 2.2 What We Are Not Solving

- Macro recording or automation.
- Shortcut customization or rebinding.
- Drag-and-drop detection or other actions that cannot be strictly mapped to a keyboard shortcut equivalent.
- Accessibility assistance for users with motor impairments (distinct goal, distinct design).

---

## 3. Goals and Out of Scope

### 3.1 Goals

| # | Goal | Measurable Signal |
|---|------|-------------------|
| G1 | Detect menu-, toolbar-, and context-menu-driven actions that have known keyboard equivalents, across any application | ≥ 85% detection recall on a defined test corpus spanning system apps, browsers, editors, and productivity suites |
| G2 | Display a non-intrusive shortcut callout within 500ms of action completion | P99 display latency < 500ms |
| G3 | Run as a background daemon with negligible resource footprint | < 1% sustained CPU, < 50MB RAM at steady state on a mid-range 2023 laptop |
| G4 | Support system startup (autostart) on all three platforms | Works out-of-the-box on Linux (systemd user unit / XDG autostart), Windows (startup registry), macOS (launchd) |
| G5 | Work across all major applications without per-app configuration | Leverages OS accessibility APIs for universal coverage; application-specific shortcut data refined per app |
| G6 | Reinforce learning through repetition: callout appears every time the shortcut is missed, with no suppression | Callout shown on every detected miss; no throttle, no DND override |

### 3.2 Out of Scope

- **NG1**: Custom user-defined shortcut database (In V2 — user can extend and maintain their own shortcut mappings).
- **NG2**: Network connectivity or telemetry of any kind. No user data leaves the device, ever, in any form. This is non-negotiable and made transparent to the user at install time.
- **NG3**: Games and game input (DirectInput, SDL, raw HID). Keyboard shortcuts in games are not the same concept; this use case is explicitly excluded.
- **NG4**: Mobile platforms (iOS, Android).
- **NG5**: Actions that cannot be strictly and unambiguously mapped to a keyboard shortcut (e.g., drag-and-drop file moves, freeform gestures). When in doubt, do not trigger.
- **NG6**: Teaching mode, quiz, or spaced repetition.

---

## 4. User Stories and Acceptance Criteria

### Epic 1: Core Detection and Display

**US-001 — Menu action detection**
> When I open the Edit menu and click "Copy," I want to see a callout displaying `Ctrl+C — Copy selection to clipboard` so I am reminded the shortcut exists.

*Acceptance Criteria:*
- AC1: Callout appears within 500ms of menu item activation.
- AC2: Callout is positioned at the bottom-center of the screen, not too wide (max ~400px).
- AC3: Callout displays: key combination (formatted per OS convention), action name, one-line description.
- AC4: Callout does not steal focus or block input.
- AC5: Callout auto-dismisses after a configurable duration (default: 3s).

**US-002 — No suppression on repeat**
> When I repeatedly open Edit > Copy via the menu, the callout should appear every single time. I should feel the friction of not having used the shortcut.

*Acceptance Criteria:*
- AC1: No throttling or suppression of any kind by default.
- AC2: Callout fires on every detected shortcut miss, regardless of how recently the same hint was shown.

**US-003 — Context menu detection**
> When I right-click and select "Paste," I want to see the relevant shortcut hint.

*Acceptance Criteria:*
- AC1: Right-click context menus treated the same as menu bar menus.
- AC2: Detection covers OS-provided context menus; best-effort for custom-rendered ones.

**US-004 — Toolbar button detection**
> When I click the Save button in a toolbar instead of pressing the keyboard shortcut, I want to be reminded of the shortcut.

*Acceptance Criteria:*
- AC1: Detection covers toolbar buttons that are exposed via the OS accessibility API.
- AC2: Best-effort coverage; only fires when shortcut mapping is unambiguous and confirmed.

**US-005 — Application-aware shortcut mapping**
> When I use Edit > Undo in Inkscape on Linux, the callout shows `Ctrl+Z`. When the same action is triggered on macOS, it shows `⌘Z`.

*Acceptance Criteria:*
- AC1: Shortcut database is OS-aware.
- AC2: Application-specific overrides (e.g., VS Code's own binding conventions) are supported.
- AC3: Coverage spans all applications whose menus are accessible via the platform's accessibility API — not limited to system-level apps.

### Epic 2: Background Operation

**US-006 — Autostart on login**
> After installing KeyboardHero, it should start automatically when I log in, without requiring me to configure anything.  It must prompt the user for this permission.

*Acceptance Criteria:*
- AC1: Installer/first-run creates the appropriate autostart entry on all supported platforms.
- AC2: Daemon survives logout/login cycles.
- AC3: A system tray icon (or equivalent on Wayland) indicates KeyboardHero is running.

**US-007 — Low resource footprint**
> Running KeyboardHero should not cause perceptible slowdown or battery drain.

*Acceptance Criteria:*
- AC1: Sustained CPU < 1% on a quad-core 2023 laptop at idle (no user interaction).
- AC2: Peak CPU during event processing < 5% for < 200ms.
- AC3: Resident memory < 50MB.
- AC4: No disk writes during steady-state operation (no logging to disk by default).

### Epic 3: Configuration

**US-008 — Basic configuration via UI**
> I want to adjust the callout duration without editing a config file.

*Acceptance Criteria:*
- AC1: Settings accessible from system tray or equivalent.
- AC2: Configurable: callout duration (1–10s).
- AC3: Settings persisted to a user config file (`~/.config/keyboardhero/config.toml` or OS equivalent).

**US-009 — Application exclusion list**
> I never want KeyboardHero hints in specific apps (e.g., a video editor where mouse is intentional).

*Acceptance Criteria:*
- AC1: User can add application names to an exclusion list.
- AC2: Exclusion list is editable from the Settings panel.

---

## 5. Functional Requirements

### 5.1 Detection Engine

| ID | Requirement |
|----|-------------|
| FR-01 | Observe menu bar activations system-wide using OS accessibility APIs, for all applications that expose accessible menus. |
| FR-02 | Observe right-click context menu selections system-wide. |
| FR-03 | Observe toolbar button activations via accessibility APIs on a best-effort basis. |
| FR-04 | Identify the foreground application at event time. |
| FR-05 | Look up the activated element in the shortcut database for the current application and OS. |
| FR-06 | Emit a `HintEvent` only when a shortcut mapping is unambiguous and confirmed. Do not trigger on actions that cannot be strictly defined as a keyboard shortcut equivalent. |
| FR-07 | No suppression or throttling. Every detected miss fires a callout. |

### 5.2 Shortcut Database

| ID | Requirement |
|----|-------------|
| FR-10 | Bundled database of shortcuts for ≥ 15 common cross-platform applications at launch (browsers, text editors, file managers, terminals, office suites). |
| FR-11 | Database schema supports: `app_id`, `platform`, `menu_path` (e.g., `Edit > Copy`), `shortcut_keys`, `description`, `category`. |
| FR-12 | Application matching uses executable name and optionally window title pattern. |
| FR-13 | Database is human-readable (TOML), version-controlled, and structured for community contribution in V2. |
| FR-14 | Coverage is not limited to system-level apps. Every application whose menus are accessible via the platform API is in scope. |

### 5.3 Callout Renderer

| ID | Requirement |
|----|-------------|
| FR-20 | Render a floating, always-on-top, non-focusable overlay window. |
| FR-21 | Display: key combination (OS-formatted keycaps), action name, one-line description. |
| FR-22 | Position: horizontally centered at the bottom of the screen, ~16px above the taskbar/dock. Maximum width: 400px. |
| FR-23 | Animate in (fade/slide) and out (fade). Animation duration ≤ 150ms. |
| FR-24 | Callout must not intercept mouse events (click-through). |
| FR-25 | Does not suppress on OS Do Not Disturb / Focus Assist where avoidable. The callout is the user's own feedback tool; it is not a system notification and should not be silenced by notification settings. |
| FR-26 | Support HiDPI / Retina / fractional scaling displays. |
| FR-27 | Follow OS light/dark mode automatically. |

### 5.4 Daemon and Lifecycle

| ID | Requirement |
|----|-------------|
| FR-30 | Daemon runs as a standard user process (no root/admin required post-install). |
| FR-31 | Single instance enforcement. |
| FR-32 | System tray icon (or Wayland-equivalent status indicator) with menu: Pause/Resume, Settings, Quit. |
| FR-33 | Graceful shutdown on SIGTERM / OS logout. |
| FR-34 | Autostart entries created during install, removable via Settings. |

---

## 6. Non-Functional Requirements

| Category | Requirement |
|----------|-------------|
| **Performance** | Steady-state CPU < 1%; peak burst < 5% for < 200ms; RSS < 50MB. |
| **Latency** | P99 callout display latency < 500ms from element activation. |
| **Reliability** | Must not crash on unrecognized application or menu structure; errors silently discarded or written to a bounded debug log. |
| **Privacy** | Zero network requests. Zero persistent logging of user actions. Zero telemetry. This is made explicit and verifiable (open source, no outbound connections). |
| **Security** | No elevated privileges. Does not read, store, or transmit file contents, clipboard data, or keystrokes entered into text fields. |
| **Accessibility** | Callout text meets WCAG AA contrast ratio. Font size configurable. |
| **Packaging** | Single installer per platform: `.AppImage` and/or AUR package on Linux, `.exe` NSIS installer on Windows, `.dmg` on macOS. |
| **Internationalization** | UI strings externalized for i18n in V1 even if only English ships; shortcut database supports locale-aware key names. |

---

## 7. UX and Design

### 7.1 Callout Anatomy

```
         ┌──────────────────────────────────────┐
         │   Ctrl + C     Copy to clipboard     │
         │   Use instead of  Edit › Copy        │
         └──────────────────────────────────────┘
                  ▲ centered, bottom of screen
```

- Max width: ~400px. Compact, single or two lines.
- Keycaps rendered as OS-native pill badges (e.g., `⌘` `C` on macOS, `Ctrl` `C` on Linux/Windows).
- Follows OS light/dark theme.

### 7.2 Design Principles

1. **Invisible by default**: The callout is the only visible artifact; no persistent UI widget on screen.
2. **Never blocking**: Click-through; never steals focus; never pauses the user's workflow.
3. **Persistent by design**: Unlike notification-style tools, KeyboardHero does not back off when ignored. The user earns silence by using shortcuts, not by waiting.
4. **Strictly scoped detection**: A callout only fires when the action-to-shortcut mapping is unambiguous. No false positives from ambiguous gestures or drag-and-drop.
5. **Transparent**: Open source. No network calls. Any technical user can verify what the daemon does.

---

## 8. Platform Strategy

| Platform | Detection Mechanism | Overlay Technology | Autostart |
|----------|--------------------|--------------------|-----------|
| Linux — Wayland (GNOME, KDE, etc.) | AT-SPI2 (menu/toolbar events) | GTK4 overlay window or wlr-layer-shell (compositor-dependent) | systemd user unit |
| Linux — X11 / XWayland | AT-SPI2 + XRecord | X11 override-redirect window | XDG autostart + systemd user unit |
| Windows 10/11 | UIAutomation + WinEventHook | Win32 layered window (`WS_EX_LAYERED` + `WS_EX_TRANSPARENT`) | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |
| macOS 13+ | AXObserver + CGEventTap | `NSWindow` non-activating panel | LaunchAgent plist |

**V1 Scope**: Linux Wayland (GNOME primary target, best-effort KDE/other), Linux X11, Windows 10+, macOS 13+.

### 8.1 Wayland Notes

GNOME on Wayland exposes accessible UI trees via AT-SPI2, which provides the menu activation events needed for detection. The primary Wayland constraint is overlay rendering: GNOME does not implement `wlr-layer-shell`, so the callout window must be rendered via a GNOME-compatible path (GTK4 with appropriate window type hints, or a GNOME Shell extension as a rendering backend). This is an engineering risk addressed in the Design Doc.

---

## 9. Privacy Commitment

KeyboardHero observes which menu or toolbar items a user activates — not the content of any document, clipboard, or text field. All processing is local. There is no telemetry, no crash reporting, no update pings, and no analytics — in V1 or beyond without explicit opt-in.

This is made credible by:
- Open-sourcing the full codebase.
- A plain-language privacy statement shown during install.
- No outbound network sockets opened by the daemon (verifiable via `ss` / Activity Monitor).
- Requesting only the minimum OS permissions required.

---

## 10. V2 Outlook

The following are explicitly out of scope for V1 but inform V1 architecture decisions:

- **User-defined shortcut database**: Users will be able to add, edit, and maintain their own shortcut mappings. The V1 database schema must be designed to support this extension without breaking changes.
- **Native Wayland for non-GNOME compositors**: Broader layer-shell support once the protocol landscape stabilizes.

---

## 11. Success Metrics

| Metric | Target |
|--------|--------|
| Crash rate | < 0.1% of daemon-hours |
| P99 detection latency | < 500ms |
| Resource constraints | 95% of installs under 1% CPU / 50MB RAM |
| Detection precision | ≥ 95% (no callout fired for actions without a strict shortcut mapping) |
| Detection recall | ≥ 85% across test corpus (menu/toolbar actions that have a known shortcut) |

---

## 12. Risks

| Risk | Severity | Likelihood | Mitigation |
|------|----------|------------|------------|
| macOS accessibility permission prompts scare users | High | High | First-run onboarding explains exactly why permission is needed and what it does not observe. |
| GNOME Wayland overlay rendering has no clean API path | High | Medium | Engineering spike in Alpha; fallback to XWayland path if GNOME Shell extension approach is unacceptable. |
| AT-SPI2 not enabled by default on some distros | Medium | Medium | Installer checks and enables AT-SPI2; provides user-facing guidance if manual step required. |
| Shortcut database stale or incorrect for specific apps | Medium | High | Baseline is conservative and tested; database is version-controlled and structured for community correction via GitHub PRs. |
| "Annoying by design" perception drives uninstalls | Medium | Medium | Framed clearly in marketing: this tool is for users who want to build keyboard habits, not for casual use. Pause/Resume provides an escape valve without uninstalling. |

---