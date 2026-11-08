use crate::app::App;
use crate::theme::Palette;
use crate::ui::widgets::{
    self, field_width, form_row, mac_drag, mac_text_edit, mac_toggle, two_col,
};
use eframe::egui::{self, RichText};

fn tr<'a>(zh: &'a str, en: &'a str) -> &'a str {
    if crate::locale::is_english() {
        en
    } else {
        zh
    }
}

fn pal() -> Palette {
    Palette::app()
}

fn card_title(ui: &mut egui::Ui, p: &Palette, title: &str) {
    ui.label(
        RichText::new(title)
            .size(widgets::FORM_FONT + 1.0)
            .strong()
            .color(p.text),
    );
    ui.add_space(12.0);
}

/// Doc/y.html tab-sharing: two-column clipboard + file transfer.
pub fn draw_sharing(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();
    two_col(ui, |left, right| {
        draw_clipboard_card(app, left, &p);
        draw_transfer_card(app, right, &p);
    });
}

fn draw_clipboard_card(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("剪贴板无缝同步", "Clipboard sync"));
        form_row(
            ui,
            p,
            tr("启用剪贴板共享", "Enable clipboard"),
            |ui| {
                if mac_toggle(ui, p, &mut app.cfg.clipboard.enabled).clicked() {
                    app.queue_save();
                }
            },
        );
        form_row(ui, p, tr("同步内容格式", "Formats"), |ui| {
            ui.horizontal(|ui| {
                if ui
                    .checkbox(&mut app.cfg.clipboard.text, tr("富文本", "Rich text"))
                    .changed()
                {
                    app.queue_save();
                }
                if ui
                    .checkbox(&mut app.cfg.clipboard.image, tr("图像/截图", "Images"))
                    .changed()
                {
                    app.queue_save();
                }
            });
        });
        form_row(ui, p, tr("文字上限 (MB)", "Text limit (MB)"), |ui| {
            if mac_drag(
                ui,
                egui::DragValue::new(&mut app.cfg.clipboard.max_inline_mb).range(1..=15),
            )
            .changed()
            {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("发送时机", "When to send"), |ui| {
            egui::ComboBox::from_id_salt("push_mode")
                .selected_text(if app.cfg.clipboard.push_mode == "on_switch" {
                    tr("切到该电脑时", "On switch")
                } else {
                    tr("立即发送", "Immediate")
                })
                .width(field_width(ui))
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_value(
                            &mut app.cfg.clipboard.push_mode,
                            "immediate".into(),
                            tr("立即发送", "Immediate"),
                        )
                        .changed()
                    {
                        app.queue_save();
                    }
                    if ui
                        .selectable_value(
                            &mut app.cfg.clipboard.push_mode,
                            "on_switch".into(),
                            tr("切到该电脑时", "On switch"),
                        )
                        .changed()
                    {
                        app.queue_save();
                    }
                });
        });
    });
}

fn draw_transfer_card(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("跨屏拖拽文件传输", "Cross-screen file transfer"));
        form_row(ui, p, tr("启用跨屏传输", "Enable transfer"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.file_transfer.enabled).clicked() {
                app.queue_save();
            }
        });
        form_row(
            ui,
            p,
            tr("单文件上限 (MB)", "Per-file limit (MB)"),
            |ui| {
                if mac_drag(
                    ui,
                    egui::DragValue::new(&mut app.cfg.file_transfer.max_size_mb).speed(10.0),
                )
                .changed()
                {
                    app.queue_save();
                }
            },
        );
        form_row(ui, p, tr("剪贴板文件", "Clipboard files"), |ui| {
            if ui
                .checkbox(
                    &mut app.cfg.clipboard.files,
                    tr("Ctrl+V 传输", "Transfer via Ctrl+V"),
                )
                .changed()
            {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("限速", "Speed limit"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.file_transfer.limit_speed).clicked() {
                app.queue_save();
            }
            if app.cfg.file_transfer.limit_speed {
                ui.add_space(8.0);
                if mac_drag(
                    ui,
                    egui::DragValue::new(&mut app.cfg.file_transfer.max_speed_mibs)
                        .range(1..=10_000)
                        .suffix(" MiB/s"),
                )
                .changed()
                {
                    app.queue_save();
                }
            }
        });
    });
}

