// Diagnostic tool: dumps ALL AT-SPI2 events to stderr with no filtering.
// Run this, interact with any app, and observe what comes through.
// Usage: kbhero-debug

use atspi::{
    events::{Event, EventProperties},
    AccessibilityConnection,
};
use futures_lite::StreamExt;

#[tokio::main]
async fn main() {
    let conn = AccessibilityConnection::new()
        .await
        .expect("could not connect to AT-SPI2 bus");

    // Register for every object event type
    conn.register_event::<atspi::events::object::StateChangedEvent>().await.unwrap();
    conn.register_event::<atspi::events::object::ChildrenChangedEvent>().await.unwrap();
    conn.register_event::<atspi::events::object::SelectionChangedEvent>().await.unwrap();
    conn.register_event::<atspi::events::object::ActiveDescendantChangedEvent>().await.unwrap();
    conn.register_event::<atspi::events::focus::FocusEvent>().await.unwrap();

    eprintln!("Connected. Interact with any open app window — ALL events will print here.");
    eprintln!("Press Ctrl-C to quit.\n");

    let zbus = conn.inner().connection().clone();
    let mut stream = conn.event_stream();

    while let Some(result) = stream.next().await {
        let ev = match result {
            Ok(e) => e,
            Err(e) => { eprintln!("err: {e}"); continue; }
        };

        let (kind, sender, path) = match &ev {
            Event::Object(o) => {
                use atspi::events::ObjectEvents::*;
                let kind = match o {
                    StateChanged(e) => format!("StateChanged({:?}={:?})", e.state, e.enabled),
                    ChildrenChanged(e) => format!("ChildrenChanged({:?})", e.operation),
                    SelectionChanged(_) => "SelectionChanged".into(),
                    ActiveDescendantChanged(_) => "ActiveDescendantChanged".into(),
                    _ => "Object(other)".into(),
                };
                (kind, ev.sender().to_string(), ev.path().to_string())
            }
            Event::Focus(_) => ("Focus".into(), ev.sender().to_string(), ev.path().to_string()),
            _ => continue,
        };

        // Quick role lookup (best-effort, skip if slow)
        let role_str = {
            let s = sender.clone();
            let p = path.clone();
            match atspi::proxy::accessible::AccessibleProxy::builder(&zbus)
                .destination(s.as_str()).ok()
                .and_then(|b| b.path(p.as_str()).ok())
                .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
            {
                Some(b) => match tokio::time::timeout(
                    std::time::Duration::from_millis(50),
                    b.build()
                ).await {
                    Ok(Ok(p)) => match tokio::time::timeout(
                        std::time::Duration::from_millis(50),
                        p.get_role()
                    ).await {
                        Ok(Ok(r)) => format!("{r:?}"),
                        _ => "?".into(),
                    },
                    _ => "?".into(),
                },
                None => "?".into(),
            }
        };

        eprintln!("{:<45} sender={:<8} role={:<20} path={}", kind, sender, role_str, path);
    }
}
