#![cfg(target_os = "linux")]

use std::time::Instant;

use atspi::{
    events::{Event, EventProperties, ObjectEvents},
    proxy::{accessible::AccessibleProxy, action::ActionProxy},
    AccessibilityConnection, Role, State,
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
    /// Connect to the AT-SPI2 accessibility bus and subscribe to state-change events.
    pub async fn connect() -> Result<Self, EngineError> {
        eprintln!("[engine] connect(): opening AT-SPI2 connection...");
        let conn = AccessibilityConnection::new().await?;
        eprintln!("[engine] connect(): connection opened, registering event...");
        conn.register_event::<atspi::events::object::StateChangedEvent>().await?;
        eprintln!("[engine] connect(): registered. Engine ready.");
        let zbus = conn.inner().connection().clone();
        Ok(Self { conn, zbus })
    }

    /// Consume accessibility events, mapping each menu-item selection to a
    /// `RawActivationEvent`, until the AT-SPI2 bus disconnects.
    pub async fn run(self, tx: mpsc::Sender<RawActivationEvent>) {
        eprintln!("[engine] run() started, waiting for AT-SPI2 events...");
        let mut stream = self.conn.event_stream();
        let mut total = 0u64;
        while let Some(result) = stream.next().await {
            total += 1;
            let ev = match result {
                Ok(Event::Object(ObjectEvents::StateChanged(ev))) => ev,
                Ok(_) => continue,
                Err(e) => { eprintln!("[engine] stream error: {e}"); continue; }
            };
            eprintln!("[engine] StateChanged state={:?} enabled={}", ev.state, ev.enabled);
            // GTK4 uses Focused=true on MenuItem; GTK3/others use Selected=true.
            if !matches!(ev.state, State::Selected | State::Focused) || !ev.enabled {
                continue;
            }
            if let Some(raw) = build_event(&self.zbus, ev).await {
                let _ = tx.send(raw).await;
            }
        }
        eprintln!("[engine] event stream ended after {total} events");
    }
}

// ── Event mapping ─────────────────────────────────────────────────────────────

async fn build_event(
    conn: &Connection,
    ev: atspi::events::object::StateChangedEvent,
) -> Option<RawActivationEvent> {
    // Bind to locals so the temporaries outlive the await points below.
    let sender = ev.sender().to_string();
    let path   = ev.path().to_string();

    eprintln!("[build] Focused/Selected event: sender={sender} path={path}");

    let proxy = match accessible(conn, &sender, &path).await {
        Some(p) => p,
        None => { eprintln!("[build] proxy creation failed"); return None; }
    };

    let role = match proxy.get_role().await {
        Ok(r) => r,
        Err(e) => { eprintln!("[build] get_role failed: {e}"); return None; }
    };
    if !matches!(role, Role::MenuItem | Role::CheckMenuItem | Role::RadioMenuItem) {
        return None;
    }

    // GTK4 exposes shortcuts via the ARIA `keyshortcuts` accessible attribute.
    // This is more reliable than ActionProxy.get_key_binding for GTK4 apps.
    let attrs = proxy.get_attributes().await.ok().unwrap_or_default();
    let shortcut_from_attrs = attrs.get("keyshortcuts").and_then(|s| parse_primary_shortcut(s));

    // Item name: GTK4 GtkModelButton stores the label on a child Panel (a GTK4
    // accessibility bug — text is not propagated to accessible name). Try the
    // normal paths; if all fail, allow an empty name so we can still show the
    // shortcut from the attributes.
    let name = proxy.name().await.ok().unwrap_or_default();
    let name = if !name.is_empty() {
        name
    } else {
        child_name(conn, &sender, &path).await.unwrap_or_default()
    };

    // We need at least a shortcut or a name to produce a useful event.
    if name.is_empty() && shortcut_from_attrs.is_none() {
        return None;
    }

    let label_path = if name.is_empty() {
        vec![]
    } else {
        walk_menu_path(conn, &sender, &path, name).await
    };

    // Prefer attrs shortcut (free, already fetched) over an extra ActionProxy roundtrip.
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

/// Walks up the accessibility parent chain collecting menu-segment names
/// until a MenuBar, Frame, Window, or Application root is reached.
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

/// Parses the primary shortcut from a GTK4 ARIA `keyshortcuts` attribute.
/// Format is space-separated alternatives: "Control+N Alt+n" — we take the first.
fn parse_primary_shortcut(s: &str) -> Option<String> {
    s.split_whitespace().next().filter(|t| !t.is_empty()).map(str::to_string)
}

/// Reads the keyboard shortcut from the AT-SPI2 Action interface (fallback for non-GTK4).
/// The AT-SPI2 format is "mnemonic;sequence;shortcut" — we want the third field.
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

/// Gets the application name from the root Application accessible object,
/// lowercased so it matches the executable names in the shortcut database.
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

/// Walks the subtree of a MenuItem up to `depth` levels deep, trying
/// `name()`, the AT-SPI2 Text interface, and description() on each node.
/// GTK4 GtkModelButton stores the visible label on a grandchild Label.
async fn child_name(conn: &Connection, sender: &str, path: &str) -> Option<String> {
    subtree_text(conn, sender, path, 3).await
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
    eprintln!("[child_name] depth={depth} sender={sender} path={path} count={count}");

    for i in 0..count.min(8) {
        let child_ref = match proxy.get_child_at_index(i).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        let c_sender = match child_ref.name_as_str() { Some(s) => s.to_string(), None => continue };
        let c_path   = child_ref.path_as_str().to_string();

        if let Some(child) = accessible(conn, &c_sender, &c_path).await {
            let child_role = child.get_role().await.ok();
            let name  = child.name().await.ok().unwrap_or_default();
            let desc  = child.description().await.ok().unwrap_or_default();
            let attrs = child.get_attributes().await.ok().unwrap_or_default();
            eprintln!("[child_name]   child[{i}] role={child_role:?} name={name:?} desc={desc:?} attrs={attrs:?}");

            if !name.is_empty()  { return Some(name); }
            if !desc.is_empty()  { return Some(desc); }

            // AT-SPI2 Text interface (GtkLabel stores text here, not as accessible name)
            if let Ok(text) = text_content(conn, &c_sender, &c_path).await {
                eprintln!("[child_name]   child[{i}] text={text:?}");
                if !text.is_empty() { return Some(text); }
            }

            // Recurse into deeper children
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

/// Convenience: build an `AccessibleProxy` from a D-Bus sender name and object path.
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
