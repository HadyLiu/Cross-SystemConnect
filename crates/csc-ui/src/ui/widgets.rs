//! Shared chrome — application-center style.

use crate::theme::Palette;
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke, Vec2};

pub fn hint(ui: &mut egui::Ui, p: &Palette, text: &str) {
    ui.label(RichText::new(text).size(FORM_FONT - 1.0).color(p.text_2));
}

/// Form metrics: label / control share one font size and one row height.
pub const FORM_FONT: f32 = 14.0;
pub const FORM_LABEL_W: f32 = 140.0;
pub const FORM_FIELD_W: f32 = 180.0;
pub const FORM_ROW_H: f32 = 32.0;

/// Field width capped by remaining row space (avoids column overflow / overlap).
pub fn field_width(ui: &egui::Ui) -> f32 {
    ui.available_width().clamp(64.0, FORM_FIELD_W)
}

/// Two equal columns; `add(left, right)` runs sequentially (safe for `&mut App`).
///
/// Uses unbounded column height so cards inside a `ScrollArea` are not clipped to
/// the viewport (`ui.columns` max_rect bottoms at the parent max, which hides the
/// next row of cards).
pub fn two_col(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui, &mut egui::Ui)) {
    let gap = 14.0;
    ui.spacing_mut().item_spacing.x = gap;
    let total_w = ui.available_width();
    let col_w = ((total_w - gap) * 0.5).max(0.0);
    let top_left = ui.cursor().min;
    // Large finite height: enough for any settings page, without relying on
    // parent max_rect (ScrollArea content max is only the viewport).
    let tall = 100_000.0;

    let left_rect = egui::Rect::from_min_size(top_left, egui::vec2(col_w, tall));
    let right_rect = egui::Rect::from_min_size(
        egui::pos2(top_left.x + col_w + gap, top_left.y),
        egui::vec2(col_w, tall),
    );

    let mut left = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("two_col_l")
            .max_rect(left_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    left.set_width(col_w);
    // Horizontal-only clip so a wide control cannot paint into the sibling column.
    left.shrink_clip_rect(egui::Rect::from_x_y_ranges(
        left_rect.x_range(),
        ui.clip_rect().y_range(),
    ));

    let mut right = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("two_col_r")
            .max_rect(right_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    right.set_width(col_w);
    right.shrink_clip_rect(egui::Rect::from_x_y_ranges(
        right_rect.x_range(),
        ui.clip_rect().y_range(),
    ));

    add(&mut left, &mut right);

    let h = left.min_size().y.max(right.min_size().y);
    ui.advance_cursor_after_rect(egui::Rect::from_min_size(
        top_left,
        egui::vec2(total_w, h),
    ));
}

/// Form row: fixed label column, controls on the same baseline (no nested
/// allocate_ui — that was expanding to the full card height and floating
/// controls below their labels).
pub fn form_row(ui: &mut egui::Ui, p: &Palette, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let row_w = ui.available_width();
    let label_w = FORM_LABEL_W.min((row_w * 0.42).max(72.0));

    // horizontal defaults to cross-align Center → label & control share one baseline.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        ui.set_max_width(row_w);

        ui.allocate_ui_with_layout(
            egui::vec2(label_w, FORM_ROW_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_max_width(label_w);
                ui.add(
                    egui::Label::new(RichText::new(label).size(FORM_FONT).color(p.text_2))
                        .truncate(),
                );
            },
        );

        // Controls are direct children — height follows content, not available_height.
        add(ui);
    });
    ui.add_space(12.0);
}

/// Number / DragValue sized like `.mac-input`, shrinks in narrow columns.
pub fn mac_drag(ui: &mut egui::Ui, drag: egui::DragValue<'_>) -> egui::Response {
    ui.add_sized(egui::vec2(field_width(ui), FORM_ROW_H), drag)
}

/// Soft toggle (green when on); height matches form row.
pub fn mac_toggle(ui: &mut egui::Ui, p: &Palette, on: &mut bool) -> egui::Response {
    let size = Vec2::new(48.0, FORM_ROW_H - 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    let fill = if *on {
        p.ok
    } else {
        Color32::from_rgb(0x4a, 0x4e, 0x56)
    };
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
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
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
    paint_nav_icon(ui.painter(), icon_rect, icon, icon_color);
    ui.painter().text(
        egui::pos2(rect.left() + 42.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        text_color,
    );
    resp
}

fn paint_nav_icon(painter: &egui::Painter, rect: egui::Rect, icon: NavIcon, color: Color32) {
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
            painter.rect_filled(front, 1.0, Color32::from_rgb(0x24, 0x26, 0x2a));
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
    ui.add(
        egui::Button::new(RichText::new(label).size(FORM_FONT).color(p.text))
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(6.0)
            .min_size(egui::vec2(0.0, FORM_ROW_H)),
    )
}

pub fn primary_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(FORM_FONT).color(p.on_accent))
            .fill(p.accent)
            .stroke(Stroke::NONE)
            .corner_radius(6.0)
            .min_size(egui::vec2(0.0, FORM_ROW_H)),
    )
}

pub fn danger_text_button(ui: &mut egui::Ui, p: &Palette, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).size(FORM_FONT).color(p.danger))
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(6.0)
            .min_size(egui::vec2(0.0, FORM_ROW_H)),
    )
}

pub fn pairing_code(ui: &mut egui::Ui, p: &Palette, code: &str) {
    let display = format_code(code);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(118.0, FORM_ROW_H), Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(6), p.input);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, p.border),
        egui::StrokeKind::Outside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        display,
        FontId::monospace(FORM_FONT),
        p.accent,
    );
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
        ui.painter()
            .rect_filled(*rect, CornerRadius::same(6), p.input);
        ui.painter().rect_stroke(
            *rect,
            CornerRadius::same(6),
            Stroke::new(
                if is_focus { 1.5 } else { 1.0 },
                if is_focus { p.accent } else { p.border },
            ),
            egui::StrokeKind::Outside,
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
    let w = field_width(ui);
    ui.add_sized(
        egui::vec2(w, FORM_ROW_H),
        egui::TextEdit::singleline(text)
            .desired_width(w)
            .margin(egui::Margin::symmetric(10, 4))
            .horizontal_align(egui::Align::LEFT)
            .vertical_align(egui::Align::Center)
            .font(FontId::proportional(FORM_FONT)),
    )
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
        Color32::from_rgb(0xc8, 0xcc, 0xd4)
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
