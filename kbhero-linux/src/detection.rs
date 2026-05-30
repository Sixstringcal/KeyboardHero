#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use atspi::{
    events::{Event, EventProperties, ObjectEvents},
    proxy::{accessible::AccessibleProxy, action::ActionProxy},
    AccessibilityConnection, Role, State, StateSet,
};
use futures_lite::StreamExt;
use kbhero_core::types::{AppIdentity, ElementRole, RawActivationEvent};
use thiserror::Error;
use tokio::sync::mpsc;
use zbus::{proxy::CacheProperties, Connection};

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("could not connect to AT-SPI2 bus: {0}")]
    Connect(#[from] atspi::AtspiError),
}

pub struct AtSpi2Engine {
    conn: AccessibilityConnection,
    zbus: Connection,
}

impl AtSpi2Engine {
    pub async fn connect() -> Result<Self, EngineError> {
        eprintln!("[engine] connect(): opening AT-SPI2 connection...");
        let conn = AccessibilityConnection::new().await?;
        conn.register_event::<atspi::events::object::StateChangedEvent>().await?;
        eprintln!("[engine] connect(): registered. Engine ready.");
        let zbus = conn.inner().connection().clone();
        Ok(Self { conn, zbus })
    }

    /// Consume accessibility events until the AT-SPI2 bus disconnects.
    ///
    /// # Click vs hover detection
    ///
    /// 1. **Candidate phase:** On `Selected/Focused=true` for a menu item,
    ///    build the event and park it.  The item's parent must be a `Menu` or
    ///    `PopupMenu` accessible — this is the key filter that rejects toolbar
    ///    focus events, programmatic state updates after an action, and any
    ///    focus change that happens after the menu has already closed.
    ///
    /// 2. **Commit phase:** When the candidate item loses its
    ///    `Selected/Focused` state we start a 100 ms debounce.  If no new
    ///    menu item is selected in that window the menu has closed and we emit.
    pub async fn run(self, tx: mpsc::Sender<RawActivationEvent>) {
        eprintln!("[engine] run() started, waiting for AT-SPI2 events...");

        struct Candidate {
            item_sender: String,
            item_path:   String,
            event:       RawActivationEvent,
        }

        let mut candidate: Option<Candidate> = None;
        let (debounce_tx, mut debounce_rx) = mpsc::channel::<RawActivationEvent>(4);
        let mut stream = self.conn.event_stream();

        loop {
            tokio::select! {
                result = stream.next() => {
                    let ev = match result {
                        Some(Ok(Event::Object(ObjectEvents::StateChanged(ev)))) => ev,
                        Some(Ok(_))  => continue,
                        Some(Err(e)) => { eprintln!("[engine] stream error: {e}"); continue; }
                        None         => break,
                    };

                    let sender = ev.sender().to_string();
                    let path   = ev.path().to_string();

                    match (ev.state, ev.enabled) {
                        (State::Selected | State::Focused, true) => {
                            if let Some(raw) = build_event(&self.zbus, ev).await {
                                eprintln!("[engine] candidate: {:?} / {:?}", raw.label_path, raw.discovered_shortcut);
                                candidate = Some(Candidate {
                                    item_sender: sender,
                                    item_path:   path,
                                    event:       raw,
                                });
                            }
                        }

                        (State::Selected | State::Focused, false) => {
                            let is_our_item = candidate.as_ref().map_or(false, |c| {
                                c.item_sender == sender && c.item_path == path
                            });
                            if is_our_item {
                                if let Some(c) = candidate.take() {
                                    let dtx = debounce_tx.clone();
                                    tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_millis(100)).await;
                                        let _ = dtx.send(c.event).await;
                                    });
                                }
                            }
                        }

                        _ => {}
                    }
                }

                Some(deselected_event) = debounce_rx.recv() => {
                    if candidate.is_none() {
                        eprintln!("[engine] debounce commit — emitting hint");
                        let _ = tx.send(deselected_event).await;
                    } else {
                        eprintln!("[engine] debounce discarded (new candidate exists)");
                    }
                }
            }
        }

        eprintln!("[engine] event stream ended");
    }
}

// ── Event mapping ─────────────────────────────────────────────────────────────

