//! Application-center visual tokens. Dark is the default; light can be chosen in settings.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    Style, TextStyle, Visuals,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub sidebar: Color32,
    pub card: Color32,
    pub card_hover: Color32,
    pub input: Color32,
    pub text: Color32,
    pub text_2: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub accent: Color32,
    #[allow(dead_code)] // reserved for pressed/hover accents
    pub accent_hover: Color32,
    pub accent_soft: Color32,
    pub ok: Color32,
    pub ok_soft: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub on_accent: Color32,
    pub dark: bool,
    pub faint: Color32,
    pub hover_well: Color32,
}

impl Palette {
    /// The palette for the theme currently applied to the window.
    pub fn app() -> Self {
        current()
    }

    pub fn named(name: &str) -> Self {
        if name.eq_ignore_ascii_case("light") {
            Self::light()
        } else {
            Self::dark()
        }
    }

    /// Cursor-style settings: near-black canvas, raised cards, blue accent.
    pub fn dark() -> Self {
        Self {
            bg: Color32::from_rgb(0x14, 0x14, 0x14),
            sidebar: Color32::from_rgb(0x1a, 0x1a, 0x1a),
            card: Color32::from_rgb(0x21, 0x21, 0x21),
            card_hover: Color32::from_rgb(0x2a, 0x2a, 0x2a),
            // Recessed field well — darker than the card face.
            input: Color32::from_rgb(0x16, 0x16, 0x16),
            text: Color32::from_rgb(0xe8, 0xe8, 0xe8),
            text_2: Color32::from_rgb(0x8b, 0x8b, 0x8b),
            border: Color32::from_rgb(0x3a, 0x3a, 0x3a),
            border_strong: Color32::from_rgb(0x4a, 0x4a, 0x4a),
            accent: Color32::from_rgb(0x3b, 0x82, 0xf6),
            accent_hover: Color32::from_rgb(0x25, 0x6e, 0xeb),
            accent_soft: Color32::from_rgba_unmultiplied(0x3b, 0x82, 0xf6, 36),
            ok: Color32::from_rgb(0x22, 0xc5, 0x5e),
            ok_soft: Color32::from_rgba_unmultiplied(0x22, 0xc5, 0x5e, 40),
            warn: Color32::from_rgb(0xe0, 0xa2, 0x3b),
            danger: Color32::from_rgb(0xf0, 0x6a, 0x5d),
            on_accent: Color32::WHITE,
            dark: true,
            faint: Color32::from_rgb(0x28, 0x28, 0x28),
            hover_well: Color32::from_rgb(0x1c, 0x1c, 0x1c),
        }
    }

    pub fn light() -> Self {
        Self {
            bg: Color32::from_rgb(0xf5, 0xf6, 0xf8),
            sidebar: Color32::from_rgb(0xee, 0xef, 0xf2),
            card: Color32::from_rgb(0xff, 0xff, 0xff),
            card_hover: Color32::from_rgb(0xf0, 0xf2, 0xf5),
            input: Color32::from_rgb(0xee, 0xf0, 0xf3),
            text: Color32::from_rgb(0x1f, 0x23, 0x29),
            text_2: Color32::from_rgb(0x5c, 0x63, 0x70),
            border: Color32::from_rgb(0xdd, 0xe1, 0xe6),
            border_strong: Color32::from_rgb(0xc5, 0xca, 0xd1),
            accent: Color32::from_rgb(0x3b, 0x82, 0xf6),
            accent_hover: Color32::from_rgb(0x25, 0x6e, 0xeb),
            accent_soft: Color32::from_rgba_unmultiplied(0x3b, 0x82, 0xf6, 48),
            ok: Color32::from_rgb(0x1f, 0x9d, 0x55),
            ok_soft: Color32::from_rgba_unmultiplied(0x1f, 0x9d, 0x55, 40),
            warn: Color32::from_rgb(0xb7, 0x79, 0x1f),
            danger: Color32::from_rgb(0xd2, 0x3f, 0x31),
            on_accent: Color32::WHITE,
            dark: false,
            faint: Color32::from_rgb(0xe8, 0xea, 0xed),
            hover_well: Color32::from_rgb(0xe4, 0xe6, 0xea),
        }
    }
}

