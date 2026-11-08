//! Linux tray via StatusNotifierItem.
//!
//! Menu actions that must work while the settings window is minimized run here
//! on the tray/dbus thread — the egui loop may be asleep and never drain events.

use crate::events::EventBus;
use crate::icon::{self, GreenDot};

pub struct LinuxTray {
    pub bus: EventBus,
    pub connected: bool,
    pub being_controlled: bool,
    pub blink_on: bool,
}

impl LinuxTray {
    fn dot(&self) -> GreenDot {
        if !self.connected || (self.being_controlled && !self.blink_on) {
            GreenDot::Off
        } else {
            GreenDot::On
        }
    }
}

impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "cross-systemconnect".into()
    }

    fn title(&self) -> String {
        crate::app_title()
    }

    fn status(&self) -> ksni::Status {
        // Passive items may be hidden by GNOME AppIndicator.
        ksni::Status::Active
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let Some((w, h, data)) = argb_icon(32, self.dot()) else {
            return Vec::new();
        };
        vec![ksni::Icon {
            width: w,
            height: h,
            data,
        }]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let en = crate::locale::is_english();
        let description = if !self.connected {
            if en {
                "Not connected"
            } else {
                "未连接"
            }
        } else if self.being_controlled {
            if en {
                "Being controlled"
            } else {
                "被控中"
            }
        } else if en {
            "Connected"
        } else {
            "已连接"
        }
        .to_string();
        ksni::ToolTip {
            title: crate::app_title(),
            description,
            icon_pixmap: self.icon_pixmap(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.bus.show_settings_now();
    }

    fn secondary_activate(&mut self, x: i32, y: i32) {
        self.activate(x, y);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![settings_item(), quit_item()]
    }
}

/// Start the StatusNotifierItem on a dedicated thread (no unwrap panic on dbus errors).
pub fn spawn(bus: EventBus) -> Option<ksni::Handle<LinuxTray>> {
    let service = ksni::TrayService::new(LinuxTray {
        bus,
        connected: false,
        being_controlled: false,
        blink_on: true,
    });
    let handle = service.handle();
    match std::thread::Builder::new()
        .name("csc-linux-tray".into())
        .spawn(move || {
            if let Err(e) = service.run() {
                tracing::error!("linux tray stopped: {e}");
            }
        }) {
        Ok(_) => Some(handle),
        Err(e) => {
            tracing::error!("failed to spawn linux tray thread: {e}");
            None
        }
    }
}

fn settings_item() -> ksni::MenuItem<LinuxTray> {
    ksni::menu::StandardItem {
        label: crate::locale::settings().into(),
        activate: Box::new(|tray: &mut LinuxTray| {
            tray.bus.show_settings_now();
        }),
        ..Default::default()
    }
    .into()
}

fn quit_item() -> ksni::MenuItem<LinuxTray> {
    ksni::menu::StandardItem {
        label: crate::locale::quit().into(),
        activate: Box::new(|_tray: &mut LinuxTray| {
            // Must not call blocking quit_core() on the dbus/tray thread — a hung
            // IPC wait would swallow Quit. force_quit_now always ends the process.
            crate::singleton::release();
            crate::ipc_client::force_quit_now();
        }),
        ..Default::default()
    }
    .into()
}

fn argb_icon(size: u32, dot: GreenDot) -> Option<(i32, i32, Vec<u8>)> {
    let (rgba, w, h) = icon::tray_rgba(size, dot).ok()?;
    let mut argb = Vec::with_capacity(rgba.len());
    for &[r, g, b, a] in rgba.as_chunks::<4>().0 {
        argb.extend_from_slice(&[a, r, g, b]);
    }
    Some((w as i32, h as i32, argb))
}
