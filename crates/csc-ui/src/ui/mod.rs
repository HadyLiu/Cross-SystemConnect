mod home;
mod pages;
mod transfer;
pub mod widgets;

pub use home::draw_home;
pub use pages::{draw_about, draw_sharing, draw_switch, draw_system};
pub use transfer::TransferUi;

use crate::app::{App, Page};
use crate::theme::Palette;
use eframe::egui::{self, Order, RichText};

const TITLE_H: f32 = 36.0;
const NAV_W: f32 = 210.0;
const PAGE_HEADER_H: f32 = 48.0;

pub fn draw(app: &mut App, ui: &mut egui::Ui) {
    let p = Palette::app();
    let ctx = ui.ctx().clone();
    let full = ctx.content_rect();

    ui.painter().rect_filled(full, 0.0, p.bg);

    // Page body on the root layer (normal layout so cards/controls paint correctly).
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(full.left() + NAV_W, full.top() + TITLE_H),
        full.right_bottom(),
    );
    draw_content(app, ui, &p, content_rect);

    // Claim the viewport so root Ui does not grow with page content.
    let _ = ui.allocate_rect(full, egui::Sense::hover());

    // Chrome on a foreground layer — never scrolls with the page body.
    draw_titlebar(app, &ctx, &p, full);
    draw_nav(app, &ctx, &p, full);
}

fn draw_titlebar(app: &mut App, ctx: &egui::Context, p: &Palette, full: egui::Rect) {
    let size = egui::vec2(full.width(), TITLE_H);

    egui::Area::new(egui::Id::new("shell_titlebar"))
        .order(Order::Foreground)
        .fixed_pos(full.left_top())
        .default_size(size)
        .constrain(false)
        .movable(false)
        .show(ctx, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            let rect = ui.max_rect();
            ui.set_clip_rect(rect);

            ui.painter().rect_filled(rect, 0.0, p.bg);
            ui.painter().hline(
                rect.x_range(),
                rect.bottom() - 0.5,
                egui::Stroke::new(1.0, p.border),
            );

            let controls_w = 124.0;
            let drag_rect = egui::Rect::from_min_max(
                rect.left_top(),
                egui::pos2((rect.right() - controls_w).max(rect.left()), rect.bottom()),
            );
            let controls_rect = egui::Rect::from_min_max(
                egui::pos2(drag_rect.right(), rect.top()),
                rect.right_bottom(),
            );

            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(drag_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
                |ui| {
                    ui.set_min_height(rect.height());
                    ui.add_space(12.0);
                    if let Some(tex) = app.logo.as_ref() {
                        ui.add(
                            egui::Image::new(egui::load::SizedTexture::from_handle(tex))
                                .fit_to_exact_size(egui::vec2(16.0, 16.0)),
                        );
                        ui.add_space(6.0);
                    }
                    ui.label(
                        RichText::new(format!(
                            "Cross-SystemConnect  v{}",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .size(11.0)
                        .color(p.text_2),
                    );
                },
            );

            let drag = ui.interact(
                drag_rect,
                ui.id().with("title_drag"),
                egui::Sense::click_and_drag(),
            );
            if drag.drag_started() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if drag.double_clicked() {
                let maximized = ui.ctx().input(|i| i.viewport().maximized).unwrap_or(false);
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
            }

            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(controls_rect)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
                |ui| {
                    ui.set_min_height(rect.height());
                    ui.add_space(4.0);
                    match widgets::window_controls(ui, p) {
                        widgets::WindowAction::Close => {
                            if app.cfg.gui.close_to_tray {
                                app.hide_settings(ui.ctx());
                            } else {
                                app.request_quit(ui.ctx());
                            }
                        }
                        widgets::WindowAction::Minimize => {
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                        widgets::WindowAction::ToggleMaximize => {
                            let maximized =
                                ui.ctx().input(|i| i.viewport().maximized).unwrap_or(false);
                            ui.ctx()
                                .send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        widgets::WindowAction::None => {}
                    }
                },
            );
        });
}

fn draw_nav(app: &mut App, ctx: &egui::Context, p: &Palette, full: egui::Rect) {
    let size = egui::vec2(NAV_W, (full.height() - TITLE_H).max(0.0));
    let pos = egui::pos2(full.left(), full.top() + TITLE_H);

    egui::Area::new(egui::Id::new("shell_nav"))
        .order(Order::Foreground)
        .fixed_pos(pos)
        .default_size(size)
        .constrain(false)
        .movable(false)
        .show(ctx, |ui| {
            ui.set_min_size(size);
            ui.set_max_size(size);
            let rect = ui.max_rect();
            ui.set_clip_rect(rect);

            ui.painter()
                .vline(rect.right() - 0.5, rect.y_range(), egui::Stroke::new(1.0, p.border));

            egui::Frame::new()
                .fill(p.sidebar)
                .inner_margin(egui::Margin::symmetric(10, 12))
                .show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    ui.set_max_height(ui.available_height());
                    ui.add_space(8.0);
                    widgets::brand_block(ui, p, app.logo.as_ref());
                    ui.add_space(18.0);

                    for (page, icon, zh, en) in [
                        (
                            Page::Home,
                            widgets::NavIcon::Devices,
                            "设备与拓扑",
                            "Devices and topology",
                        ),
                        (
                            Page::Sharing,
                            widgets::NavIcon::Sharing,
                            "共享与文件",
                            "Sharing and files",
                        ),
                        (
                            Page::Switch,
                            widgets::NavIcon::Switch,
                            "切换与热键",
                            "Switching and hotkeys",
                        ),
                        (
                            Page::System,
                            widgets::NavIcon::System,
                            "系统与网络",
                            "System and network",
                        ),
                        (Page::About, widgets::NavIcon::About, "关于软件", "About"),
                    ] {
                        let label = app.t(zh, en).to_string();
                        if widgets::nav_item(ui, p, app.page == page, icon, &label).clicked() {
                            app.page = page;
                        }
                        ui.add_space(3.0);
                    }

                    ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                        ui.add_space(4.0);
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            ui.cursor().top(),
                            egui::Stroke::new(1.0, p.border),
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(app.t("无界无缝接入", "Seamless access"))
                                    .size(11.0)
                                    .color(p.text_2),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let connected = app
                                    .status
                                    .get("connected")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                widgets::status_pill(
                                    ui,
                                    p,
                                    connected,
                                    if connected {
                                        app.t("已连接", "Connected")
                                    } else {
                                        app.t("就绪", "Ready")
                                    },
                                );
                            });
                        });
                    });
                });
        });
}