/// 切换与热键：快捷键 | 边缘切换 左右分布。
pub fn draw_switch(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();
    two_col(ui, |left, right| {
        draw_hotkeys_card(app, left, &p);
        draw_edge_card(app, right, &p);
    });
}

fn draw_hotkeys_card(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("快捷键", "Shortcuts"));
        widgets::hint(
            ui,
            p,
            tr(
                "在输入框中直接编辑组合键文字，例如 Ctrl+Alt+L。",
                "Edit the combo as text, e.g. Ctrl+Alt+L.",
            ),
        );
        ui.add_space(10.0);
        hotkey_row(app, ui, p, tr("锁定屏幕", "Lock screens"), |c| {
            &mut c.hotkeys.lock_screen
        });
        if matches!(app.cfg.role, csc_config::Role::Server) {
            hotkey_row(app, ui, p, tr("回到本机", "Return home"), |c| {
                &mut c.hotkeys.return_to_server
            });
        }
        hotkey_row(
            app,
            ui,
            p,
            tr("锁定在当前屏幕", "Lock to screen"),
            |c| &mut c.hotkeys.lock_to_screen,
        );
        hotkey_row(app, ui, p, tr("紧急暂停", "Emergency pause"), |c| {
            &mut c.hotkeys.emergency_pause
        });
    });
}

fn draw_edge_card(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("边缘切换", "Edge switching"));
        if !matches!(app.cfg.role, csc_config::Role::Server) {
            widgets::hint(
                ui,
                p,
                tr(
                    "切换规则由键鼠主机设置。",
                    "Switching rules are set by the keyboard host.",
                ),
            );
            return;
        }

        form_row(ui, p, tr("延迟 (ms)", "Delay (ms)"), |ui| {
            if mac_drag(
                ui,
                egui::DragValue::new(&mut app.cfg.switch_cfg.edge_delay_ms).range(0..=2000),
            )
            .changed()
            {
                app.queue_save();
            }
        });
        form_row(
            ui,
            p,
            tr("对角切换区 (px)", "Diagonal zone (px)"),
            |ui| {
                if mac_drag(
                    ui,
                    egui::DragValue::new(&mut app.cfg.switch_cfg.diagonal_zone_px).range(1..=400),
                )
                .changed()
                {
                    app.queue_save();
                }
            },
        );
        form_row(ui, p, tr("二次撞边切换", "Double-tap edge"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.switch_cfg.double_tap_edge).clicked() {
                app.queue_save();
            }
        });
        form_row(
            ui,
            p,
            tr("二次窗口 (ms)", "Double-tap window (ms)"),
            |ui| {
                if mac_drag(
                    ui,
                    egui::DragValue::new(&mut app.cfg.switch_cfg.double_tap_window_ms)
                        .range(50..=2000),
                )
                .changed()
                {
                    app.queue_save();
                }
            },
        );
        form_row(ui, p, tr("按住不切换", "Hold to block"), |ui| {
            let label = match app.cfg.switch_cfg.block_modifier.as_str() {
                "ctrl" => "Ctrl",
                "shift" => "Shift",
                "alt" => "Alt",
                _ => tr("无", "None"),
            };
            egui::ComboBox::from_id_salt("block_mod")
                .selected_text(label)
                .width(field_width(ui))
                .show_ui(ui, |ui| {
                    for (v, lab) in [
                        ("none", tr("无", "None")),
                        ("ctrl", "Ctrl"),
                        ("shift", "Shift"),
                        ("alt", "Alt"),
                    ] {
                        if ui
                            .selectable_value(
                                &mut app.cfg.switch_cfg.block_modifier,
                                v.to_string(),
                                lab,
                            )
                            .changed()
                        {
                            app.queue_save();
                        }
                    }
                });
        });
        form_row(
            ui,
            p,
            tr("全屏时不切换", "Block when fullscreen"),
            |ui| {
                if mac_toggle(ui, p, &mut app.cfg.switch_cfg.block_when_fullscreen).clicked() {
                    app.queue_save();
                }
            },
        );
        form_row(ui, p, tr("尽量不休眠", "Keep awake"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.power.keep_peers_awake).clicked() {
                app.cfg.gui.prevent_sleep = app.cfg.power.keep_peers_awake;
                app.queue_save();
            }
        });
        form_row(ui, p, tr("指针速度", "Pointer speed"), |ui| {
            if mac_drag(
                ui,
                egui::DragValue::new(&mut app.cfg.pointer.speed)
                    .speed(0.05)
                    .range(0.25..=4.0),
            )
            .changed()
            {
                app.queue_save();
            }
        });
    });
}

