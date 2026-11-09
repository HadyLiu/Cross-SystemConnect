use crate::app::App;
use crate::theme::Palette;
use crate::ui::widgets::{
    self, field_width, mac_number, mac_number_with, mac_text_edit, mac_toggle, outline_button,
    section_heading, settings_card, settings_card_body,
};
use crate::update::UpdateState;
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

/// 共享与文件 — Cursor 风格分区卡片。
pub fn draw_sharing(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();

    section_heading(ui, &p, tr("剪贴板", "Clipboard"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("启用剪贴板共享", "Enable clipboard"),
            tr(
                "在已连接的电脑之间同步剪贴板内容。",
                "Sync clipboard contents across connected computers.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.clipboard.enabled).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("同步富文本", "Rich text"),
            tr("包含格式化文字。", "Include formatted text."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.clipboard.text).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("同步图像 / 截图", "Images / screenshots"),
            tr("跨屏粘贴图片。", "Paste images across screens."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.clipboard.image).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("文字上限 (MB)", "Text limit (MB)"),
            tr(
                "超过此大小的文本不会同步。",
                "Text larger than this is not synced.",
            ),
            |ui| {
                if mac_number(ui, &mut app.cfg.clipboard.max_inline_mb, 1..=15).changed() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("发送时机", "When to send"),
            tr(
                "立即发送，或切到该电脑时再推送。",
                "Send immediately, or when switching to that computer.",
            ),
            |ui| {
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
            },
        );
    });

    section_heading(ui, &p, tr("文件传输", "File transfer"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("启用跨屏传输", "Enable transfer"),
            tr(
                "拖拽或粘贴文件到另一台电脑。",
                "Drag or paste files to another computer.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.file_transfer.enabled).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("单文件上限 (MB)", "Per-file limit (MB)"),
            tr("单个文件的最大体积。", "Maximum size of a single file."),
            |ui| {
                if mac_number(ui, &mut app.cfg.file_transfer.max_size_mb, 1..=1_000_000).changed() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("剪贴板文件", "Clipboard files"),
            tr(
                "允许用 Ctrl+V 传输文件。",
                "Allow transferring files with Ctrl+V.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.clipboard.files).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("限速", "Speed limit"),
            tr(
                "限制传输带宽，避免占满网络。",
                "Cap bandwidth so the link stays usable.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.file_transfer.limit_speed).clicked() {
                    app.queue_save();
                }
                if app.cfg.file_transfer.limit_speed {
                    ui.add_space(8.0);
                    if mac_number_with(
                        ui,
                        &mut app.cfg.file_transfer.max_speed_mibs,
                        1..=10_000,
                        " MiB/s",
                    )
                    .changed()
                    {
                        app.queue_save();
                    }
                }
            },
        );
    });
}

/// 切换与热键。
pub fn draw_switch(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();

    section_heading(ui, &p, tr("快捷键", "Shortcuts"));
    settings_card(ui, &p, |list| {
        hotkey_settings_row(
            app,
            list,
            &p,
            tr("锁定屏幕", "Lock screens"),
            tr(
                "点击右侧按钮后按下组合键；Esc 取消，Backspace 清除。",
                "Click the field, then press the keys. Esc cancels, Backspace clears.",
            ),
            |c| &mut c.hotkeys.lock_screen,
        );
        // Always listed: reset/demote to client used to hide this row and look like
        // the shortcut was deleted. It only acts while this computer is the host.
        hotkey_settings_row(
            app,
            list,
            &p,
            tr("回到本机", "Return home"),
            tr(
                "指针立刻回到键鼠主机。",
                "Return the pointer to the keyboard host.",
            ),
            |c| &mut c.hotkeys.return_to_server,
        );
        hotkey_settings_row(
            app,
            list,
            &p,
            tr("锁定在当前屏幕", "Lock to screen"),
            tr("暂时禁止边缘穿越。", "Temporarily block edge crossing."),
            |c| &mut c.hotkeys.lock_to_screen,
        );
        hotkey_settings_row(
            app,
            list,
            &p,
            tr("紧急暂停", "Emergency pause"),
            tr("立刻停止远程控制。", "Stop remote control immediately."),
            |c| &mut c.hotkeys.emergency_pause,
        );
    });

    section_heading(ui, &p, tr("边缘切换", "Edge switching"));
    if !matches!(app.cfg.role, csc_config::Role::Server) {
        settings_card_body(ui, &p, |ui| {
            widgets::hint(
                ui,
                &p,
                tr(
                    "切换规则由键鼠主机设置。",
                    "Switching rules are set by the keyboard host.",
                ),
            );
        });
        return;
    }

    settings_card(ui, &p, |list| {
        list.row(
            tr("延迟 (ms)", "Delay (ms)"),
            tr(
                "指针贴边多久后切换。",
                "How long the pointer stays on an edge before switching.",
            ),
            |ui| {
                if mac_number(ui, &mut app.cfg.switch_cfg.edge_delay_ms, 0..=2000).changed() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("对角切换区 (px)", "Diagonal zone (px)"),
            tr(
                "角落斜向切换的感应宽度。",
                "Corner zone width for diagonal switches.",
            ),
            |ui| {
                if mac_number(ui, &mut app.cfg.switch_cfg.diagonal_zone_px, 1..=400).changed() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("二次撞边切换", "Double-tap edge"),
            tr(
                "需要连续两次顶到边缘才切换。",
                "Require two edge hits before switching.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.switch_cfg.double_tap_edge).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("二次窗口 (ms)", "Double-tap window (ms)"),
            tr(
                "两次撞边的有效间隔。",
                "Valid interval between the two edge hits.",
            ),
            |ui| {
                if mac_number(ui, &mut app.cfg.switch_cfg.double_tap_window_ms, 50..=2000).changed()
                {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("按住不切换", "Hold to block"),
            tr(
                "按住修饰键时忽略边缘切换。",
                "Ignore edge switches while holding a modifier.",
            ),
            |ui| {
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
            },
        );
        list.row(
            tr("全屏时不切换", "Block when fullscreen"),
            tr(
                "全屏应用时阻止穿屏。",
                "Block crossing while an app is fullscreen.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.switch_cfg.block_when_fullscreen).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("尽量不休眠", "Keep awake"),
            tr(
                "有对端连接时尽量阻止休眠。",
                "Try to prevent sleep while peers are linked.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.power.keep_peers_awake).clicked() {
                    app.cfg.gui.prevent_sleep = app.cfg.power.keep_peers_awake;
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("指针速度", "Pointer speed"),
            tr(
                "远程指针的默认灵敏度。",
                "Default sensitivity for the remote pointer.",
            ),
            |ui| {
                if mac_number(ui, &mut app.cfg.pointer.speed, 0.25..=4.0).changed() {
                    app.queue_save();
                }
            },
        );
    });
}

fn hotkey_settings_row(
    app: &mut App,
    list: &mut widgets::SettingsList<'_, '_>,
    p: &Palette,
    title: &str,
    desc: &str,
    field: fn(&mut csc_config::Config) -> &mut String,
) {
    list.row(title, desc, |ui| {
        let v = field(&mut app.cfg);
        if widgets::hotkey_field(ui, v).changed() {
            app.queue_save();
        }
        let _ = p;
    });
}

/// 系统与网络。
pub fn draw_system(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();

    section_heading(ui, &p, tr("网络端口", "Network ports"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("发现端口", "Discovery port"),
            tr(
                "默认 45433；各机须相同。",
                "Default 45433; must match on every computer.",
            ),
            |ui| {
                if mac_number(ui, &mut app.cfg.network.discovery_port, 1..=65535).changed() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("控制端口", "Control port"),
            tr("默认 45277。", "Default 45277."),
            |ui| {
                if mac_number(ui, &mut app.cfg.control_port, 1..=65535).changed() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("传输端口", "Transfer port"),
            tr(
                "默认 45278，不能与控制端口相同。",
                "Default 45278; must differ from control.",
            ),
            |ui| {
                if mac_number(ui, &mut app.cfg.data_port, 1..=65535).changed() {
                    app.queue_save();
                }
            },
        );
    });
    if app.cfg.control_port == app.cfg.data_port {
        ui.colored_label(
            p.danger,
            tr(
                "控制端口和传输端口不能相同",
                "Control and transfer ports must differ",
            ),
        );
        ui.add_space(12.0);
    }

    section_heading(ui, &p, tr("心跳", "Heartbeat"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("启用心跳", "Enable heartbeat"),
            tr(
                "定期探测对端是否在线。",
                "Periodically probe whether the peer is online.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.network.heartbeat_enabled).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("间隔 (ms)", "Interval (ms)"),
            tr("心跳发送间隔。", "How often heartbeats are sent."),
            |ui| {
                if mac_number(ui, &mut app.cfg.network.heartbeat_ms, 500..=60_000).changed() {
                    app.queue_save();
                }
            },
        );
    });

    section_heading(ui, &p, tr("通知", "Notifications"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("对端连接", "Peer connected"),
            tr(
                "配对请求总会弹出；连接提醒默认关闭。",
                "Pairing always pops up; connect notices default off.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.notify.connect).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("断开 / 失联", "Disconnect / lost"),
            tr("连接中断时提醒。", "Notify when the link drops."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.notify.disconnect).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("文件传输", "File transfer"),
            tr(
                "传输开始或完成时提醒。",
                "Notify when transfers start or finish.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.notify.transfer).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("错误", "Errors"),
            tr("重要错误弹出通知。", "Show notices for important errors."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.notify.errors).clicked() {
                    app.queue_save();
                }
            },
        );
    });

    section_heading(ui, &p, tr("界面语言", "Language"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("语言", "Language"),
            tr("更改后立即生效。", "Takes effect immediately."),
            |ui| {
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
            },
        );
    });

    section_heading(ui, &p, tr("窗口 / 托盘", "Window / tray"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("启动后驻留托盘", "Stay in tray"),
            tr("启动后隐藏主窗口。", "Hide the main window after launch."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.gui.auto_hide).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("关闭后继续驻留", "Close to tray"),
            tr("点关闭时最小化到托盘。", "Closing minimizes to the tray."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.gui.close_to_tray).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("开机自启", "Launch at login"),
            tr("登录系统后自动启动。", "Start automatically after sign-in."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.gui.launch_at_login).clicked() {
                    let _ = crate::autostart::apply(app.cfg.gui.launch_at_login);
                    app.queue_save();
                }
            },
        );
    });

    section_heading(ui, &p, tr("隐私与诊断", "Privacy & diagnostics"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("测量模式", "Bench mode"),
            tr(
                "日志不记录按键、剪贴板和文件内容。",
                "Logs omit keystrokes, clipboard and file contents.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.diagnostics.bench_enabled).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("导出诊断", "Export diagnostics"),
            tr(
                "打包日志与状态以便排查。",
                "Bundle logs and status for troubleshooting.",
            ),
            |ui| {
                if outline_button(ui, &p, tr("导出", "Export")).clicked() {
                    match crate::ipc_client::export_diagnostics() {
                        Ok(v) => {
                            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("");
                            app.set_ok(format!("{} {path}", tr("已导出", "Exported")));
                        }
                        Err(e) => app.set_err(e.to_string()),
                    }
                }
            },
        );
        list.row(
            tr("重置（保留配对）", "Reset (keep pairing)"),
            tr(
                "恢复默认设置，保留已信任设备。",
                "Restore defaults but keep trusted devices.",
            ),
            |ui| {
                if outline_button(ui, &p, tr("重置", "Reset")).clicked() {
                    match crate::ipc_client::reset_data(true) {
                        Ok(_) => {
                            app.reload_config();
                            app.set_ok(tr("已重置", "Reset done"));
                        }
                        Err(e) => app.set_err(e.to_string()),
                    }
                }
            },
        );
        list.row(
            tr("重置并清除配对", "Reset and clear pairing"),
            tr(
                "清除全部设置与信任关系。",
                "Clear all settings and trusted peers.",
            ),
            |ui| {
                if widgets::danger_text_button(ui, &p, tr("清除", "Clear")).clicked() {
                    match crate::ipc_client::reset_data(false) {
                        Ok(_) => {
                            app.reload_config();
                            app.set_ok(tr("已清除", "Cleared"));
                        }
                        Err(e) => app.set_err(e.to_string()),
                    }
                }
            },
        );
    });

    section_heading(ui, &p, tr("日志", "Logging"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("级别", "Level"),
            tr("写入日志的详细程度。", "How verbose the log file is."),
            |ui| {
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
            },
        );
        list.row(
            tr("写入文件", "Write to file"),
            tr("将日志保存到磁盘。", "Save the log to disk."),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.log.to_file).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(
            tr("路径", "Path"),
            tr(
                "留空则使用默认位置。",
                "Leave empty to use the default location.",
            ),
            |ui| {
                let path_w = (field_width(ui) - 20.0).max(64.0);
                if ui
                    .add_sized(
                        egui::vec2(path_w, widgets::FORM_ROW_H),
                        egui::TextEdit::singleline(&mut app.cfg.log.file)
                            .desired_width(path_w)
                            .margin(egui::Margin::symmetric(10, 4))
                            .font(egui::FontId::proportional(widgets::FORM_FONT)),
                    )
                    .changed()
                {
                    app.queue_save();
                }
                if outline_button(ui, &p, tr("浏览", "Browse")).clicked() {
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
            },
        );
    });

    section_heading(ui, &p, tr("进入 / 离开命令", "Enter / leave commands"));
    settings_card(ui, &p, |list| {
        list.row(
            tr("进入本机时执行", "On enter"),
            tr(
                "指针进入本机时运行命令。",
                "Run a command when the pointer enters.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.enable_enter_command).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(tr("进入命令", "Enter command"), "", |ui| {
            if mac_text_edit(ui, &mut app.cfg.enter_command).changed() {
                app.queue_save();
            }
        });
        list.row(
            tr("离开本机时执行", "On leave"),
            tr(
                "指针离开本机时运行命令。",
                "Run a command when the pointer leaves.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.enable_exit_command).clicked() {
                    app.queue_save();
                }
            },
        );
        list.row(tr("离开命令", "Leave command"), "", |ui| {
            if mac_text_edit(ui, &mut app.cfg.exit_command).changed() {
                app.queue_save();
            }
        });
    });

    section_heading(ui, &p, tr("实时日志", "Live log"));
    settings_card_body(ui, &p, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr("最近输出", "Recent output"))
                    .size(13.0)
                    .color(p.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if outline_button(ui, &p, tr("刷新", "Refresh")).clicked() {
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

/// 关于软件。
pub fn draw_about(app: &mut App, ui: &mut egui::Ui) {
    let p = pal();

    section_heading(ui, &p, tr("关于", "About"));
    settings_card(ui, &p, |list| {
        list.row(
            "Cross-SystemConnect",
            &format!(
                "{} v{}",
                tr(
                    "用一台电脑的键盘鼠标控制局域网中的其他电脑。",
                    "Control other PCs on the LAN with one keyboard and mouse."
                ),
                env!("CARGO_PKG_VERSION")
            ),
            |ui| {
                if let Some(tex) = app.logo.as_ref() {
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::from_handle(tex))
                            .fit_to_exact_size(egui::vec2(36.0, 36.0)),
                    );
                }
            },
        );
        let state = app.updater.state();
        let desc = match &state {
            UpdateState::Idle => tr(
                "从 GitHub Releases 查询最新版本，只打开下载页，不自动安装。",
                "Looks up the latest version on GitHub Releases. Opens the download; never installs.",
            )
            .to_string(),
            UpdateState::Checking => tr("正在检查…", "Checking…").to_string(),
            UpdateState::UpToDate(v) => {
                format!("{} v{v}", tr("已是最新版本", "Up to date:"))
            }
            UpdateState::Available(r) => format!(
                "{} v{}（{}）",
                tr("发现新版本", "New version"),
                r.version,
                r.published
            ),
            UpdateState::Failed(e) => format!("{}：{e}", tr("检查失败", "Check failed")),
        };
        list.row(tr("检查更新", "Check for updates"), &desc, |ui| {
            let checking = matches!(state, UpdateState::Checking);
            let label = tr("检查", "Check");
            if ui
                .add_enabled_ui(!checking, |ui| outline_button(ui, &p, label))
                .inner
                .clicked()
            {
                // Manual check: tip in the title bar only; no desktop toast.
                app.updater.check(ui.ctx(), false);
            }
            if let UpdateState::Available(r) = &state {
                if outline_button(ui, &p, tr("发布页", "Release page")).clicked() {
                    let _ = open::that(&r.page);
                }
                if let Some(url) = &r.download {
                    if outline_button(ui, &p, tr("下载", "Download")).clicked() {
                        let _ = open::that(url);
                    }
                }
            }
        });
        list.row(
            tr("启动时检查更新", "Check on startup"),
            tr(
                "启动时查询一次；有新版本发系统通知，并在窗口左上角提示。",
                "Checks once at launch. A new version shows a system notice and a tip at the top left.",
            ),
            |ui| {
                if mac_toggle(ui, &p, &mut app.cfg.gui.check_updates).clicked() {
                    app.queue_save();
                }
            },
        );
    });

    if let UpdateState::Available(r) = app.updater.state() {
        if !r.notes.trim().is_empty() {
            ui.add_space(12.0);
            section_heading(
                ui,
                &p,
                &format!("v{} {}", r.version, tr("更新内容", "notes")),
            );
            settings_card_body(ui, &p, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(r.notes.trim()).size(12.5).color(p.text_2))
                        .wrap(),
                );
            });
        }
    }
}
