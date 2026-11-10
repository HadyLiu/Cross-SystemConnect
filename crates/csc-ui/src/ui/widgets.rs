//! Shared chrome — application-center style.

use crate::theme::Palette;
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke, Vec2};

pub fn hint(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).size(FORM_FONT - 1.0).color(p.text_2));
}

/// Form metrics: label / control share one font size and one row height.
pub const FORM_FONT: f32 = 14.0;
pub const FORM_FIELD_W: f32 = 152.0;
pub const FORM_ROW_H: f32 = 34.0;
pub const FIELD_RADIUS: u8 = 10;

/// Field width capped by remaining row space (avoids column overflow / overlap).
pub fn field_width(ui: &egui::Ui) -> f32 {
    ui.available_width().clamp(64.0, FORM_FIELD_W)
}

/// Cursor-style section title above a settings card.
pub fn section_heading(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title).size(15.0).strong().color(p.text));
    ui.add_space(10.0);
}

/// Outlined action button (Open / Show / Import style).
pub fn outline_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(12.5).color(p.text))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::new(1.0, p.border_strong))
            .corner_radius(6.0)
            .min_size(egui::vec2(64.0, 28.0)),
    )
}

/// Cursor-style settings card: rounded panel of title/desc rows + right controls.
pub fn settings_card(ui: &mut egui::Ui, p: &Palette, add: impl FnOnce(&mut SettingsList<'_, '_>)) {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(10))
        .show(ui, |ui| {
            let mut list = SettingsList { ui, p, rows: 0 };
            add(&mut list);
        });
    ui.add_space(20.0);
}

/// Card with custom body (topology, logs) — same shell, padded content.
pub fn settings_card_body(ui: &mut egui::Ui, p: &Palette, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::NONE)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| add(ui));
    ui.add_space(20.0);
}

pub struct SettingsList<'ui, 'p> {
    ui: &'ui mut egui::Ui,
    p: &'p Palette,
    rows: usize,
}

impl SettingsList<'_, '_> {
    /// Title + muted description on the left; control(s) on the right.
    pub fn row(&mut self, title: &str, desc: &str, add: impl FnOnce(&mut egui::Ui)) {
        if self.rows > 0 {
            let full = self.ui.max_rect();
            let y = self.ui.cursor().top();
            self.ui.painter().hline(
                egui::Rangef::new(full.left() + 16.0, full.right() - 16.0),
                y,
                Stroke::new(1.0, self.p.border),
            );
        }
        self.rows += 1;

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 13))
            .show(self.ui, |ui| {
                let p = *self.p;
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        // Leave room for a field + outline button (pairing code row).
                        let text_w = (ui.available_width() - 250.0).max(120.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(text_w, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.label(RichText::new(title).size(13.5).color(p.text));
                                if !desc.is_empty() {
                                    ui.add_space(3.0);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(desc).size(12.0).color(p.text_2),
                                        )
                                        .wrap(),
                                    );
                                }
                            },
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        add(ui);
                    });
                });
            });
    }
}

/// Centered inset number field. Type to edit — no left/right drag scrubbing.
pub fn mac_number<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
) -> egui::Response {
    mac_number_with(ui, value, range, "")
}

/// Like [`mac_number`], with a display suffix (e.g. `" MiB/s"`).
pub fn mac_number_with<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    suffix: &str,
) -> egui::Response {
    number_field(ui, value, range, suffix, None)
}

/// Like [`mac_number`], always shown with `decimals` digits (e.g. `1.0`).
pub fn mac_number_fixed<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    decimals: usize,
) -> egui::Response {
    number_field(ui, value, range, "", Some(decimals))
}

