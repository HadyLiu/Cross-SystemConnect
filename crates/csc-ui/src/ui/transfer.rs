//! File receive progress as an egui Viewport (replaces the old WebView window).

use eframe::egui::{self, Color32, RichText, ViewportBuilder, ViewportClass, ViewportId};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub struct TransferUi {
    pub transfers: Vec<Value>,
    /// Batches the user closed; they do not reopen the window.
    hidden: HashSet<u64>,
    /// Batches whose conflict prompt already brought the window forward.
    asked: HashSet<u64>,
    /// Per-batch conflict picks: transfer id -> conflict index -> choice.
    picks: HashMap<u64, HashMap<u64, String>>,
    open: bool,
}

impl TransferUi {
    pub fn is_open(&self) -> bool {
        self.open || !self.transfers.is_empty()
    }

    pub fn set_transfers(&mut self, list: Vec<Value>) {
        let ids: HashSet<u64> = list
            .iter()
            .filter_map(|t| t.get("id").and_then(|v| v.as_u64()))
            .collect();
        self.hidden.retain(|id| ids.contains(id));
        self.asked.retain(|id| ids.contains(id));
        self.picks.retain(|id, _| ids.contains(id));

        let visible: Vec<Value> = list
            .into_iter()
            .filter(|t| {
                t.get("id")
                    .and_then(|v| v.as_u64())
                    .map(|id| !self.hidden.contains(&id))
                    .unwrap_or(true)
            })
            .collect();

        let need_focus = visible.iter().any(|t| {
            let id = t.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            let conflict = t.get("state").and_then(|s| s.as_str()) == Some("conflict");
            conflict && self.asked.insert(id)
        });

        if !visible.is_empty() {
            self.open = true;
        }
        self.transfers = visible;
        if need_focus {
            // Viewport draw will Focus + Visible.
            self.open = true;
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, zh: bool) {
        if !self.open && self.transfers.is_empty() {
            return;
        }
        if self.transfers.is_empty() {
            self.open = false;
            return;
        }

        let builder = ViewportBuilder::default()
            .with_title(if zh {
                "Cross-SystemConnect 文件接收"
            } else {
                "Cross-SystemConnect file receive"
            })
            .with_inner_size([460.0, 340.0])
            .with_min_inner_size([320.0, 160.0]);

        let mut close = false;
        let mut focus = false;
        for t in &self.transfers {
            let id = t.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
            if t.get("state").and_then(|s| s.as_str()) == Some("conflict")
                && self.asked.contains(&id)
            {
                focus = true;
                break;
            }
        }

        let transfers = self.transfers.clone();
        let mut picks = self.picks.clone();

        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("csc_transfer"),
            builder,
            |ui, class| {
                if focus {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Visible(true));
                }
                if class == ViewportClass::EmbeddedWindow {
                    draw_list(ui, &transfers, &mut picks, zh, &mut close);
                } else {
                    egui::CentralPanel::default().show(ui, |ui| {
                        draw_list(ui, &transfers, &mut picks, zh, &mut close);
                    });
                }
                if ui.input(|i| i.viewport().close_requested()) {
                    close = true;
                }
            },
        );

        self.picks = picks;
        if close {
            for t in &self.transfers {
                if let Some(id) = t.get("id").and_then(|v| v.as_u64()) {
                    self.hidden.insert(id);
                    let state = t.get("state").and_then(|s| s.as_str()).unwrap_or("");
                    if matches!(state, "done" | "failed" | "cancelled" | "paused_link_lost") {
                        let _ = crate::ipc_client::dismiss_transfer(id);
                    }
                }
            }
            self.open = false;
            self.transfers.clear();
        }
    }
}

fn draw_list(
    ui: &mut egui::Ui,
    transfers: &[Value],
    picks: &mut HashMap<u64, HashMap<u64, String>>,
    zh: bool,
    _close: &mut bool,
) {
    if transfers.is_empty() {
        ui.label(if zh {
            "没有正在接收的文件"
        } else {
            "No incoming files"
        });
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for t in transfers {
            draw_item(ui, t, picks, zh);
            ui.add_space(8.0);
        }
    });
}

