//! Doc/y.html tab-devices: two-column basics+topology, then connected devices.

use crate::app::App;
use crate::theme::Palette;
use crate::ui::widgets::{
    self, digit_boxes, field_width, form_row, ghost_button, pairing_code, primary_button, two_col,
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

    draw_pair_confirm(app, ui, &p);

    // Doc/y.html .two-column-layout
    two_col(ui, |left, right| {
        crate::theme::section_frame(&p).show(left, |ui| {
            ui.label(
                RichText::new(tr("基础参数与配对", "Basics and pairing"))
                    .size(widgets::FORM_FONT + 1.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(12.0);

            form_row(ui, &p, tr("计算机名称", "Computer name"), |ui| {
                if widgets::mac_text_edit(ui, &mut app.cfg.name).lost_focus() {
                    app.queue_save();
                }
            });

            form_row(ui, &p, tr("当前角色", "Role"), |ui| {
                let server = matches!(app.cfg.role, csc_config::Role::Server);
                let current = if server {
                    tr("主机 (控制端)", "Host (controller)")
                } else {
                    tr("从机 (被控端)", "Client (controlled)")
                };
                egui::ComboBox::from_id_salt("role")
                    .selected_text(current)
                    .width(field_width(ui))
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(server, tr("主机 (控制端)", "Host (controller)"))
                            .clicked()
                        {
                            app.cfg.role = csc_config::Role::Server;
                            app.queue_save();
                        }
                        if ui
                            .selectable_label(!server, tr("从机 (被控端)", "Client (controlled)"))
                            .clicked()
                        {
                            app.cfg.role = csc_config::Role::Client;
                            app.queue_save();
                        }
                    });
            });

            form_row(ui, &p, tr("搜索 IP 范围", "Search IP range"), |ui| {
                draw_nic_combo(app, ui);
            });

            form_row(ui, &p, tr("本机配对码", "Pairing code"), |ui| {
                let code = app
                    .status
                    .get("homeCode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("------")
                    .to_string();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    pairing_code(ui, &p, &code);
                    if ghost_button(ui, &p, tr("更换", "Refresh")).clicked() {
                        match crate::ipc_client::roll_home_code() {
                            Ok(_) => {
                                app.reload_status();
                                app.set_ok(tr("已换一组配对码", "New pairing code"));
                            }
                            Err(e) => app.set_err(e.to_string()),
                        }
                    }
                });
            });

            ui.label(
                RichText::new(tr("输入对方配对码", "Enter peer code"))
                    .size(widgets::FORM_FONT)
                    .color(p.text_2),
            );
            ui.add_space(8.0);
            if digit_boxes(ui, &p, &mut app.pair_digits) {
                try_send_pair_code(app);
            }
            ui.add_space(12.0);
        });

        crate::theme::section_frame(&p).show(right, |ui| {
            ui.label(
                RichText::new(tr("屏幕拓扑 (3×3 矩阵)", "Screen topology (3×3)"))
                    .size(widgets::FORM_FONT + 1.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(12.0);
            if is_server {
                draw_desk(app, ui, &p);
            } else {
                widgets::hint(
                    ui,
                    &p,
                    tr(
                        "拓扑由键鼠主机摆放。本机显示配对码供主机输入。",
                        "Layout is set by the keyboard host. Show your code to the host.",
                    ),
                );
                ui.add_space(8.0);
                draw_desk_readonly(app, ui, &p);
            }
        });
    });

    ui.add_space(14.0);

    // Doc/y.html 已连接设备
    crate::theme::section_frame(&p).show(ui, |ui| {
        ui.label(
            RichText::new(tr("已连接设备", "Connected devices"))
                .size(widgets::FORM_FONT + 1.0)
                .strong()
                .color(p.text),
        );
        ui.add_space(10.0);
        draw_device_table(app, ui, &p);
    });
}

fn draw_nic_combo(app: &mut App, ui: &mut egui::Ui) {
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
            let ip = nic.get("ipv4").and_then(|v| v.as_str()).unwrap_or("");
            if ip.is_empty() {
                continue;
            }
            options.push((ip.to_string(), format!("{name}: {ip}")));
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

    // header
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        header_cell(ui, p, tr("设备名", "Name"), 140.0);
        header_cell(ui, p, tr("IP 地址", "IP"), 110.0);
        header_cell(ui, p, tr("延迟", "Latency"), 70.0);
        header_cell(ui, p, tr("指针灵敏度", "Pointer"), 120.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(tr("操作", "Action"))
                    .size(11.0)
                    .color(p.text_2),
            );
        });
    });
    ui.painter().hline(
        ui.max_rect().x_range(),
        ui.cursor().top(),
        egui::Stroke::new(1.0, p.border),
    );
    ui.add_space(4.0);

    for name in peers {
        ui.horizontal(|ui| {
            ui.set_min_height(32.0);
            ui.label(
                RichText::new(&name)
                    .size(11.0)
                    .color(Color32::WHITE)
                    .strong(),
            );
            ui.add_space(12.0);
            let ip = peer_ip(app, &name);
            ui.label(RichText::new(ip).size(11.0).color(p.text_2));
            ui.add_space(12.0);
            ui.label(RichText::new("—").size(11.0).color(p.ok));
            ui.add_space(12.0);
            let mut speed = app
                .cfg
                .pointer
                .peers
                .get(&name)
                .copied()
                .unwrap_or(app.cfg.pointer.speed);
            if ui
                .add(
                    egui::Slider::new(&mut speed, 0.5..=2.0)
                        .show_value(false)
                        .trailing_fill(true),
                )
                .changed()
            {
                app.cfg.pointer.peers.insert(name.clone(), speed);
                app.queue_save();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::danger_text_button(ui, p, tr("断开", "Disconnect")).clicked() {
                    match crate::ipc_client::forget_peer(&name) {
                        Ok(_) => {
                            app.reload_config();
                            app.set_ok(format!("{} {name}", tr("已移除", "Removed")));
                        }
                        Err(e) => app.set_err(e.to_string()),
                    }
                }
            });
        });
        ui.add_space(2.0);
    }
}