fn number_field<Num: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut Num,
    range: std::ops::RangeInclusive<Num>,
    suffix: &str,
    decimals: Option<usize>,
) -> egui::Response {
    let p = Palette::app();
    let w = field_width(ui);
    let id = ui.next_auto_id().with("mac_number");
    let focused = ui.memory(|m| m.has_focus(id));

    let display = match decimals {
        Some(d) => format!("{:.*}", d, value.to_f64()),
        None => format_number(value.to_f64(), suffix),
    };
    let mut text = if focused {
        ui.data_mut(|d| {
            d.get_temp::<String>(id)
                .unwrap_or_else(|| strip_suffix(&display, suffix))
        })
    } else {
        display
    };

    let stroke = if focused {
        Stroke::new(1.5, p.accent)
    } else {
        Stroke::new(1.0, p.border)
    };
    let frame = egui::Frame::new()
        .fill(p.input)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(FIELD_RADIUS))
        .inner_margin(egui::Margin::symmetric(8, 6));

    let mut response = ui.add_sized(
        egui::vec2(w, FORM_ROW_H),
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .desired_width(w)
            .frame(frame)
            .background_color(p.input)
            .horizontal_align(egui::Align::Center)
            .vertical_align(egui::Align::Center)
            .font(FontId::proportional(FORM_FONT)),
    );

    if focused || response.gained_focus() {
        ui.data_mut(|d| d.insert_temp(id, text.clone()));
    }
    if response.lost_focus() {
        ui.data_mut(|d| d.remove_temp::<String>(id));
    }

    // Commit on Enter / focus lost (not while every keystroke — avoids fighting the caret).
    let commit =
        response.lost_focus() || (focused && ui.input(|i| i.key_pressed(egui::Key::Enter)));
    if commit {
        let raw = strip_suffix(text.trim(), suffix);
        if let Ok(parsed) = raw.parse::<f64>() {
            let lo = range.start().to_f64();
            let hi = range.end().to_f64();
            let clamped = parsed.clamp(lo.min(hi), lo.max(hi));
            let next = Num::from_f64(clamped);
            if next.to_f64() != value.to_f64() {
                *value = next;
                response.mark_changed();
            }
        }
    }

    response
}

fn format_number(v: f64, suffix: &str) -> String {
    let body = if (v.fract().abs() * 1e9).round() < 1.0 {
        format!("{v:.0}")
    } else if (v * 100.0).fract().abs() < 1e-6 {
        format!("{v:.2}")
    } else {
        format!("{v}")
    };
    if suffix.is_empty() {
        body
    } else {
        format!("{body}{suffix}")
    }
}

fn strip_suffix(s: &str, suffix: &str) -> String {
    let t = s.trim();
    if !suffix.is_empty() && t.ends_with(suffix) {
        t[..t.len() - suffix.len()].trim().to_string()
    } else {
        t.to_string()
    }
}

fn paint_inset_field(ui: &mut egui::Ui, p: &Palette, rect: egui::Rect) {
    ui.painter().rect(
        rect,
        CornerRadius::same(FIELD_RADIUS),
        p.input,
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
}

/// Soft toggle (green when on); height matches form row.
pub fn mac_toggle(ui: &mut egui::Ui, p: &Palette, on: &mut bool) -> egui::Response {
    let size = Vec2::new(48.0, FORM_ROW_H - 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    let fill = if *on { p.ok } else { p.border_strong };
    let radius = (rect.height() * 0.5) as u8;
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius), fill);
    let kn = rect.height() * 0.5 - 3.0;
    let kx = if *on {
        rect.right() - kn - 3.0
    } else {
        rect.left() + kn + 3.0
    };
    ui.painter()
        .circle_filled(egui::pos2(kx, rect.center().y), kn, Color32::WHITE);
    resp
}

#[derive(Clone, Copy)]
pub enum NavIcon {
    Devices,
    Sharing,
    Switch,
    System,
    About,
}

/// Soft selected pill (accent wash + accent text) — app-center category row.
pub fn nav_item(
    ui: &mut egui::Ui,
    p: &Palette,
    selected: bool,
    icon: NavIcon,
    label: &str,
) -> egui::Response {
    let desired = Vec2::new(ui.available_width(), 36.0);
    let (rect, resp) = ui.allocate_exact_size(desired, Sense::click());
    let hover = resp.hovered();
    let fill = if selected {
        p.accent_soft
    } else if hover {
        p.card_hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
    let text_color = if selected {
        p.accent
    } else if hover {
        p.text
    } else {
        p.text_2
    };
    let icon_color = text_color;
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 22.0, rect.center().y),
        Vec2::splat(16.0),
    );
    paint_nav_icon(ui.painter(), icon_rect, icon, icon_color, p.sidebar);
    ui.painter().text(
        egui::pos2(rect.left() + 42.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        text_color,
    );
    resp
}

