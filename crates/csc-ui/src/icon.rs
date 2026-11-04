//! Load tray/window icon from `resources/app.png` (embedded at build time).
//! Connected peers get a green status dot in the bottom-right of the tray icon.

const ICON_PNG: &[u8] = include_bytes!("../../../resources/app.png");

/// Green corner-dot for the tray: off when disconnected; solid when linked;
/// blink phase is toggled by the status poller while this computer is controlled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GreenDot {
    Off,
    On,
}

pub fn app_png_bytes() -> &'static [u8] {
    ICON_PNG
}

#[cfg(not(target_os = "linux"))]
pub fn app_tray_icon(dot: GreenDot) -> tray_icon::Icon {
    /// Preferred tray size on Windows (sharp at 100%/125% DPI).
    const TRAY_SIZE: u32 = 32;
    match tray_rgba(TRAY_SIZE, dot) {
        Ok((rgba, w, h)) => {
            tray_icon::Icon::from_rgba(rgba, w, h).unwrap_or_else(|_| fallback_icon(dot))
        }
        Err(_) => fallback_icon(dot),
    }
}

pub fn app_window_icon() -> Option<tao::window::Icon> {
    let (rgba, w, h) = load_rgba(256).ok()?;
    tao::window::Icon::from_rgba(rgba, w, h).ok()
}

pub(crate) fn load_rgba(size: u32) -> Result<(Vec<u8>, u32, u32), ()> {
    tray_rgba(size, GreenDot::Off)
}

/// RGBA pixels with an optional solid green status dot in the bottom-right.
pub(crate) fn tray_rgba(size: u32, dot: GreenDot) -> Result<(Vec<u8>, u32, u32), ()> {
    let img = image::load_from_memory(ICON_PNG).map_err(|_| ())?;
    let mut rgba = img
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    if matches!(dot, GreenDot::On) {
        paint_green_dot(&mut rgba);
    }
    let (w, h) = rgba.dimensions();
    Ok((rgba.into_raw(), w, h))
}

fn paint_green_dot(rgba: &mut image::RgbaImage) {
    let (w, h) = rgba.dimensions();
    if w < 8 || h < 8 {
        return;
    }
    // ~22% of the edge, inset 1 px so the panel does not clip it.
    let r = ((w.min(h) as f32) * 0.22).round().max(3.0) as i32;
    let cx = w as i32 - r - 1;
    let cy = h as i32 - r - 1;
    let r2 = r * r;
    let rim2 = (r - 1).max(1) * (r - 1).max(1);
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = dx * dx + dy * dy;
            if d2 > r2 {
                continue;
            }
            let x = cx + dx;
            let y = cy + dy;
            if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                continue;
            }
            let px = rgba.get_pixel_mut(x as u32, y as u32);
            // Soft white rim so the green reads on light and dark panels.
            if d2 > rim2 {
                px.0 = [0xff, 0xff, 0xff, 0xff];
            } else {
                px.0 = [0x22, 0xc5, 0x5e, 0xff];
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn fallback_icon(dot: GreenDot) -> tray_icon::Icon {
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
    if matches!(dot, GreenDot::On) {
        let mut img = image::RgbaImage::from_raw(W, H, rgba).expect("fallback rgba");
        paint_green_dot(&mut img);
        rgba = img.into_raw();
    }
    tray_icon::Icon::from_rgba(rgba, W, H).expect("fallback tray icon")
}
