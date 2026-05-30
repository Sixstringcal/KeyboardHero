#![cfg(target_os = "linux")]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use kbhero_core::types::ShortcutMatch;

/// Sent from the tokio background thread to the GTK main thread.
pub struct CalloutMsg {
    pub m:           ShortcutMatch,
    pub duration_ms: u32,
}

// Safety: ShortcutMatch contains only owned Strings and a Copy enum.
// The Sender end lives on the background thread; Receiver stays on the main thread.
unsafe impl Send for CalloutMsg {}

const CSS: &str = "
window.kbhero-callout {
    background-color: rgba(44, 44, 46, 0.92);
    border-radius: 10px;
    border: 1px solid rgba(255, 255, 255, 0.12);
}
.kbhero-box {
    padding: 12px 22px;
}
.kbhero-keys {
    color: #F2F2F7;
    font-size: 18pt;
    font-weight: bold;
    font-family: monospace;
    letter-spacing: 0.04em;
}
.kbhero-action {
    color: rgba(242, 242, 247, 0.55);
    font-size: 10pt;
    margin-top: 3px;
}
";

/// Initialize GTK4 and run the main loop.  Blocks until the process is killed.
///
/// `rx` delivers `CalloutMsg` values produced by the resolver on a background
/// thread.  We poll it every 16 ms via a glib timeout source — latency is fine
/// for a visual hint, and this avoids the thread-safety constraints of GTK
/// signal emission from outside the main loop.
pub fn run(rx: mpsc::Receiver<CalloutMsg>) {
    gtk4::init().expect("GTK4 init failed");

    let provider = gtk4::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let active: Rc<RefCell<Option<gtk4::Window>>> = Rc::new(RefCell::new(None));
    let provider = Rc::new(provider);

    // Wrap the receiver so the closure can be 'static
    let rx = Rc::new(RefCell::new(rx));

    let active_poll  = Rc::clone(&active);
    let provider_poll = Rc::clone(&provider);
    let rx_poll      = Rc::clone(&rx);

    glib::timeout_add_local(Duration::from_millis(16), move || {
        loop {
            match rx_poll.borrow().try_recv() {
                Ok(msg) => {
                    if let Some(old) = active_poll.borrow_mut().take() {
                        old.close();
                    }
                    let win = build_callout(&provider_poll, &msg);
                    let win_dismiss  = win.clone();
                    let active_close = Rc::clone(&active_poll);
                    glib::timeout_add_local_once(
                        Duration::from_millis(msg.duration_ms as u64),
                        move || {
                            win_dismiss.close();
                            *active_close.borrow_mut() = None;
                        },
                    );
                    *active_poll.borrow_mut() = Some(win);
                }
                Err(mpsc::TryRecvError::Empty)        => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return glib::ControlFlow::Break;
                }
            }
        }
        glib::ControlFlow::Continue
    });

    glib::MainLoop::new(None, false).run();
}

fn build_callout(_provider: &gtk4::CssProvider, msg: &CalloutMsg) -> gtk4::Window {
    let win = gtk4::Window::new();
    win.set_decorated(false);
    win.set_resizable(false);
    win.set_deletable(false);
    win.add_css_class("kbhero-callout");

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    outer.add_css_class("kbhero-box");

    let keys = gtk4::Label::new(Some(&msg.m.shortcut_keys));
    keys.add_css_class("kbhero-keys");
    outer.append(&keys);

    if !msg.m.action_name.is_empty() {
        let action = gtk4::Label::new(Some(&msg.m.action_name));
        action.add_css_class("kbhero-action");
        outer.append(&action);
    }

    win.set_child(Some(&outer));
    win.present();
    win
}