fn paint_nav_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: NavIcon,
    color: Color32,
    fill: Color32,
) {
    let stroke = Stroke::new(1.4, color);
    let c = rect.center();
    match icon {
        NavIcon::Devices => {
            // monitor + base (y.html devices svg)
            let screen =
                egui::Rect::from_center_size(egui::pos2(c.x, c.y - 1.5), Vec2::new(12.0, 8.0));
            painter.rect_stroke(screen, 1.0, stroke, egui::StrokeKind::Outside);
            painter.line_segment(
                [
                    egui::pos2(c.x - 6.0, c.y + 5.5),
                    egui::pos2(c.x + 6.0, c.y + 5.5),
                ],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x, screen.bottom()), egui::pos2(c.x, c.y + 5.5)],
                stroke,
            );
        }
        NavIcon::Sharing => {
            // two stacked docs
            let back =
                egui::Rect::from_min_size(egui::pos2(c.x - 5.5, c.y - 6.5), Vec2::new(9.0, 11.0));
            let front =
                egui::Rect::from_min_size(egui::pos2(c.x - 3.0, c.y - 4.0), Vec2::new(9.0, 11.0));
            painter.rect_stroke(back, 1.0, stroke, egui::StrokeKind::Outside);
            painter.rect_filled(front, 1.0, fill);
            painter.rect_stroke(front, 1.0, stroke, egui::StrokeKind::Outside);
        }
        NavIcon::Switch => {
            // bolt
            let pts = [
                egui::pos2(c.x + 1.5, c.y - 6.5),
                egui::pos2(c.x - 2.5, c.y - 0.5),
                egui::pos2(c.x + 1.0, c.y - 0.5),
                egui::pos2(c.x - 1.5, c.y + 6.5),
                egui::pos2(c.x + 2.5, c.y + 0.5),
                egui::pos2(c.x - 1.0, c.y + 0.5),
            ];
            for w in pts.windows(2) {
                painter.line_segment([w[0], w[1]], stroke);
            }
            painter.line_segment([pts[5], pts[0]], stroke);
        }
        NavIcon::System => {
            // gear: circle + teeth ticks
            painter.circle_stroke(c, 3.2, stroke);
            for i in 0..6 {
                let a = (i as f32) * std::f32::consts::TAU / 6.0;
                let i0 = egui::vec2(a.cos(), a.sin()) * 4.5;
                let i1 = egui::vec2(a.cos(), a.sin()) * 6.8;
                painter.line_segment([c + i0, c + i1], stroke);
            }
        }
        NavIcon::About => {
            painter.circle_stroke(c, 6.5, stroke);
            painter.circle_filled(egui::pos2(c.x, c.y - 3.0), 1.1, color);
            painter.line_segment(
                [egui::pos2(c.x, c.y - 0.5), egui::pos2(c.x, c.y + 3.8)],
                Stroke::new(1.6, color),
            );
        }
    }
}

/// App-center brand header: large logo + name + version.
pub fn brand_block(ui: &mut egui::Ui, p: &Palette, logo: Option<&egui::TextureHandle>) {
    ui.vertical_centered(|ui| {
        if let Some(tex) = logo {
            ui.add(
                egui::Image::new(egui::load::SizedTexture::from_handle(tex))
                    .fit_to_exact_size(Vec2::splat(52.0))
                    .corner_radius(12.0),
            );
        } else {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(52.0), Sense::hover());
            ui.painter()
                .rect_filled(r, CornerRadius::same(12), p.accent);
        }
        ui.add_space(8.0);
        ui.label(
            RichText::new("Cross-SystemConnect")
                .size(13.0)
                .strong()
                .color(p.text),
        );
        ui.label(
            RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                .size(11.0)
                .color(p.text_2),
        );
    });
}

pub fn status_pill(ui: &mut egui::Ui, p: &Palette, ok: bool, label: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (dot, _) = ui.allocate_exact_size(Vec2::splat(6.0), Sense::hover());
        let c = if ok { p.ok } else { p.text_2 };
        ui.painter().circle_filled(dot.center(), 3.0, c);
        if ok {
            ui.painter().circle_stroke(
                dot.center(),
                4.5,
                Stroke::new(1.5, Color32::from_rgba_unmultiplied(0x34, 0xc7, 0x59, 90)),
            );
        }
        ui.label(
            RichText::new(label)
                .size(10.0)
                .color(if ok { p.text } else { p.text_2 }),
        );
    });
}

