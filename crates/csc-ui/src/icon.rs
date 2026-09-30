//! Load tray/window icon from `resources/app.png` (embedded at build time).

const ICON_PNG: &[u8] = include_bytes!("../../../resources/app.png");

pub fn app_png_bytes() -> &'static [u8] {
    ICON_PNG
}

/// Preferred tray size on Windows (sharp at 100%/125% DPI).
const TRAY_SIZE: u32 = 32;

#[cfg(not(target_os = "linux"))]
pub fn app_tray_icon() -> tray_icon::Icon {
    match load_rgba(TRAY_SIZE) {
        Ok((rgba, w, h)) => {
            tray_icon::Icon::from_rgba(rgba, w, h).unwrap_or_else(|_| fallback_icon())
        }
        Err(_) => fallback_icon(),
    }
}

pub fn app_window_icon() -> Option<tao::window::Icon> {
    let (rgba, w, h) = load_rgba(256).ok()?;
    tao::window::Icon::from_rgba(rgba, w, h).ok()
}

pub(crate) fn load_rgba(size: u32) -> Result<(Vec<u8>, u32, u32), ()> {
    let img = image::load_from_memory(ICON_PNG).map_err(|_| ())?;
    let rgba = img
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

#[cfg(not(target_os = "linux"))]
fn fallback_icon() -> tray_icon::Icon {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let edge = x < 2 || y < 2 || x >= W - 2 || y >= H - 2;
            if edge {
                rgba[i] = 0x25;
                rgba[i + 1] = 0x63;
                rgba[i + 2] = 0xeb;
                rgba[i + 3] = 0x00;
            } else {
                rgba[i] = 0x3b;
                rgba[i + 1] = 0x82;
                rgba[i + 2] = 0xf6;
                rgba[i + 3] = 0xff;
            }
        }
    }
    tray_icon::Icon::from_rgba(rgba, W, H).expect("fallback tray icon")
}
