//! Doc/y.html tab-devices: two-column basics+topology, then connected devices.

use crate::app::App;
use crate::theme::Palette;
use crate::ui::widgets::{
    self, digit_boxes, field_width, ghost_button, mac_number_fixed, pairing_code, primary_button,
    section_heading, settings_card, settings_card_body,
};
use eframe::egui::{self, Color32, RichText, Sense};

fn tr<'a>(zh: &'a str, en: &'a str) -> &'a str {
    if crate::locale::is_english() {
        en
    } else {
        zh
    }
}

pub fn draw_home(app: &mut App, ui: &mut egui::Ui) {
    let p = Palette::app();
    let is_server = matches!(app.cfg.role, csc_config::Role::Server);

    section_heading(ui, &p, tr("基础参数与配对", "Basics and pairing"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("计算机名称", "Computer name"),
            tr("在局域网中显示的名称。", "Name shown on the LAN."),
            |ui| {
                if widgets::mac_text_edit(ui, &mut app.cfg.name).lost_focus() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("搜索 IP 范围", "Search IP range"),
            tr(
                "默认优先以太网；仅同一 /24 网段内的电脑可连接。",
                "Ethernet is preferred by default; only peers on the same /24 may connect.",
            ),
            |ui| {
                draw_nic_combo(app, ui, &p);
            },
        );
        list.row(
            tr("本机配对码", "Pairing code"),
            tr(
                "后三位是本机 IP 尾号；配对成功一次后自动更换。",
                "Last three digits are this PC’s IP host; peers dial directly.",
            ),
            |ui| {
                let code = app
                    .status
                    .get("homeCode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("------")
                    .to_string();
                // right_to_left: button on the far right, same edge as IP combo.
                let refresh = ui.add(
                    egui::Button::new(
                        RichText::new(tr("更换", "Refresh"))
                            .size(12.5)
                            .color(p.text),
                    )
                    .fill(egui::Color32::TRANSPARENT)
                    .stroke(egui::Stroke::new(1.0, p.border_strong))
                    .corner_radius(6.0)
                    .min_size(egui::vec2(64.0, 46.0)),
                );
                if refresh.clicked() {
                    match crate::ipc_client::roll_home_code() {
                        Ok(_) => {
                            app.reload_status();
                            app.set_ok(tr("已换一组配对码", "New pairing code"));
                        }
                        Err(e) => app.set_err(e.to_string()),
                    }
                }
                ui.add_space(8.0);
                pairing_code(ui, &p, &code);
            },
        );
    });

    settings_card_body(ui, &p, |ui| {
        ui.label(
            RichText::new(tr("输入对方配对码", "Enter peer code"))
                .size(13.5)
                .color(p.text),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(tr(
                "后三位对应对方 IP 尾号，输满后立即直连，无需等待搜索。",
                "Last three digits are the peer’s IP host; connects immediately.",
            ))
            .size(12.0)
            .color(p.text_2),
        );
        ui.add_space(12.0);
        if digit_boxes(ui, &p, &mut app.pair_digits) {
            try_send_pair_code(app);
        }
    });

    section_heading(ui, &p, tr("屏幕拓扑", "Screen topology"));
    settings_card_body(ui, &p, |ui| {
        if is_server {
            draw_desk(app, ui, &p);
        } else {
            widgets::hint(
                ui,
                &p,
                tr(
                    "中间是键鼠主机；本机位置由主机摆放。",
                    "Center is the keyboard host; this PC’s slot is set by the host.",
                ),
            );
            ui.add_space(8.0);
            draw_desk_readonly(app, ui, &p);
        }
    });

    section_heading(ui, &p, tr("已信任设备", "Trusted devices"));
    settings_card_body(ui, &p, |ui| {
        draw_device_table(app, ui, &p);
    });
}

fn draw_nic_combo(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    // right_to_left: refresh sits on the far right, same as the pairing-code row.
    let refresh = ui.add(
        egui::Button::new(
            RichText::new(tr("刷新", "Refresh"))
                .size(12.5)
                .color(p.text),
        )
        .fill(egui::Color32::TRANSPARENT)
        .stroke(egui::Stroke::new(1.0, p.border_strong))
        .corner_radius(6.0)
        .min_size(egui::vec2(64.0, 46.0)),
    );
    if refresh.clicked() {
        match crate::ipc_client::refresh_nics() {
            Ok(v) => {
                if let Some(ip) = v.get("searchAddress").and_then(|s| s.as_str()) {
                    if !ip.is_empty() && app.cfg.search_address != ip {
                        app.cfg.search_address = ip.to_string();
                    }
                }
                app.reload_config();
                app.reload_status();
                let count = app
                    .status
                    .get("localNics")
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                app.set_ok(format!(
                    "{} ({count})",
                    tr("已刷新网卡", "Adapters refreshed")
                ));
            }
            Err(e) => {
                app.reload_status();
                app.set_err(e.to_string());
            }
        }
    }
    ui.add_space(8.0);

    let nics = app
        .status
        .get("localNics")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut options: Vec<(String, String)> = Vec::new();
    if nics.is_empty() {
        if let Some(arr) = app.status.get("localIpv4").and_then(|v| v.as_array()) {
            for ip in arr {
                if let Some(ip) = ip.as_str() {
                    options.push((ip.to_string(), ip.to_string()));
                }
            }
        }
    } else {
        for nic in &nics {
            let name = nic.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            // Core / LocalNic use `address`; older drafts used `ipv4`.
            let ip = nic
                .get("address")
                .or_else(|| nic.get("ipv4"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if ip.is_empty() {
                continue;
            }
            let kind = match nic.get("kind").and_then(|v| v.as_str()).unwrap_or("") {
                "ethernet" => tr("以太网", "Ethernet"),
                "wifi" => tr("无线", "Wi-Fi"),
                _ => "",
            };
            let label = if kind.is_empty() {
                format!("{name}: {ip}")
            } else {
                format!("{name} ({kind}): {ip}")
            };
            options.push((ip.to_string(), label));
        }
    }
    let current = if app.cfg.search_address.is_empty() {
        options
            .first()
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| "—".into())
    } else {
        options
            .iter()
            .find(|(ip, _)| *ip == app.cfg.search_address)
            .map(|(_, l)| l.clone())
            .unwrap_or_else(|| app.cfg.search_address.clone())
    };
    egui::ComboBox::from_id_salt("nic")
        .selected_text(current)
        .width(field_width(ui))
        .show_ui(ui, |ui| {
            for (ip, label) in &options {
                if ui
                    .selectable_label(app.cfg.search_address == *ip, label)
                    .clicked()
                {
                    app.cfg.search_address = ip.clone();
                    app.queue_save();
                }
            }
        });
}

fn draw_device_table(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    let peers = app.cfg.trusted_peers.clone();
    if peers.is_empty() {
        widgets::hint(ui, p, tr("还没有信任的电脑。", "No trusted computers yet."));
        return;
    }

    let cols = device_col_widths(ui.available_width());
    const ROW_H: f32 = 36.0;
    const GAP: f32 = 12.0;

    device_row(ui, ROW_H, GAP, &cols, |ui, c| {
        col_label(ui, c.name, tr("设备名", "Name"), p.text_2, false);
        col_label(ui, c.ip, tr("IP 地址", "IP"), p.text_2, false);
        col_label(ui, c.status, tr("状态", "Status"), p.text_2, false);
        col_label(ui, c.pointer, tr("指针灵敏度", "Pointer"), p.text_2, false);
        col_label(ui, c.action, tr("操作", "Action"), p.text_2, false);
    });
    ui.painter().hline(
        ui.max_rect().x_range(),
        ui.cursor().top(),
        egui::Stroke::new(1.0, p.border),
    );
    ui.add_space(6.0);

    for name in peers {
        let ip = peer_ip(app, &name);
        let online = peer_online(app, &name);
        let mut forget = false;
        device_row(ui, ROW_H, GAP, &cols, |ui, c| {
            col_label(ui, c.name, &name, Color32::WHITE, true);
            col_label(ui, c.ip, &ip, p.text_2, false);
            let (text, color) = if online {
                (tr("在线", "Online"), p.ok)
            } else {
                (tr("离线", "Offline"), p.text_2)
            };
            col_label(ui, c.status, text, color, false);

            ui.allocate_ui_with_layout(
                egui::vec2(c.pointer, ROW_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_size(egui::vec2(c.pointer, ROW_H));
                    ui.set_max_width(c.pointer);
                    let mut speed = app
                        .cfg
                        .pointer
                        .peers
                        .get(&name)
                        .copied()
                        .unwrap_or(app.cfg.pointer.speed);
                    if speed_control(ui, p, c.pointer, ROW_H, &mut speed) {
                        app.cfg.pointer.peers.insert(name.clone(), speed);
                        app.queue_save();
                    }
                },
            );

            ui.allocate_ui_with_layout(
                egui::vec2(c.action, ROW_H),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_size(egui::vec2(c.action, ROW_H));
                    ui.set_max_width(c.action);
                    if widgets::danger_text_button(ui, p, tr("断开", "Disconnect")).clicked() {
                        forget = true;
                    }
                },
            );
        });
        if forget {
            match crate::ipc_client::forget_peer(&name) {
                Ok(_) => {
                    app.reload_config();
                    app.set_ok(format!("{} {name}", tr("已移除", "Removed")));
                }
                Err(e) => app.set_err(e.to_string()),
            }
        }
        ui.add_space(4.0);
    }
}

const SPEED_MIN: f32 = 0.6;
const SPEED_MAX: f32 = 2.0;
const SPEED_STEP: f32 = 0.2;
const SPEED_FIELD_W: f32 = 64.0;

/// Slider (0.6–2.0 in steps of 0.2) plus a field for typing an exact value.
fn speed_control(ui: &mut egui::Ui, p: &Palette, w: f32, h: f32, speed: &mut f32) -> bool {
    let gap = 8.0;
    let slider_w = (w - SPEED_FIELD_W - gap).max(48.0);
    ui.spacing_mut().item_spacing.x = gap;
    ui.spacing_mut().slider_width = slider_w;
    let visuals = &mut ui.visuals_mut().widgets;
    visuals.inactive.bg_fill = p.border_strong;
    visuals.inactive.fg_stroke.color = p.text;
    visuals.hovered.fg_stroke.color = Color32::WHITE;
    ui.visuals_mut().selection.bg_fill = p.accent;
    let mut changed = ui
        .add(
            egui::Slider::new(speed, SPEED_MIN..=SPEED_MAX)
                .step_by(f64::from(SPEED_STEP))
                .show_value(false)
                .trailing_fill(true),
        )
        .changed();
    ui.allocate_ui_with_layout(
        egui::vec2(SPEED_FIELD_W, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_max_width(SPEED_FIELD_W);
            changed |= mac_number_fixed(ui, speed, SPEED_MIN..=SPEED_MAX, 1).changed();
        },
    );
    changed
}

#[derive(Clone, Copy)]
struct DeviceCols {
    name: f32,
    ip: f32,
    status: f32,
    pointer: f32,
    action: f32,
}

fn device_col_widths(total: f32) -> DeviceCols {
    let action = 72.0;
    let gaps = 12.0 * 4.0;
    let rest = (total - action - gaps).max(280.0);
    DeviceCols {
        name: rest * 0.30,
        ip: rest * 0.24,
        status: rest * 0.14,
        pointer: rest * 0.32,
        action,
    }
}

fn device_row(
    ui: &mut egui::Ui,
    row_h: f32,
    gap: f32,
    cols: &DeviceCols,
    add: impl FnOnce(&mut egui::Ui, &DeviceCols),
) {
    ui.horizontal(|ui| {
        ui.set_min_height(row_h);
        ui.spacing_mut().item_spacing.x = gap;
        add(ui, cols);
    });
}

fn col_label(ui: &mut egui::Ui, w: f32, text: &str, color: Color32, strong: bool) {
    ui.allocate_ui_with_layout(
        egui::vec2(w, ui.min_rect().height().max(36.0)),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(egui::vec2(w, 36.0));
            ui.set_max_width(w);
            let mut label = RichText::new(text).size(12.0).color(color);
            if strong {
                label = label.strong();
            }
            ui.add(egui::Label::new(label).truncate());
        },
    );
}