pub fn ghost_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    outline_button(ui, p, label)
}

pub fn primary_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(FORM_FONT).color(p.on_accent))
            .fill(p.accent)
            .stroke(Stroke::NONE)
            .corner_radius(8.0)
            .min_size(egui::vec2(0.0, FORM_ROW_H)),
    )
}

pub fn danger_text_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(FORM_FONT).color(p.danger))
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(8.0)
            .min_size(egui::vec2(0.0, FORM_ROW_H)),
    )
}

pub fn pairing_code(ui: &mut egui::Ui, p: &Palette, code: &str) {
    let display = format_code(code);
    // Same width as ComboBox / mac_number so the right column lines up.
    let w = field_width(ui).max(FORM_FIELD_W);
    let h = 46.0;
    let size = Vec2::new(w, h);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_inset_field(ui, p, rect);
    // Layout then paint at geometric center (Align2 alone can look top-heavy).
    let galley = ui
        .painter()
        .layout_no_wrap(display, FontId::monospace(28.0), p.accent);
    let pos = rect.center() - galley.size() * 0.5;
    ui.painter().galley(pos, galley, p.accent);
}

pub fn digit_boxes(ui: &mut egui::Ui, p: &Palette, digits: &mut [String; 6]) -> bool {
    let focus_id = ui.id().with("pair_digit_focus");
    let active_id = ui.id().with("pair_digit_active");
    let mut changed = false;
    // Must persist across frames — temp data is wiped each frame and would
    // always snap focus back to box 0 after typing.
    let mut focused: usize =
        ui.memory_mut(|m| m.data.get_persisted::<usize>(focus_id).unwrap_or(0));
    let mut active: bool =
        ui.memory_mut(|m| m.data.get_persisted::<bool>(active_id).unwrap_or(false));

    // Stretch 6 boxes across the full card width with even gaps.
    let total_w = ui.available_width();
    let gap = 8.0_f32;
    let box_w = ((total_w - gap * 5.0) / 6.0).max(28.0);
    let box_h = box_w.clamp(FORM_ROW_H, 48.0);
    let font = FontId::monospace((box_h * 0.48).clamp(FORM_FONT, 22.0));

    // Pass 1: hit-test clicks; Pass 2 draws after keys so focus ring advances same frame.
    let mut rects = [egui::Rect::NOTHING; 6];
    let mut clicked_box = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, slot) in rects.iter_mut().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(box_w, box_h), Sense::click());
            *slot = rect;
            if resp.clicked() {
                focused = i;
                active = true;
                clicked_box = true;
            }
        }
    });

    if ui.input(|i| i.pointer.any_click()) && !clicked_box {
        active = false;
    }

    // No TextEdit → no blinking caret. Keys only while a box is selected.
    if active {
        let typed: Vec<char> = ui.input(|i| {
            let mut out = Vec::new();
            for e in &i.events {
                match e {
                    egui::Event::Text(s) | egui::Event::Paste(s) => {
                        out.extend(s.chars().filter(|c| c.is_ascii_digit()));
                    }
                    _ => {}
                }
            }
            out
        });
        let backspace = ui.input(|i| i.key_pressed(egui::Key::Backspace));

        if !typed.is_empty() {
            let mut idx = focused;
            for ch in typed {
                if idx >= 6 {
                    break;
                }
                digits[idx] = ch.to_string();
                changed = true;
                idx += 1;
            }
            // Always move to the next box, whether it already has a digit or not.
            focused = idx.min(5);
        } else if backspace {
            if !digits[focused].is_empty() {
                digits[focused].clear();
                changed = true;
            } else if focused > 0 {
                focused -= 1;
                digits[focused].clear();
                changed = true;
            }
        }
    }

    for (i, rect) in rects.iter().enumerate() {
        let is_focus = active && focused == i;
        ui.painter().rect(
            *rect,
            CornerRadius::same(FIELD_RADIUS),
            p.input,
            Stroke::new(
                if is_focus { 1.5 } else { 1.0 },
                if is_focus { p.accent } else { p.border },
            ),
            egui::StrokeKind::Inside,
        );
        if !digits[i].is_empty() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                &digits[i],
                font.clone(),
                p.accent,
            );
        }
    }

    ui.memory_mut(|m| {
        m.data.insert_persisted(focus_id, focused);
        m.data.insert_persisted(active_id, active);
    });
    changed
}