fn hotkey_row(
    app: &mut App,
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    field: fn(&mut csc_config::Config) -> &mut String,
) {
    form_row(ui, p, label, |ui| {
        let v = field(&mut app.cfg);
        if mac_text_edit(ui, v).changed() {
            app.queue_save();
        }
    });
}

/// 系统与网络：双栏分区。
pub fn draw_system(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();

    two_col(ui, |left, right| {
        draw_net(app, left, &p);
        draw_lang(app, right, &p);
    });
    ui.add_space(14.0);
    two_col(ui, |left, right| {
        draw_privacy(app, left, &p);
        draw_window_tray(app, right, &p);
    });
    ui.add_space(14.0);
    draw_log_and_commands(app, ui, &p);
}

fn draw_net(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("网络端口", "Network ports"));
        form_row(ui, p, tr("发现端口", "Discovery port"), |ui| {
            if mac_drag(
                ui,
                egui::DragValue::new(&mut app.cfg.network.discovery_port).range(1..=65535),
            )
            .changed()
            {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("控制端口", "Control port"), |ui| {
            if mac_drag(
                ui,
                egui::DragValue::new(&mut app.cfg.control_port).range(1..=65535),
            )
            .changed()
            {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("传输端口", "Transfer port"), |ui| {
            if mac_drag(
                ui,
                egui::DragValue::new(&mut app.cfg.data_port).range(1..=65535),
            )
            .changed()
            {
                app.queue_save();
            }
        });
        if app.cfg.control_port == app.cfg.data_port {
            ui.colored_label(
                p.danger,
                tr(
                    "控制端口和传输端口不能相同",
                    "Control and transfer ports must differ",
                ),
            );
            ui.add_space(8.0);
        }
        widgets::hint(
            ui,
            p,
            tr(
                "发现端口用于设备发现与心跳，各机须相同；控制 / 传输默认 24900 / 24901。",
                "Discovery carries peer find + heartbeat (must match). Control / transfer default 24900 / 24901.",
            ),
        );
        ui.add_space(12.0);
        card_title(ui, p, tr("心跳", "Heartbeat"));
        form_row(ui, p, tr("启用心跳", "Enable heartbeat"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.network.heartbeat_enabled).clicked() {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("间隔 (ms)", "Interval (ms)"), |ui| {
            if mac_drag(
                ui,
                egui::DragValue::new(&mut app.cfg.network.heartbeat_ms).range(500..=60_000),
            )
            .changed()
            {
                app.queue_save();
            }
        });
    });
}

fn draw_lang(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("通知", "Notifications"));
        form_row(ui, p, tr("对端连接", "Peer connected"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.notify.connect).clicked() {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("断开 / 失联", "Disconnect / lost"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.notify.disconnect).clicked() {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("文件传输", "File transfer"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.notify.transfer).clicked() {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("错误", "Errors"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.notify.errors).clicked() {
                app.queue_save();
            }
        });
        widgets::hint(
            ui,
            p,
            tr(
                "配对请求总会弹出；连接提醒默认关闭。",
                "Pairing always pops up; connect notices default off.",
            ),
        );
        ui.add_space(12.0);
        card_title(ui, p, tr("界面语言", "Language"));
        form_row(ui, p, tr("语言", "Language"), |ui| {
            let current = match app.cfg.gui.language.as_str() {
                "zh-CN" => "简体中文",
                "en-US" => "English",
                _ => tr("跟随系统", "Follow system"),
            };
            egui::ComboBox::from_id_salt("lang")
                .selected_text(current)
                .width(field_width(ui))
                .show_ui(ui, |ui| {
                    for (v, label) in [
                        ("auto", tr("跟随系统", "Follow system")),
                        ("zh-CN", "简体中文"),
                        ("en-US", "English"),
                    ] {
                        if ui
                            .selectable_value(&mut app.cfg.gui.language, v.to_string(), label)
                            .changed()
                        {
                            crate::locale::apply(&app.cfg.gui.language);
                            app.queue_save();
                        }
                    }
                });
        });
    });
}

fn draw_privacy(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("隐私与诊断", "Privacy & diagnostics"));
        widgets::hint(
            ui,
            p,
            tr(
                "日志不记录按键、剪贴板和文件内容；诊断包指纹只保留前 8 位。",
                "Logs omit keystrokes, clipboard and file contents; diagnostics keep 8-char fingerprints.",
            ),
        );
        ui.add_space(10.0);
        form_row(ui, p, tr("测量模式", "Bench mode"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.diagnostics.bench_enabled).clicked() {
                app.queue_save();
            }
        });
        ui.horizontal(|ui| {
            if widgets::ghost_button(ui, p, tr("导出诊断", "Export diagnostics")).clicked() {
                match crate::ipc_client::export_diagnostics() {
                    Ok(v) => {
                        let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        app.set_ok(format!("{} {path}", tr("已导出", "Exported")));
                    }
                    Err(e) => app.set_err(e.to_string()),
                }
            }
            if widgets::ghost_button(ui, p, tr("重置（保留配对）", "Reset (keep pairing)"))
                .clicked()
            {
                match crate::ipc_client::reset_data(true) {
                    Ok(_) => {
                        app.reload_config();
                        app.set_ok(tr("已重置", "Reset done"));
                    }
                    Err(e) => app.set_err(e.to_string()),
                }
            }
        });
        ui.add_space(8.0);
        if widgets::danger_text_button(ui, p, tr("重置并清除配对", "Reset and clear pairing"))
            .clicked()
        {
            match crate::ipc_client::reset_data(false) {
                Ok(_) => {
                    app.reload_config();
                    app.set_ok(tr("已清除", "Cleared"));
                }
                Err(e) => app.set_err(e.to_string()),
            }
        }
    });
}