fn peer_ip(app: &App, name: &str) -> String {
    if let Some(arr) = app.status.get("discovered").and_then(|v| v.as_array()) {
        for peer in arr {
            let n = peer
                .get("name")
                .or_else(|| peer.get("hostName"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if n.eq_ignore_ascii_case(name) {
                if let Some(ip) = peer.get("address").and_then(|v| v.as_str()) {
                    if !ip.is_empty() {
                        return ip.to_string();
                    }
                }
            }
        }
    }
    if let Some(peer) = app.status.get("pendingPeer") {
        let n = peer
            .get("name")
            .or_else(|| peer.get("hostName"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if n.eq_ignore_ascii_case(name) {
            if let Some(ip) = peer.get("address").and_then(|v| v.as_str()) {
                if !ip.is_empty() {
                    return ip.to_string();
                }
            }
        }
    }
    "—".into()
}

fn peer_online(app: &App, name: &str) -> bool {
    let connected = app
        .status
        .get("connected")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !connected {
        return false;
    }
    let peer_name = app
        .status
        .get("peerName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    peer_name
        .split(',')
        .map(str::trim)
        .any(|n| n.eq_ignore_ascii_case(name))
}

fn try_send_pair_code(app: &mut App) {
    let code: String = app.pair_digits.iter().cloned().collect();
    if code.len() != 6 || code == app.last_pair_sent {
        return;
    }
    // Typing a peer code means this computer is the keyboard host.
    if !matches!(app.cfg.role, csc_config::Role::Server) {
        app.cfg.role = csc_config::Role::Server;
        app.queue_save();
    }
    app.last_pair_sent = code.clone();
    match crate::ipc_client::pair_offer(&code) {
        Ok(_) => app.set_ok(tr("正在连接…", "Connecting…")),
        Err(e) => {
            app.last_pair_sent.clear();
            app.set_err(e.to_string());
        }
    }
}

/// Connection confirm as a centered modal (not an inline banner).
pub fn draw_pair_confirm_modal(app: &mut App, ctx: &egui::Context) {
    let p = Palette::app();
    let Some(peer) = app.status.get("pendingPeer").cloned() else {
        return;
    };
    if !peer
        .get("confirmOnly")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return;
    }
    let host = peer
        .get("hostName")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if host.is_empty() {
        return;
    }
    let name = peer
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let who = if !name.is_empty() && name != host {
        format!("{name}（{host}）")
    } else if !name.is_empty() {
        name
    } else {
        host.clone()
    };

    let frame = egui::Frame::new()
        .fill(p.card)
        .stroke(egui::Stroke::new(1.0, p.accent))
        .corner_radius(12)
        .inner_margin(egui::Margin::symmetric(22, 18))
        .shadow(egui::Shadow {
            offset: [0, 8],
            blur: 24,
            spread: 0,
            color: Color32::from_black_alpha(120),
        });

    egui::Modal::new(egui::Id::new("pair_confirm"))
        .frame(frame)
        .backdrop_color(Color32::from_black_alpha(140))
        .show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.set_max_width(420.0);
            ui.label(
                RichText::new(tr("是否连接", "Allow connection"))
                    .size(16.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(10.0);
            ui.label(
                RichText::new(format!(
                    "{} {}",
                    who,
                    tr(
                        "输入了本机配对码。确认后才会建立连接。",
                        "entered this computer’s pairing code. Confirm to connect."
                    )
                ))
                .size(13.0)
                .color(p.text_2),
            );
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ghost_button(ui, &p, tr("取消", "Cancel")).clicked() {
                        match crate::ipc_client::resolve_peer(&host, "reject", "") {
                            Ok(_) => {
                                app.reload_status();
                                app.set_ok(tr("已取消", "Cancelled"));
                            }
                            Err(e) => app.set_err(e.to_string()),
                        }
                    }
                    if primary_button(ui, &p, tr("确认连接", "Confirm")).clicked() {
                        match crate::ipc_client::resolve_peer(&host, "trust", "") {
                            Ok(_) => {
                                app.reload_status();
                                app.reload_config();
                                app.set_ok(tr("已确认", "Confirmed"));
                            }
                            Err(e) => app.set_err(e.to_string()),
                        }
                    }
                });
            });
        });
}