fn header_cell(ui: &mut egui::Ui, p: &Palette, text: &str, w: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(w, 18.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(RichText::new(text).size(11.0).color(p.text_2));
        },
    );
}

fn peer_ip(app: &App, name: &str) -> String {
    if let Some(arr) = app.status.get("peers").and_then(|v| v.as_array()) {
        for peer in arr {
            let n = peer
                .get("name")
                .or_else(|| peer.get("hostname"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if n.eq_ignore_ascii_case(name) {
                if let Some(ip) = peer
                    .get("ip")
                    .or_else(|| peer.get("addr"))
                    .and_then(|v| v.as_str())
                {
                    return ip.to_string();
                }
            }
        }
    }
    "—".into()
}

fn try_send_pair_code(app: &mut App) {
    let code: String = app.pair_digits.iter().cloned().collect();
    if code.len() != 6 || code == app.last_pair_sent {
        return;
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

fn draw_pair_confirm(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
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

    egui::Frame::new()
        .fill(p.accent_soft)
        .stroke(egui::Stroke::new(1.0, p.accent))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(16, 14))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!(
                    "{} {}",
                    who,
                    tr(
                        "输入了本机配对码。点确认后才会连接。",
                        "entered this computer’s code. It connects after you confirm."
                    )
                ))
                .color(p.text),
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if primary_button(ui, p, tr("确认", "Confirm")).clicked() {
                    match crate::ipc_client::resolve_peer(&host, "trust", "") {
                        Ok(_) => {
                            app.reload_status();
                            app.set_ok(tr("已确认", "Confirmed"));
                        }
                        Err(e) => app.set_err(e.to_string()),
                    }
                }
                if ghost_button(ui, p, tr("取消", "Cancel")).clicked() {
                    match crate::ipc_client::resolve_peer(&host, "reject", "") {
                        Ok(_) => {
                            app.reload_status();
                            app.set_ok(tr("已取消", "Cancelled"));
                        }
                        Err(e) => app.set_err(e.to_string()),
                    }
                }
            });
        });
    ui.add_space(12.0);
}

fn draw_desk_readonly(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    // same visual grid, interactions disabled
    let _ = app;
    let avail = ui.available_width().max(180.0);
    let h = 155.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail, h), Sense::hover());
    paint_topology_grid(ui, p, rect, &[], &app.cfg.name, None, false);
}

fn draw_desk(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    let host = if app.cfg.name.is_empty() {
        "本机".to_string()
    } else {
        app.cfg.name.clone()
    };
    let avail = ui.available_width().max(180.0);
    let h = 155.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail, h), Sense::click());
    let screens: Vec<(i32, i32, String)> = app
        .cfg
        .screens
        .iter()
        .filter(|s| s.name != host)
        .map(|s| (s.x, s.y, s.name.clone()))
        .collect();
    paint_topology_grid(ui, p, rect, &screens, &host, app.desk_pick, true);

    let placed: Vec<String> = app.cfg.screens.iter().map(|s| s.name.clone()).collect();
    let unplaced: Vec<String> = app
        .cfg
        .trusted_peers
        .iter()
        .filter(|n| !placed.iter().any(|p| p.eq_ignore_ascii_case(n)) && **n != host)
        .cloned()
        .collect();
    if !unplaced.is_empty() {
        ui.add_space(8.0);
        widgets::hint(
            ui,
            p,
            tr(
                "未摆放（点空位再点名字）：",
                "Unplaced (click empty slot, then a name):",
            ),
        );
        ui.horizontal_wrapped(|ui| {
            for name in unplaced {
                if ghost_button(ui, p, &name).clicked() {
                    if let Some((sx, sy)) = app.desk_pick {
                        app.cfg.screens.retain(|s| !(s.x == sx && s.y == sy));
                        app.cfg.screens.push(csc_config::ScreenPlace {
                            name: name.clone(),
                            x: sx,
                            y: sy,
                        });
                        app.desk_pick = None;
                        app.queue_save();
                    } else {
                        app.desk_pending = Some(name);
                    }
                }
            }
        });
    }

    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            if let Some((sx, sy)) = slot_at(rect, pos) {
                if sx == 0 && sy == 0 {
                    return;
                }
                if let Some(name) = app.desk_pending.take() {
                    app.cfg.screens.retain(|s| !(s.x == sx && s.y == sy));
                    app.cfg
                        .screens
                        .retain(|s| !s.name.eq_ignore_ascii_case(&name));
                    app.cfg
                        .screens
                        .push(csc_config::ScreenPlace { name, x: sx, y: sy });
                    app.queue_save();
                } else if let Some(idx) = app
                    .cfg
                    .screens
                    .iter()
                    .position(|s| s.x == sx && s.y == sy && s.name != host)
                {
                    let removed = app.cfg.screens.remove(idx);
                    app.desk_pending = Some(removed.name);
                    app.queue_save();
                } else {
                    app.desk_pick = Some((sx, sy));
                }
            }
        }
    }
}