fn draw_window_tray(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    crate::theme::section_frame(p).show(ui, |ui| {
        card_title(ui, p, tr("窗口 / 托盘", "Window / tray"));
        form_row(ui, p, tr("启动后驻留托盘", "Stay in tray"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.gui.auto_hide).clicked() {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("关闭后继续驻留", "Close to tray"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.gui.close_to_tray).clicked() {
                app.queue_save();
            }
        });
        form_row(ui, p, tr("开机自启", "Launch at login"), |ui| {
            if mac_toggle(ui, p, &mut app.cfg.gui.launch_at_login).clicked() {
                let _ = crate::autostart::apply(app.cfg.gui.launch_at_login);
                app.queue_save();
            }
        });
    });
}

fn draw_log_and_commands(app: &mut App, ui: &mut egui::Ui, p: &Palette) {
    two_col(ui, |left, right| {
        crate::theme::section_frame(p).show(left, |ui| {
            card_title(ui, p, tr("日志设置", "Log settings"));
            form_row(ui, p, tr("级别", "Level"), |ui| {
                egui::ComboBox::from_id_salt("log_level")
                    .selected_text(app.cfg.log.level.clone())
                    .width(field_width(ui))
                    .show_ui(ui, |ui| {
                        for v in ["debug", "info", "warn", "error"] {
                            if ui
                                .selectable_value(&mut app.cfg.log.level, v.to_string(), v)
                                .changed()
                            {
                                app.cfg.log_level = v.to_string();
                                app.queue_save();
                            }
                        }
                    });
            });
            form_row(ui, p, tr("写入文件", "Write to file"), |ui| {
                if mac_toggle(ui, p, &mut app.cfg.log.to_file).clicked() {
                    app.queue_save();
                }
            });
            form_row(ui, p, tr("路径", "Path"), |ui| {
                let path_w = (field_width(ui) - 100.0).max(64.0);
                if ui
                    .add_sized(
                        egui::vec2(path_w, widgets::FORM_ROW_H),
                        egui::TextEdit::singleline(&mut app.cfg.log.file)
                            .desired_width(path_w)
                            .margin(egui::Margin::symmetric(10, 4))
                            .horizontal_align(egui::Align::LEFT)
                            .vertical_align(egui::Align::Center)
                            .font(egui::FontId::proportional(widgets::FORM_FONT)),
                    )
                    .changed()
                {
                    app.queue_save();
                }
                if widgets::ghost_button(ui, p, tr("浏览…", "Browse…")).clicked() {
                    let start = if app.cfg.log.file.trim().is_empty() {
                        csc_config::default_log_path()
                    } else {
                        std::path::PathBuf::from(app.cfg.log.file.trim())
                    };
                    if let Some(path) = crate::log_picker::pick(&start) {
                        app.cfg.log.file = path.display().to_string();
                        app.queue_save();
                    }
                }
                if widgets::ghost_button(ui, p, tr("默认", "Default")).clicked() {
                    app.cfg.log.file.clear();
                    app.queue_save();
                }
            });
        });

        crate::theme::section_frame(p).show(right, |ui| {
            card_title(ui, p, tr("进入 / 离开命令", "Enter / leave commands"));
            form_row(ui, p, tr("进入本机时执行", "On enter"), |ui| {
                if mac_toggle(ui, p, &mut app.cfg.enable_enter_command).clicked() {
                    app.queue_save();
                }
            });
            form_row(ui, p, tr("进入命令", "Enter command"), |ui| {
                if mac_text_edit(ui, &mut app.cfg.enter_command).changed() {
                    app.queue_save();
                }
            });
            form_row(ui, p, tr("离开本机时执行", "On leave"), |ui| {
                if mac_toggle(ui, p, &mut app.cfg.enable_exit_command).clicked() {
                    app.queue_save();
                }
            });
            form_row(ui, p, tr("离开命令", "Leave command"), |ui| {
                if mac_text_edit(ui, &mut app.cfg.exit_command).changed() {
                    app.queue_save();
                }
            });
        });
    });

    ui.add_space(14.0);
    crate::theme::section_frame(p).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr("实时日志", "Live log"))
                    .size(13.0)
                    .strong()
                    .color(p.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::ghost_button(ui, p, tr("刷新", "Refresh")).clicked() {
                    app.reload_logs();
                }
            });
        });
        ui.add_space(10.0);
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut app.log_text)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .interactive(false),
                );
            });
    });
}