fn draw_content(app: &mut App, ui: &mut egui::Ui, p: &Palette, rect: egui::Rect) {
    ui.scope_builder(
        egui::UiBuilder::new()
            .id_salt("shell_content")
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(rect);
            // Cap width/height to the content slot without expanding min_rect first —
            // set_min_size(full) before layout can disturb available_height.
            ui.set_max_size(rect.size());

            // Fixed page header (does not scroll).
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), PAGE_HEADER_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add_space(20.0);
                    ui.label(
                        RichText::new(app.page_title())
                            .size(20.0)
                            .strong()
                            .color(p.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(20.0);
                        if widgets::ghost_button(ui, p, app.t("刷新节点", "Refresh nodes")).clicked()
                        {
                            let _ = crate::ipc_client::rescan();
                            app.reload_status();
                            app.set_ok(app.t("已重新搜索", "Rescanned"));
                        }
                        if !app.message.is_empty() {
                            let color = if app.message_ok { p.ok } else { p.danger };
                            ui.label(RichText::new(&app.message).size(11.0).color(color));
                        }
                    });
                },
            );
            ui.painter().hline(
                ui.max_rect().x_range(),
                ui.cursor().top(),
                egui::Stroke::new(1.0, p.border),
            );

            let body_h = ui.available_height().max(64.0);
            egui::ScrollArea::vertical()
                .id_salt("page_body")
                .max_height(body_h)
                .min_scrolled_height(body_h)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(20, 16))
                        .show(ui, |ui| match app.page {
                            Page::Home => draw_home(app, ui),
                            Page::Sharing => draw_sharing(app, ui),
                            Page::Switch => draw_switch(app, ui),
                            Page::System => draw_system(app, ui),
                            Page::About => draw_about(app, ui),
                        });
                });
        },
    );
}
