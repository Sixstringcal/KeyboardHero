// Linux detection engine — Milestone 1 (AT-SPI2 over D-Bus).
// Stub only: the trait impl will be added in the Linux alpha milestone.

use kbhero_core::types::{AppIdentity, ElementRole, RawActivationEvent};

pub struct LinuxDetectionEngine;

impl LinuxDetectionEngine {
    pub fn new() -> Self {
        Self
    }
}

/// Placeholder until the AT-SPI2 backend is wired up.
/// Returns a hard-coded test event so the daemon can be exercised end-to-end
/// on Linux before real event detection is implemented.
pub async fn next_event_stub() -> Option<RawActivationEvent> {
    use std::time::Instant;
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    Some(RawActivationEvent {
        timestamp:           Instant::now(),
        app:                 AppIdentity {
            executable:   "stub".to_string(),
            window_title: None,
            pid:          0,
        },
        role:                ElementRole::MenuItem,
        label_path:          vec!["Edit".to_string(), "Copy".to_string()],
        discovered_shortcut: None,
    })
}