/// Doc/y.html tab-about.
pub fn draw_about(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();
    crate::theme::section_frame(&p).show(ui, |ui| {
        ui.label(
            RichText::new(tr("关于 CrossConnect", "About CrossConnect"))
                .size(12.0)
                .strong()
                .color(p.text),
        );
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            if let Some(tex) = app.logo.as_ref() {
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::from_handle(tex))
                        .fit_to_exact_size(egui::vec2(56.0, 56.0)),
                );
            }
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("Cross-SystemConnect")
                        .size(16.0)
                        .strong()
                        .color(p.text),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")))
                        .size(13.0)
                        .color(p.accent),
                );
            });
        });
        ui.add_space(12.0);
        widgets::hint(
            ui,
            &p,
            tr(
                "基于 Rust 核心与 egui 轻量渲染构筑。协议版本 2：用一台电脑的键盘和鼠标控制局域网里的其他 Windows 或 Linux 电脑，并可以同步剪贴板和文件。",
                "Built with a Rust core and egui. Protocol 2: control other Windows/Linux PCs on the LAN with one keyboard and mouse, and sync clipboard and files.",
            ),
        );
        ui.add_space(12.0);
        if widgets::ghost_button(ui, &p, tr("检查更新", "Check for updates")).clicked() {
            app.set_ok(tr(
                "更新地址还没有配置，所以这里不会联网。",
                "No update address is configured, so this does not use the network.",
            ));
        }
    });
}