fn draw_desk_readonly(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    // Client view: keyboard host stays in the center; this computer is a peer tile.
    let local = app.cfg.name.clone();
    let kb_host = keyboard_host_name(app);
    let avail = ui.available_width().max(180.0);
    let h = 210.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail, h), Sense::hover());

    let mut screens: Vec<(i32, i32, String)> = app
        .cfg
        .screens
        .iter()
        .filter(|s| !s.name.eq_ignore_ascii_case(&kb_host))
        .filter(|s| !(s.x == 0 && s.y == 0))
        .map(|s| (s.x, s.y, s.name.clone()))
        .collect();

    // Local machine not in host layout yet — still show it beside the host.
    let local_shown = screens
        .iter()
        .any(|(_, _, n)| n.eq_ignore_ascii_case(&local));
    if !local.is_empty()
        && !local.eq_ignore_ascii_case(&kb_host)
        && !local_shown
        && app
            .status
            .get("connected")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    {
        screens.push((1, 0, local.clone()));
    }

    paint_topology_grid(
        ui,
        p,
        rect,
        &screens,
        &kb_host,
        tr("(主机)", "(host)"),
        Some(local.as_str()),
        None,
        None,
    );
}

/// Name of the keyboard host for client topology (center cell).
fn keyboard_host_name(app: &App) -> String {
    if let Some(peer) = app
        .status
        .get("peerName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return peer.split(',').next().unwrap_or(peer).trim().to_string();
    }
    for key in ["activeScreen", "hostName"] {
        if let Some(n) = app.status.get(key).and_then(|v| v.as_str()).map(str::trim) {
            if !n.is_empty() && !n.eq_ignore_ascii_case(&app.cfg.name) {
                return n.to_string();
            }
        }
    }
    app.cfg
        .trusted_peers
        .iter()
        .find(|n| !n.eq_ignore_ascii_case(&app.cfg.name))
        .cloned()
        .unwrap_or_else(|| tr("主机", "Host").to_string())
}