fn draw_item(
    ui: &mut egui::Ui,
    t: &Value,
    picks: &mut HashMap<u64, HashMap<u64, String>>,
    zh: bool,
) {
    let id = t.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
    let state = t.get("state").and_then(|s| s.as_str()).unwrap_or("");
    let title = t.get("title").and_then(|s| s.as_str()).unwrap_or("?");
    let peer = t.get("peer").and_then(|s| s.as_str()).unwrap_or("");
    let via = t.get("via").and_then(|s| s.as_str()).unwrap_or("");
    let bytes_done = t.get("bytesDone").and_then(|v| v.as_u64()).unwrap_or(0);
    let bytes_total = t.get("bytesTotal").and_then(|v| v.as_u64()).unwrap_or(0);
    let files_done = t.get("filesDone").and_then(|v| v.as_u64()).unwrap_or(0);
    let files_total = t.get("filesTotal").and_then(|v| v.as_u64()).unwrap_or(0);
    let bps = t.get("bytesPerSec").and_then(|v| v.as_u64()).unwrap_or(0);
    let message = t.get("message").and_then(|s| s.as_str()).unwrap_or("");
    let dest = t.get("dest").and_then(|s| s.as_str()).unwrap_or("");
    let open_path = t.get("openPath").and_then(|s| s.as_str()).unwrap_or("");
    let pct = if bytes_total > 0 {
        ((bytes_done as f32) / (bytes_total as f32)).clamp(0.0, 1.0)
    } else if state == "done" {
        1.0
    } else {
        0.0
    };
    let fin = matches!(state, "done" | "failed" | "cancelled" | "paused_link_lost");
    let bar_color = match state {
        "done" => Color32::from_rgb(0x4c, 0xd4, 0x9a),
        "failed" | "cancelled" => Color32::from_rgb(0xff, 0x6b, 0x6b),
        "paused_link_lost" => Color32::from_rgb(0xe6, 0xb3, 0x5a),
        _ => Color32::from_rgb(0x3b, 0x8c, 0xff),
    };

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let from = if via.is_empty() {
                    peer.to_string()
                } else if zh {
                    format!("{peer}（经 {via}）")
                } else {
                    format!("{peer} (via {via})")
                };
                ui.label(
                    RichText::new(if zh {
                        format!("来自 {from}")
                    } else {
                        format!("from {from}")
                    })
                    .weak(),
                );
            });
        });
        let bar = egui::ProgressBar::new(pct).fill(bar_color);
        ui.add(bar);
        ui.horizontal(|ui| {
            let files = if files_total > 1 {
                format!("{files_done}/{files_total} · ")
            } else {
                String::new()
            };
            ui.label(format!(
                "{files}{} / {}",
                fmt_size(bytes_done),
                fmt_size(bytes_total)
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut right = String::new();
                if state == "running" && bps > 0 {
                    right.push_str(&format!("{}/s ", fmt_size(bps)));
                }
                right.push_str(&state_label(state, zh));
                if let Some(eta) = eta_text(t, zh) {
                    right.push(' ');
                    right.push_str(&eta);
                }
                ui.label(RichText::new(right).weak());
            });
        });
        if !message.is_empty() {
            let color = if state == "failed" {
                Color32::from_rgb(0xff, 0x6b, 0x6b)
            } else {
                Color32::from_rgb(0x8f, 0xa3, 0xba)
            };
            ui.colored_label(color, message);
        } else if !dest.is_empty() && !fin {
            ui.label(
                RichText::new(if zh {
                    format!("保存到 {dest}")
                } else {
                    format!("Saving to {dest}")
                })
                .weak(),
            );
        }

        if let Some(renamed) = t.get("renamed").and_then(|v| v.as_array()) {
            if !renamed.is_empty() {
                egui::CollapsingHeader::new(if zh {
                    format!("{} 个名称已改为本机可用的名称", renamed.len())
                } else {
                    format!("{} names adjusted for this computer", renamed.len())
                })
                .show(ui, |ui| {
                    for r in renamed {
                        let from = r.get("from").and_then(|s| s.as_str()).unwrap_or("?");
                        let to = r.get("to").and_then(|s| s.as_str()).unwrap_or("?");
                        ui.label(format!("{from} → {to}"));
                    }
                });
            }
        }

        if state == "conflict" {
            if let Some(conflicts) = t.get("conflicts").and_then(|v| v.as_array()) {
                if !conflicts.is_empty() {
                    ui.label(if zh {
                        format!("目标文件夹里已有 {} 个同名项目：", conflicts.len())
                    } else {
                        format!("{} name conflicts in the destination:", conflicts.len())
                    });
                    let batch = picks.entry(id).or_default();
                    for c in conflicts {
                        let index = c.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                        let name = c.get("name").and_then(|s| s.as_str()).unwrap_or("?");
                        let is_dir = c
                            .get("isDirectory")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let new_size = c.get("newSize").and_then(|v| v.as_u64()).unwrap_or(0);
                        let old_size = c.get("oldSize").and_then(|v| v.as_u64()).unwrap_or(0);
                        let detail = if is_dir {
                            if zh {
                                "文件夹（替换会合并内容）".to_string()
                            } else {
                                "folder (replace merges contents)".to_string()
                            }
                        } else if zh {
                            format!("新：{}　原有：{}", fmt_size(new_size), fmt_size(old_size))
                        } else {
                            format!(
                                "new {} · existing {}",
                                fmt_size(new_size),
                                fmt_size(old_size)
                            )
                        };
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(name);
                                ui.label(RichText::new(detail).small().weak());
                            });
                            let cur = batch
                                .entry(index)
                                .or_insert_with(|| "keep_both".to_string())
                                .clone();
                            for (val, label_zh, label_en) in [
                                ("skip", "跳过", "Skip"),
                                ("keep_both", "保留两者", "Keep both"),
                                ("replace", "替换", "Replace"),
                            ] {
                                let on = cur == val;
                                if ui
                                    .selectable_label(on, if zh { label_zh } else { label_en })
                                    .clicked()
                                {
                                    batch.insert(index, val.to_string());
                                }
                            }
                        });
                    }
                    if conflicts.len() > 1 {
                        ui.horizontal(|ui| {
                            ui.label(if zh { "全部：" } else { "All:" });
                            for (val, label_zh, label_en) in [
                                ("skip", "跳过", "Skip"),
                                ("keep_both", "保留两者", "Keep both"),
                                ("replace", "替换", "Replace"),
                            ] {
                                if ui.button(if zh { label_zh } else { label_en }).clicked() {
                                    for c in conflicts {
                                        if let Some(index) = c.get("index").and_then(|v| v.as_u64())
                                        {
                                            batch.insert(index, val.to_string());
                                        }
                                    }
                                }
                            }
                        });
                    }
                    if ui
                        .button(RichText::new(if zh { "继续" } else { "Continue" }).strong())
                        .clicked()
                    {
                        let decisions: Vec<Value> = conflicts
                            .iter()
                            .map(|c| {
                                let index = c.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                                let choice = batch
                                    .get(&index)
                                    .cloned()
                                    .unwrap_or_else(|| "keep_both".into());
                                json!({"index": index, "choice": choice})
                            })
                            .collect();
                        let _ = crate::ipc_client::resolve_conflict(id, Value::Array(decisions));
                    }
                }
            }
        }

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if fin {
                    if ui.button(if zh { "关闭" } else { "Dismiss" }).clicked() {
                        let _ = crate::ipc_client::dismiss_transfer(id);
                    }
                    if !dest.is_empty()
                        && ui
                            .button(if zh { "打开文件夹" } else { "Open folder" })
                            .clicked()
                    {
                        let _ = open::that(dest);
                    }
                }
                if state == "done"
                    && !open_path.is_empty()
                    && ui.button(if zh { "打开" } else { "Open" }).clicked()
                {
                    let _ = open::that(open_path);
                }
                if !fin && ui.button(if zh { "取消" } else { "Cancel" }).clicked() {
                    let _ = crate::ipc_client::cancel_transfer(id);
                }
            });
        });
    });
}