static DARK: AtomicBool = AtomicBool::new(true);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

pub fn current() -> Palette {
    if is_dark() {
        Palette::dark()
    } else {
        Palette::light()
    }
}

pub fn install(ctx: &egui::Context) {
    install_cjk_font(ctx);
    apply(ctx, "dark");
}

/// Apply `dark` or `light`. Unknown values fall back to dark.
pub fn apply(ctx: &egui::Context, name: &str) {
    let p = Palette::named(name);
    DARK.store(p.dark, Ordering::Relaxed);
    apply_style(ctx, p);
    csc_platform::set_popup_menu_dark(p.dark);
}

/// Re-apply only when the saved theme does not match the window.
pub fn sync(ctx: &egui::Context, name: &str) {
    if Palette::named(name).dark != is_dark() {
        apply(ctx, name);
    }
}

pub fn apply_style(ctx: &egui::Context, p: Palette) {
    let mut style = Style::default();
    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.window_margin = Margin::same(0);
    style.spacing.button_padding = egui::vec2(12.0, 7.0);
    style.spacing.interact_size = egui::vec2(40.0, 32.0);
    style.spacing.indent = 14.0;
    style.interaction.selectable_labels = false;

    // Body/Button 14 + interact 32 → ComboBox / DragValue 与表单行同高。
    style.text_styles = [
        (TextStyle::Small, FontId::proportional(13.0)),
        (TextStyle::Body, FontId::proportional(14.0)),
        (TextStyle::Button, FontId::proportional(14.0)),
        (TextStyle::Heading, FontId::proportional(20.0)),
        (TextStyle::Monospace, FontId::monospace(14.0)),
    ]
    .into();

    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.dark_mode = p.dark;
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = p.input;
    v.text_edit_bg_color = Some(p.input);
    v.faint_bg_color = p.faint;
    v.code_bg_color = p.input;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_stroke = Stroke::new(1.0, p.border);
    // Inset rounded fields (ports / text / combos).
    let field_r = CornerRadius::same(10);
    v.widgets.noninteractive.bg_fill = p.card;
    v.widgets.noninteractive.weak_bg_fill = p.sidebar;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text_2);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.corner_radius = field_r;
    v.widgets.inactive.bg_fill = p.input;
    v.widgets.inactive.weak_bg_fill = p.input;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.corner_radius = field_r;
    v.widgets.hovered.bg_fill = p.input;
    v.widgets.hovered.weak_bg_fill = p.hover_well;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    v.widgets.hovered.corner_radius = field_r;
    v.widgets.active.bg_fill = p.input;
    v.widgets.active.weak_bg_fill = p.input;
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.corner_radius = field_r;
    v.widgets.open.bg_fill = p.input;
    v.widgets.open.weak_bg_fill = p.input;
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.open.corner_radius = field_r;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.button_frame = true;

    style.visuals = v;
    ctx.set_theme(if p.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

fn font_candidates() -> Vec<(std::path::PathBuf, u32)> {
    let mut v = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push((dir.join("assets").join("font.ttf"), 0));
        }
    }
    if cfg!(windows) {
        let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        let fonts = std::path::Path::new(&windir).join("Fonts");
        v.push((fonts.join("msyh.ttc"), 0));
        v.push((fonts.join("segoeui.ttf"), 0));
        v.push((fonts.join("simhei.ttf"), 0));
    } else {
        for p in [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        ] {
            v.push((p.into(), if p.contains("CJK") { 2 } else { 0 }));
        }
        v.push((
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf".into(),
            0,
        ));
        v.push(("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".into(), 0));
    }
    v
}

fn install_cjk_font(ctx: &egui::Context) -> Option<String> {
    for (path, index) in font_candidates() {
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let mut fonts = FontDefinitions::default();
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        fonts.font_data.insert("cjk".into(), Arc::new(data));
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts.families.entry(fam).or_default().push("cjk".into());
        }
        ctx.set_fonts(fonts);
        tracing::info!("CJK font: {} (index {index})", path.display());
        return Some(path.display().to_string());
    }
    tracing::warn!("no CJK font found; Chinese text may not render");
    None
}