fn draw_desk(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    if app.cfg.ensure_trusted_screen_places() {
        app.queue_save();
    }

    let host = if app.cfg.name.is_empty() {
        "本机".to_string()
    } else {
        app.cfg.name.clone()
    };
    let avail = ui.available_width().max(180.0);
    let h = 210.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail, h), Sense::click_and_drag());

    let dragging = app.desk_drag.as_ref().map(|(n, _)| n.clone());
    let drag_pos = app.desk_drag_pos;
    let screens: Vec<(i32, i32, String)> = app
        .cfg
        .screens
        .iter()
        .filter(|s| !s.name.eq_ignore_ascii_case(&host))
        .filter(|s| {
            dragging
                .as_ref()
                .is_none_or(|d| !s.name.eq_ignore_ascii_case(d))
        })
        .map(|s| (s.x, s.y, s.name.clone()))
        .collect();

    let snap_preview = drag_pos.and_then(|pos| {
        let (sx, sy) = desk_to_slot(rect, pos);
        if sx == 0 && sy == 0 {
            None
        } else {
            Some((sx, sy))
        }
    });
    let floating = match (drag_pos, dragging) {
        (Some(pos), Some(name)) => Some((pos, name)),
        _ => None,
    };
    paint_topology_grid(
        ui,
        p,
        rect,
        &screens,
        &host,
        tr("(本机)", "(local)"),
        None,
        snap_preview,
        floating,
    );

    widgets::hint(
        ui,
        p,
        tr(
            "拖动对方屏幕到空位，松开后吸附。",
            "Drag a peer screen onto a slot; it snaps on release.",
        ),
    );

    // Start drag on a placed peer.
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some((sx, sy)) = slot_at_exact(rect, pos) {
                if let Some(place) = app
                    .cfg
                    .screens
                    .iter()
                    .find(|s| s.x == sx && s.y == sy && !s.name.eq_ignore_ascii_case(&host))
                    .cloned()
                {
                    let cell = cell_rect(rect, sx, sy);
                    app.desk_drag = Some((place.name, pos - cell.center()));
                    app.desk_drag_pos = Some(pos);
                }
            }
        }
    }

    if app.desk_drag.is_some() {
        if let Some(pos) = ui
            .ctx()
            .pointer_interact_pos()
            .or_else(|| resp.interact_pointer_pos())
        {
            app.desk_drag_pos = Some(pos);
            ui.ctx().request_repaint();
        }
    }

    if resp.drag_stopped() || (app.desk_drag.is_some() && ui.input(|i| i.pointer.any_released())) {
        if let Some((name, _)) = app.desk_drag.take() {
            let pos = app.desk_drag_pos.take().unwrap_or(rect.center());
            let (mut sx, mut sy) = desk_to_slot(rect, pos);
            if sx == 0 && sy == 0 {
                // Don't park on the host — snap to nearest free ring slot.
                (sx, sy) = csc_config::nearest_screen_slot(1, 0, &name, &app.cfg.screens);
            }
            // Claim the slot: kick whoever was there (except the dragged peer).
            app.cfg
                .screens
                .retain(|s| s.name.eq_ignore_ascii_case(&name) || s.x != sx || s.y != sy);
            if let Some(place) = app
                .cfg
                .screens
                .iter_mut()
                .find(|s| s.name.eq_ignore_ascii_case(&name))
            {
                place.x = sx;
                place.y = sy;
            } else {
                app.cfg.screens.push(csc_config::ScreenPlace {
                    name: name.clone(),
                    x: sx,
                    y: sy,
                });
            }
            app.cfg.sync_layout_from_screens();
            app.queue_save();
            app.set_ok(tr("已更新屏幕位置", "Screen position updated"));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_topology_grid(
    ui: &egui::Ui,
    p: &Palette,
    rect: egui::Rect,
    screens: &[(i32, i32, String)],
    center_name: &str,
    center_sub: &str,
    local_name: Option<&str>,
    snap_preview: Option<(i32, i32)>,
    dragging: Option<(egui::Pos2, String)>,
) {
    let painter = ui.painter();
    painter.rect(
        rect,
        8.0,
        p.input,
        egui::Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );

    for sy in -1i32..=1 {
        for sx in -1i32..=1 {
            let cell = cell_rect(rect, sx, sy);
            let is_center = sx == 0 && sy == 0;
            let peer = screens
                .iter()
                .find(|(px, py, _)| *px == sx && *py == sy)
                .map(|(_, _, n)| n.as_str());
            let is_snap = snap_preview == Some((sx, sy));

            if is_center {
                paint_mon(
                    painter,
                    p,
                    cell,
                    center_name,
                    center_sub,
                    p.ok_soft,
                    egui::Stroke::new(1.0, p.ok),
                    p.text,
                );
            } else if let Some(name) = peer {
                let is_local = local_name.is_some_and(|l| name.eq_ignore_ascii_case(l));
                paint_mon(
                    painter,
                    p,
                    cell,
                    name,
                    if is_local {
                        tr("(本机)", "(local)")
                    } else {
                        ""
                    },
                    p.card,
                    egui::Stroke::new(1.0, p.border_strong),
                    p.text,
                );
            } else {
                painter.rect(
                    cell,
                    8.0,
                    if is_snap {
                        p.accent_soft
                    } else {
                        Color32::TRANSPARENT
                    },
                    egui::Stroke::new(1.0, if is_snap { p.accent } else { p.border }),
                    egui::StrokeKind::Inside,
                );
            }
        }
    }

    // Floating tile while dragging.
    if let Some((pos, name)) = dragging {
        let size = cell_size(rect);
        let cell = egui::Rect::from_center_size(pos, size);
        paint_mon(
            painter,
            p,
            cell,
            &name,
            "",
            p.card_hover,
            egui::Stroke::new(1.5, p.accent),
            p.text,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_mon(
    painter: &egui::Painter,
    p: &Palette,
    cell: egui::Rect,
    title: &str,
    sub: &str,
    fill: Color32,
    stroke: egui::Stroke,
    fg: Color32,
) {
    painter.rect_filled(cell, 10.0, fill);
    painter.rect_stroke(cell, 10.0, stroke, egui::StrokeKind::Inside);
    // Truncate long hostnames so two lines stay readable in the cell.
    let title = truncate_label(title, 12);
    if sub.is_empty() {
        painter.text(
            cell.center(),
            egui::Align2::CENTER_CENTER,
            title,
            egui::FontId::proportional(13.0),
            fg,
        );
    } else {
        let gap = 5.0;
        let title_h = 15.0;
        let sub_h = 13.0;
        let block_h = title_h + gap + sub_h;
        let top = cell.center().y - block_h * 0.5;
        painter.text(
            egui::pos2(cell.center().x, top + title_h * 0.5),
            egui::Align2::CENTER_CENTER,
            title,
            egui::FontId::proportional(13.0),
            fg,
        );
        painter.text(
            egui::pos2(cell.center().x, top + title_h + gap + sub_h * 0.5),
            egui::Align2::CENTER_CENTER,
            sub,
            egui::FontId::proportional(11.0),
            p.text_2,
        );
    }
}

fn truncate_label(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn desk_metrics(rect: egui::Rect) -> (egui::Rect, f32, f32, f32) {
    let pad = 8.0;
    let gap = 8.0;
    let inner = rect.shrink(pad);
    let cell_w = (inner.width() - gap * 2.0) / 3.0;
    let cell_h = (inner.height() - gap * 2.0) / 3.0;
    (inner, cell_w, cell_h, gap)
}

fn cell_size(rect: egui::Rect) -> egui::Vec2 {
    let (_, w, h, _) = desk_metrics(rect);
    egui::vec2(w, h)
}

fn cell_rect(rect: egui::Rect, sx: i32, sy: i32) -> egui::Rect {
    let (inner, cell_w, cell_h, gap) = desk_metrics(rect);
    // Map arbitrary slot coords onto the 3×3 visual ring (clamp for stacked slots).
    let col = sx.clamp(-1, 1) as f32;
    let row = sy.clamp(-1, 1) as f32;
    let x = inner.left() + (col + 1.0) * (cell_w + gap);
    let y = inner.top() + (row + 1.0) * (cell_h + gap);
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell_w, cell_h))
}

fn slot_at_exact(rect: egui::Rect, pos: egui::Pos2) -> Option<(i32, i32)> {
    let (inner, cell_w, cell_h, gap) = desk_metrics(rect);
    if !inner.contains(pos) {
        return None;
    }
    let lx = ((pos.x - inner.left()) / (cell_w + gap)).floor() as i32;
    let ly = ((pos.y - inner.top()) / (cell_h + gap)).floor() as i32;
    if (0..3).contains(&lx) && (0..3).contains(&ly) {
        Some((lx - 1, ly - 1))
    } else {
        None
    }
}

/// Continuous pointer → nearest ring slot (may be host center; caller filters).
fn desk_to_slot(rect: egui::Rect, pos: egui::Pos2) -> (i32, i32) {
    let (inner, cell_w, cell_h, gap) = desk_metrics(rect);
    let cx = ((pos.x - inner.left()) / (cell_w + gap) - 0.5).round() as i32 - 1;
    let cy = ((pos.y - inner.top()) / (cell_h + gap) - 0.5).round() as i32 - 1;
    (cx.clamp(-1, 1), cy.clamp(-1, 1))
}