async fn build_event(
    conn: &Connection,
    ev: atspi::events::object::StateChangedEvent,
) -> Option<RawActivationEvent> {
    let sender = ev.sender().to_string();
    let path   = ev.path().to_string();

    let proxy = accessible(conn, &sender, &path).await?;

    let role = proxy.get_role().await.ok()?;
    if !matches!(role, Role::MenuItem | Role::CheckMenuItem | Role::RadioMenuItem) {
        return None;
    }

    // Only track items that are currently visible on screen.  This filters
    // programmatic state updates (e.g. enabling Redo after an undo executes)
    // that fire after the menu has already closed.
    //
    // Note: gnome-text-editor uses Role::Panel throughout its menu hierarchy
    // rather than Role::Menu/PopupMenu, so an ancestor-role check does not
    // work for that app.  State::Showing is the reliable cross-toolkit guard.
    let states = proxy.get_state().await.unwrap_or_else(|_| StateSet::empty());
    if !states.contains(State::Showing) {
        return None;
    }

    let attrs = proxy.get_attributes().await.ok().unwrap_or_default();
    let shortcut_from_attrs = attrs.get("keyshortcuts").and_then(|s| parse_primary_shortcut(s));

    // Try every available AT-SPI2 text source in order of reliability.
    // gnome-text-editor (and some other GTK4 apps) leave `name()` empty due to
    // a known GtkModelButton accessibility issue, so we fall through all paths.
    let name = resolve_item_name(conn, &sender, &path, &proxy, &attrs).await;

    eprintln!("[build] name={name:?} shortcut_attrs={shortcut_from_attrs:?} all_attrs={attrs:?}");

    if name.is_empty() && shortcut_from_attrs.is_none() {
        return None;
    }

    let label_path = if name.is_empty() {
        vec![]
    } else {
        walk_menu_path(conn, &sender, &path, name).await
    };

    let discovered_shortcut = if shortcut_from_attrs.is_some() {
        shortcut_from_attrs
    } else {
        read_keybinding(conn, &sender, &path).await
    };

    let app = read_app_identity(conn, &proxy).await;

    eprintln!("[detect] app={:?} path={:?} shortcut={:?}",
        app.executable, label_path, discovered_shortcut);

    Some(RawActivationEvent {
        timestamp: Instant::now(),
        app,
        role: ElementRole::MenuItem,
        label_path,
        discovered_shortcut,
    })
}