pub fn format_code(code: &str) -> String {
    let digits: Vec<char> = code.chars().filter(|c| c.is_ascii_digit()).collect();
    let mut out = String::new();
    for i in 0..6 {
        if i == 3 {
            out.push(' ');
        }
        out.push(digits.get(i).copied().unwrap_or('·'));
    }
    out
}

pub fn mac_text_edit(ui: &mut egui::Ui, text: &mut String) -> egui::Response {
    let p = Palette::app();
    let w = field_width(ui);
    let id = ui.next_auto_id().with("mac_text");
    let focused = ui.memory(|m| m.has_focus(id));
    let stroke = if focused {
        Stroke::new(1.5, p.accent)
    } else {
        Stroke::new(1.0, p.border)
    };
    let frame = egui::Frame::new()
        .fill(p.input)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(FIELD_RADIUS))
        .inner_margin(egui::Margin::symmetric(10, 6));
    ui.add_sized(
        egui::vec2(w, FORM_ROW_H),
        egui::TextEdit::singleline(text)
            .id(id)
            .desired_width(w)
            .frame(frame)
            .background_color(p.input)
            .horizontal_align(egui::Align::Center)
            .vertical_align(egui::Align::Center)
            .font(FontId::proportional(FORM_FONT)),
    )
}

/// Shortcut field: click it, then press the combination. Esc cancels,
/// Backspace / Delete clears. Written as `Ctrl+Shift+/`, the core's hotkey syntax.
pub fn hotkey_field(ui: &mut egui::Ui, value: &mut String) -> egui::Response {
    let p = Palette::app();
    let en = crate::locale::is_english();
    let size = Vec2::new(field_width(ui), FORM_ROW_H);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        resp.request_focus();
    }
    let recording = resp.has_focus();
    if recording {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
        let events = ui.input(|i| i.events.clone());
        for ev in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } = ev
            else {
                continue;
            };
            let key = physical_key.unwrap_or(key);
            // A shortcut without Ctrl or Alt would swallow ordinary typing.
            if !modifiers.ctrl && !modifiers.alt {
                match key {
                    egui::Key::Escape => {}
                    egui::Key::Backspace | egui::Key::Delete => {
                        value.clear();
                        resp.mark_changed();
                    }
                    _ => continue,
                }
                resp.surrender_focus();
                break;
            }
            if let Some(name) = hotkey_key_name(key) {
                *value = format!("{}{name}", hotkey_mods(modifiers));
                resp.mark_changed();
                resp.surrender_focus();
                break;
            }
        }
    }
    let recording = resp.has_focus();
    let (text, color) = if recording {
        let held = hotkey_mods(ui.input(|i| i.modifiers));
        let text = if held.is_empty() {
            if en {
                "Press keys…"
            } else {
                "请按下组合键…"
            }
            .to_string()
        } else {
            format!("{held}…")
        };
        (text, p.accent)
    } else if value.trim().is_empty() {
        (if en { "Not set" } else { "未设置" }.to_string(), p.text_2)
    } else {
        (value.clone(), p.text)
    };
    let stroke = if recording {
        Stroke::new(1.5, p.accent)
    } else if resp.hovered() {
        Stroke::new(1.0, p.border_strong)
    } else {
        Stroke::new(1.0, p.border)
    };
    ui.painter().rect(
        rect,
        CornerRadius::same(FIELD_RADIUS),
        p.input,
        stroke,
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(FORM_FONT),
        color,
    );
    let tip = if en {
        "Click, then press the shortcut (Esc cancels, Backspace clears)"
    } else {
        "点击后按下快捷键（Esc 取消，Backspace 清除）"
    };
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tip)
}

fn hotkey_mods(m: egui::Modifiers) -> String {
    let mut out = String::new();
    for (on, name) in [(m.ctrl, "Ctrl"), (m.alt, "Alt"), (m.shift, "Shift")] {
        if on {
            out.push_str(name);
            out.push('+');
        }
    }
    out
}

