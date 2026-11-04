//! Linux tray via StatusNotifierItem.
//!
//! tray-icon on Linux never emits click or double-click. A left click arrives
//! as Activate, which opens the settings window. Right click still shows the menu.

use crate::icon::{self, GreenDot};
use crate::UserEvent;
use tao::event_loop::EventLoopProxy;

pub struct LinuxTray {
    pub proxy: EventLoopProxy<UserEvent>,
    pub connected: bool,
    pub being_controlled: bool,
    pub blink_on: bool,
}

impl LinuxTray {
    fn dot(&self) -> GreenDot {
        if !self.connected {
            GreenDot::Off
        } else if self.being_controlled && !self.blink_on {
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

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.proxy.send_event(UserEvent::ShowSettings);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let sharing = crate::ipc_client::get_status()
            .ok()
            .and_then(|s| s.get("sharingEnabled").and_then(|v| v.as_bool()))
            .unwrap_or(true);
        let pause_label = if sharing {
            crate::locale::pause_sharing()
        } else {
            crate::locale::resume_sharing()
        };
        vec![
            item(crate::locale::settings(), UserEvent::ShowSettings),
            item(crate::locale::paste(), UserEvent::PasteHere),
            item(pause_label, UserEvent::ToggleSharing),
            item(crate::locale::quit(), UserEvent::Quit),
        ]
    }
}

fn item(label: &str, event: UserEvent) -> ksni::MenuItem<LinuxTray> {
    ksni::menu::StandardItem {
        label: label.into(),
        activate: Box::new(move |tray: &mut LinuxTray| {
            let _ = tray.proxy.send_event(event.clone());
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
