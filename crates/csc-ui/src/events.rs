//! Cross-thread events into the egui App (tray, status poller, popup watcher).

use eframe::egui::{self, Context, ViewportCommand};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub enum UiEvent {
    ShowSettings,
    Transfers(Vec<Value>),
    TrayLink {
        connected: bool,
        being_controlled: bool,
        blink_on: bool,
    },
}

#[derive(Clone)]
pub struct EventBus {
    tx: Sender<UiEvent>,
    /// Wake the egui loop when events arrive while the window is hidden.
    ctx: Arc<Mutex<Option<Context>>>,
    /// Set by tray before the UI thread drains events (minimized loop may lag).
    show_requested: Arc<AtomicBool>,
}

impl EventBus {
    pub fn new() -> (Self, Receiver<UiEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                tx,
                ctx: Arc::new(Mutex::new(None)),
                show_requested: Arc::new(AtomicBool::new(false)),
            },
            rx,
        )
    }

    pub fn bind_context(&self, ctx: Context) {
        if let Ok(mut slot) = self.ctx.lock() {
            *slot = Some(ctx);
        }
    }

    pub fn send(&self, ev: UiEvent) {
        let _ = self.tx.send(ev);
        self.wake();
    }

    /// Open settings from a non-UI thread (tray). Issue viewport commands immediately
    /// so GNOME Wayland restores a minimized window even if the egui loop is asleep.
    pub fn show_settings_now(&self) {
        self.show_requested.store(true, Ordering::SeqCst);
        let _ = self.tx.send(UiEvent::ShowSettings);
        if let Ok(guard) = self.ctx.lock() {
            if let Some(ctx) = guard.as_ref() {
                ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
                ctx.request_repaint();
            }
        }
    }

    pub fn take_show_requested(&self) -> bool {
        self.show_requested.swap(false, Ordering::SeqCst)
    }

    pub fn wake(&self) {
        if let Ok(guard) = self.ctx.lock() {
            if let Some(ctx) = guard.as_ref() {
                ctx.request_repaint();
            }
        }
    }
}