fn state_label(state: &str, zh: bool) -> String {
    match (state, zh) {
        ("waiting", true) => "等待中".into(),
        ("waiting", false) => "Waiting".into(),
        ("conflict", true) => "有重名文件".into(),
        ("conflict", false) => "Name conflict".into(),
        ("running", true) => "正在接收".into(),
        ("running", false) => "Receiving".into(),
        ("done", true) => "已完成".into(),
        ("done", false) => "Done".into(),
        ("failed", true) => "失败".into(),
        ("failed", false) => "Failed".into(),
        ("cancelled", true) => "已取消".into(),
        ("cancelled", false) => "Cancelled".into(),
        ("paused_link_lost", true) => "等待重连".into(),
        ("paused_link_lost", false) => "Waiting to reconnect".into(),
        _ => state.to_string(),
    }
}

fn fmt_size(n: u64) -> String {
    let mut n = n as f64;
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut i = 0;
    while n >= 1024.0 && i + 1 < units.len() {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n as u64, units[i])
    } else {
        format!("{n:.1} {}", units[i])
    }
}

fn eta_text(t: &Value, zh: bool) -> Option<String> {
    if t.get("state").and_then(|s| s.as_str()) != Some("running") {
        return None;
    }
    let bps = t.get("bytesPerSec").and_then(|v| v.as_u64()).unwrap_or(0);
    if bps == 0 {
        return None;
    }
    let done = t.get("bytesDone").and_then(|v| v.as_u64()).unwrap_or(0);
    let total = t.get("bytesTotal").and_then(|v| v.as_u64()).unwrap_or(0);
    if total <= done {
        return None;
    }
    let s = ((total - done) as f64 / bps as f64).ceil() as u64;
    Some(if zh {
        if s >= 3600 {
            format!("剩余 {} 小时 {} 分", s / 3600, (s % 3600).div_ceil(60))
        } else if s >= 60 {
            format!("剩余 {} 分 {} 秒", s / 60, s % 60)
        } else {
            format!("剩余 {s} 秒")
        }
    } else if s >= 3600 {
        format!("{}h {}m left", s / 3600, (s % 3600).div_ceil(60))
    } else if s >= 60 {
        format!("{}m {}s left", s / 60, s % 60)
    } else {
        format!("{s}s left")
    })
}