/// Tries every available AT-SPI2 path to find a human-readable name for a
/// menu item.  Returns an empty string if none succeed.
async fn resolve_item_name(
    conn:   &Connection,
    sender: &str,
    path:   &str,
    proxy:  &AccessibleProxy<'_>,
    attrs:  &std::collections::HashMap<String, String>,
) -> String {
    // 1. Standard accessible name (most toolkits set this correctly).
    let n = proxy.name().await.ok().unwrap_or_default();
    if !n.is_empty() { return n; }

    // 2. ARIA/accessible attributes — some GTK4 apps store the label here
    //    under keys like "label" or "aria-label".
    for key in &["label", "aria-label"] {
        if let Some(v) = attrs.get(*key) {
            if !v.is_empty() { return v.clone(); }
        }
    }

    // 3. Accessible description — occasionally used as the primary label in
    //    apps that conflate name and description.
    let d = proxy.description().await.ok().unwrap_or_default();
    if !d.is_empty() { return d; }

    // 4. AT-SPI2 Text interface directly on this item — some GtkModelButton
    //    instances implement Text even though they don't set accessible name.
    if let Ok(t) = text_content(conn, sender, path).await {
        if !t.is_empty() { return t; }
    }

    // 5. Recursive child-widget search — GTK4 GtkModelButton stores the
    //    visible label on a grandchild GtkLabel; we search up to 4 levels.
    child_name(conn, sender, path).await.unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn walk_menu_path(
    conn: &Connection,
    sender: &str,
    path: &str,
    leaf: String,
) -> Vec<String> {
    let mut segments = vec![leaf];
    let (mut cur_sender, mut cur_path) = (sender.to_string(), path.to_string());

    for _ in 0..6 {
        let proxy = match accessible(conn, &cur_sender, &cur_path).await {
            Some(p) => p,
            None => break,
        };
        let parent_ref = match proxy.parent().await {
            Ok(r) => r,
            Err(_) => break,
        };
        let p_sender = match parent_ref.name_as_str() {
            Some(s) => s.to_string(),
            None => break,
        };
        let p_path = parent_ref.path_as_str().to_string();

        let parent = match accessible(conn, &p_sender, &p_path).await {
            Some(p) => p,
            None => break,
        };
        let prole = match parent.get_role().await {
            Ok(r) => r,
            Err(_) => break,
        };
        if matches!(prole, Role::MenuBar | Role::Frame | Role::Application | Role::Dialog | Role::Window) {
            break;
        }
        if let Ok(name) = parent.name().await {
            if !name.is_empty() {
                segments.insert(0, name);
            }
        }
        cur_sender = p_sender;
        cur_path = p_path;
    }

    segments
}

fn parse_primary_shortcut(s: &str) -> Option<String> {
    s.split_whitespace().next().filter(|t| !t.is_empty()).map(str::to_string)
}

async fn read_keybinding(conn: &Connection, sender: &str, path: &str) -> Option<String> {
    let proxy = ActionProxy::builder(conn)
        .destination(sender)
        .ok()?
        .path(path)
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()?;
    let binding = proxy.get_key_binding(0).await.ok()?;
    let shortcut = binding.split(';').nth(2).filter(|s| !s.is_empty())?.to_string();
    Some(shortcut)
}

async fn read_app_identity(conn: &Connection, proxy: &AccessibleProxy<'_>) -> AppIdentity {
    let default = AppIdentity { executable: String::new(), window_title: None, pid: 0 };

    let app_ref = match proxy.get_application().await {
        Ok(r) => r,
        Err(_) => return default,
    };
    let app_sender = match app_ref.name_as_str() {
        Some(s) => s.to_string(),
        None => return default,
    };
    let app_path = app_ref.path_as_str().to_string();

    let name = match accessible(conn, &app_sender, &app_path).await {
        Some(p) => p.name().await.unwrap_or_default().to_lowercase(),
        None => String::new(),
    };

    AppIdentity { executable: name, window_title: None, pid: 0 }
}

async fn child_name(conn: &Connection, sender: &str, path: &str) -> Option<String> {
    subtree_text(conn, sender, path, 4).await
}

fn subtree_text<'a>(
    conn: &'a Connection,
    sender: &'a str,
    path: &'a str,
    depth: u8,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + 'a>> {
    Box::pin(async move { subtree_text_inner(conn, sender, path, depth).await })
}

async fn subtree_text_inner(
    conn: &Connection,
    sender: &str,
    path: &str,
    depth: u8,
) -> Option<String> {
    if depth == 0 { return None; }
    let proxy = accessible(conn, sender, path).await?;
    let count = proxy.child_count().await.ok().unwrap_or(0);
    eprintln!("[child] depth={depth} path={path} child_count={count}");

    for i in 0..count.min(8) {
        let child_ref = match proxy.get_child_at_index(i).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let c_sender = match child_ref.name_as_str() { Some(s) => s.to_string(), None => continue };
        let c_path   = child_ref.path_as_str().to_string();

        if let Some(child) = accessible(conn, &c_sender, &c_path).await {
            let child_role = child.get_role().await.ok();
            let name = child.name().await.ok().unwrap_or_default();
            let desc = child.description().await.ok().unwrap_or_default();
            eprintln!("[child]   [{i}] role={child_role:?} name={name:?} desc={desc:?}");

            if !name.is_empty() { return Some(name); }
            if !desc.is_empty() { return Some(desc); }
            if let Ok(text) = text_content(conn, &c_sender, &c_path).await {
                eprintln!("[child]   [{i}] text={text:?}");
                if !text.is_empty() { return Some(text); }
            }
            if let Some(n) = subtree_text(conn, &c_sender, &c_path, depth - 1).await {
                return Some(n);
            }
        }
    }
    None
}

async fn text_content(conn: &Connection, sender: &str, path: &str) -> zbus::Result<String> {
    use atspi::proxy::text::TextProxy;
    let proxy = TextProxy::builder(conn)
        .destination(sender)?
        .path(path)?
        .cache_properties(CacheProperties::No)
        .build()
        .await?;
    proxy.get_text(0, -1).await
}

async fn accessible<'c>(conn: &'c Connection, sender: &'c str, path: &'c str) -> Option<AccessibleProxy<'c>> {
    AccessibleProxy::builder(conn)
        .destination(sender)
        .ok()?
        .path(path)
        .ok()?
        .cache_properties(CacheProperties::No)
        .build()
        .await
        .ok()
}