fn paint_topology_grid(
    ui: &egui::Ui,
    p: &Palette,
    rect: egui::Rect,
    screens: &[(i32, i32, String)],
    host: &str,
    picked: Option<(i32, i32)>,
    _interactive: bool,
) {
    let painter = ui.painter();
    let pad = 8.0;
    let gap = 8.0;
    // Topology grid well
    painter.rect(
        rect,
        8.0,
        p.input,
        egui::Stroke::new(1.0, p.border),
        egui::StrokeKind::Inside,
    );
    let inner = rect.shrink(pad);
    let cell_w = (inner.width() - gap * 2.0) / 3.0;
    let cell_h = (inner.height() - gap * 2.0) / 3.0;

    for sy in -1i32..=1 {
        for sx in -1i32..=1 {
            let col = (sx + 1) as f32;
            let row = (sy + 1) as f32;
            let x = inner.left() + col * (cell_w + gap);
            let y = inner.top() + row * (cell_h + gap);
            let cell = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(cell_w, cell_h));
            let is_host = sx == 0 && sy == 0;
            let peer = screens
                .iter()
                .find(|(px, py, _)| *px == sx && *py == sy)
                .map(|(_, _, n)| n.as_str());
            let is_picked = picked == Some((sx, sy));

            if is_host || peer.is_some() {
                let (fill, stroke, title, sub, fg) = if is_host {
                    (
                        p.ok_soft,
                        egui::Stroke::new(1.0, p.ok),
                        host,
                        tr("(本机)", "(local)"),
                        p.text,
                    )
                } else {
                    let name = peer.unwrap_or("");
                    (
                        p.card,
                        egui::Stroke::new(1.0, if is_picked { p.accent } else { p.border_strong }),
                        name,
                        "",
                        p.text,
                    )
                };
                painter.rect_filled(cell, 5.0, fill);
                painter.rect_stroke(cell, 5.0, stroke, egui::StrokeKind::Inside);
                painter.text(
                    cell.center() - egui::vec2(0.0, if sub.is_empty() { 0.0 } else { 6.0 }),
                    egui::Align2::CENTER_CENTER,
                    title,
                    egui::FontId::proportional(11.0),
                    fg,
                );
                if !sub.is_empty() {
                    painter.text(
                        cell.center() + egui::vec2(0.0, 8.0),
                        egui::Align2::CENTER_CENTER,
                        sub,
                        egui::FontId::proportional(9.0),
                        p.text_2,
                    );
                }
            } else {
                // dashed empty slot
                let stroke = if is_picked {
                    egui::Stroke::new(1.0, p.accent)
                } else {
                    egui::Stroke::new(1.0, p.border)
                };
                painter.rect(
                    cell,
                    6.0,
                    if is_picked {
                        p.accent_soft
                    } else {
                        Color32::TRANSPARENT
                    },
                    stroke,
                    egui::StrokeKind::Inside,
                );
            }
        }
    }
}

fn slot_at(rect: egui::Rect, pos: egui::Pos2) -> Option<(i32, i32)> {
    let pad = 8.0;
    let gap = 8.0;
    let inner = rect.shrink(pad);
    if !inner.contains(pos) {
        return None;
    }
    let cell_w = (inner.width() - gap * 2.0) / 3.0;
    let cell_h = (inner.height() - gap * 2.0) / 3.0;
    let lx = ((pos.x - inner.left()) / (cell_w + gap)).floor() as i32;
    let ly = ((pos.y - inner.top()) / (cell_h + gap)).floor() as i32;
    if (0..3).contains(&lx) && (0..3).contains(&ly) {
        Some((lx - 1, ly - 1))
    } else {
        None
    }
}