/// Key name as the core's hotkey parser reads it; `None` for keys it cannot bind.
fn hotkey_key_name(key: egui::Key) -> Option<&'static str> {
    use egui::Key as K;
    Some(match key {
        K::Enter => "Enter",
        K::Escape => "Esc",
        K::Backspace => "Backspace",
        K::Tab => "Tab",
        K::Space => "Space",
        K::Insert => "Insert",
        K::Delete => "Delete",
        K::Home => "Home",
        K::End => "End",
        K::PageUp => "PageUp",
        K::PageDown => "PageDown",
        K::ArrowLeft => "Left",
        K::ArrowRight => "Right",
        K::ArrowUp => "Up",
        K::ArrowDown => "Down",
        K::Minus => "-",
        K::Equals => "=",
        K::OpenBracket => "[",
        K::CloseBracket => "]",
        K::Backslash => "\\",
        K::Semicolon => ";",
        K::Quote => "'",
        K::Backtick => "`",
        K::Comma => ",",
        K::Period => ".",
        K::Slash => "/",
        _ => {
            let name = key.name();
            let ok = (name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric())
                || (name.starts_with('F')
                    && name[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)));
            return ok.then_some(name);
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    None,
    Minimize,
    ToggleMaximize,
    Close,
}

/// Custom title-bar window buttons (minimize / maximize / close), thin white strokes.
/// Returns the clicked action; caller applies close-to-tray vs quit policy.
pub fn window_controls(ui: &mut egui::Ui, p: &Palette) -> WindowAction {
    let maximized = ui.ctx().input(|i| i.viewport().maximized).unwrap_or(false);
    let btn = Vec2::new(40.0, 28.0);
    let mut action = WindowAction::None;
    ui.spacing_mut().item_spacing.x = 0.0;

    // Parent title bar is already right-to-left: allocate Close, then Max, then Min.
    if title_btn(ui, p, btn, TitleIcon::Close).clicked() {
        action = WindowAction::Close;
    }
    if title_btn(
        ui,
        p,
        btn,
        if maximized {
            TitleIcon::Restore
        } else {
            TitleIcon::Maximize
        },
    )
    .clicked()
    {
        action = WindowAction::ToggleMaximize;
    }
    if title_btn(ui, p, btn, TitleIcon::Minimize).clicked() {
        action = WindowAction::Minimize;
    }
    action
}

enum TitleIcon {
    Minimize,
    Maximize,
    Restore,
    Close,
}

fn title_btn(ui: &mut egui::Ui, p: &Palette, size: Vec2, kind: TitleIcon) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let hover = resp.hovered();
    if hover {
        let fill = match kind {
            TitleIcon::Close => Color32::from_rgb(0xe8, 0x11, 0x23),
            _ => p.card_hover,
        };
        ui.painter().rect_filled(rect, CornerRadius::ZERO, fill);
    }
    let stroke_color = if matches!(kind, TitleIcon::Close) && hover {
        Color32::WHITE
    } else {
        p.text_2
    };
    let stroke = Stroke::new(1.2, stroke_color);
    let c = rect.center();
    let painter = ui.painter();
    match kind {
        TitleIcon::Minimize => {
            painter.line_segment(
                [egui::pos2(c.x - 5.0, c.y), egui::pos2(c.x + 5.0, c.y)],
                stroke,
            );
        }
        TitleIcon::Maximize => {
            let r = egui::Rect::from_center_size(c, Vec2::splat(10.0));
            painter.rect_stroke(r, 1.5, stroke, egui::StrokeKind::Outside);
        }
        TitleIcon::Restore => {
            // back square
            let back =
                egui::Rect::from_min_size(egui::pos2(c.x - 3.0, c.y - 5.5), Vec2::splat(8.0));
            painter.rect_stroke(back, 0.5, stroke, egui::StrokeKind::Outside);
            // front square
            let front =
                egui::Rect::from_min_size(egui::pos2(c.x - 5.5, c.y - 3.0), Vec2::splat(8.0));
            painter.rect_filled(front, 0.0, p.bg);
            painter.rect_stroke(front, 0.5, stroke, egui::StrokeKind::Outside);
        }
        TitleIcon::Close => {
            let s = 5.0;
            painter.line_segment(
                [egui::pos2(c.x - s, c.y - s), egui::pos2(c.x + s, c.y + s)],
                stroke,
            );
            painter.line_segment(
                [egui::pos2(c.x + s, c.y - s), egui::pos2(c.x - s, c.y + s)],
                stroke,
            );
        }
    }
    resp
}
