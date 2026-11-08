//! Application-center visual tokens — dark theme（对齐 Doc/界面设计.md §4 深色）。

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke,
    Style, TextStyle, Visuals,
};
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
    pub accent_hover: Color32,
    pub accent_soft: Color32,
    pub ok: Color32,
    pub ok_soft: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub on_accent: Color32,
}

impl Palette {
    /// 应用中心暗黑：深灰底、卡片面、蓝强调（界面设计 §4.1 深色）。
    pub fn app() -> Self {
        Self {
            bg: Color32::from_rgb(0x1e, 0x1f, 0x22),
            sidebar: Color32::from_rgb(0x2b, 0x2d, 0x31),
            card: Color32::from_rgb(0x2b, 0x2d, 0x31),
            card_hover: Color32::from_rgb(0x35, 0x38, 0x3e),
            input: Color32::from_rgb(0x24, 0x26, 0x2a),
            text: Color32::from_rgb(0xe6, 0xe7, 0xea),
            text_2: Color32::from_rgb(0xa3, 0xa8, 0xb3),
            border: Color32::from_rgb(0x3a, 0x3d, 0x44),
            border_strong: Color32::from_rgb(0x4a, 0x4e, 0x56),
            accent: Color32::from_rgb(0x5b, 0x8f, 0xf9),
            accent_hover: Color32::from_rgb(0x4a, 0x7e, 0xe8),
            accent_soft: Color32::from_rgba_unmultiplied(0x5b, 0x8f, 0xf9, 40),
            ok: Color32::from_rgb(0x3c, 0xc2, 0x7a),
            ok_soft: Color32::from_rgba_unmultiplied(0x3c, 0xc2, 0x7a, 40),
            warn: Color32::from_rgb(0xe0, 0xa2, 0x3b),
            danger: Color32::from_rgb(0xf0, 0x6a, 0x5d),
            on_accent: Color32::WHITE,
        }
    }
}

pub fn install(ctx: &egui::Context) {
    install_cjk_font(ctx);
    apply_style(ctx, Palette::app());
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

    let mut v = Visuals::dark();
    v.dark_mode = true;
    v.override_text_color = Some(p.text);
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.extreme_bg_color = p.input;
    v.faint_bg_color = Color32::from_rgb(0x28, 0x2a, 0x2e);
    v.code_bg_color = p.input;
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(8);
    v.window_stroke = Stroke::new(1.0, p.border);
    v.widgets.noninteractive.bg_fill = p.card;
    v.widgets.noninteractive.weak_bg_fill = p.sidebar;
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text_2);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.bg_fill = p.card;
    v.widgets.inactive.weak_bg_fill = p.input;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
    v.widgets.inactive.corner_radius = CornerRadius::same(6);
    v.widgets.hovered.bg_fill = p.card_hover;
    v.widgets.hovered.weak_bg_fill = p.card_hover;
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.border_strong);
    v.widgets.hovered.corner_radius = CornerRadius::same(6);
    v.widgets.active.bg_fill = p.accent_hover;
    v.widgets.active.weak_bg_fill = p.accent;
    v.widgets.active.fg_stroke = Stroke::new(1.0, p.on_accent);
    v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.active.corner_radius = CornerRadius::same(6);
    v.widgets.open.bg_fill = p.card;
    v.widgets.open.fg_stroke = Stroke::new(1.0, p.text);
    v.widgets.open.bg_stroke = Stroke::new(1.0, p.accent);
    v.widgets.open.corner_radius = CornerRadius::same(6);
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.button_frame = true;

    style.visuals = v;
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
}

pub fn section_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(18, 16))
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
