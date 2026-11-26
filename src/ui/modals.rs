use egui::{Color32, Context, CornerRadius, Frame, Margin, Modal, RichText, ScrollArea, Stroke, TextEdit};

use polydesk::config::{
    APP_VERSION, BUILD_TARGET, default_polygon_rpc_url, redact_rpc_url, validate_polygon_rpc_url,
};
use polydesk::ctf::CtfAction;
use polydesk::execution::{TradingMode, truncate_address};
use polydesk::i18n::tr;
use polydesk::odds::format_usd;
use polydesk::vault::WalletSignatureType;

use super::theme::ThemePalette;
use super::{ActiveModal, PolyDeskDesktop, WalletSetupTab};

fn card_frame(pal: ThemePalette) -> Frame {
    Frame::NONE
        .fill(pal.bg_sub)
        .stroke(Stroke::new(1.0, pal.border))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(12))
}

impl PolyDeskDesktop {
    pub fn render_modals(&mut self, ctx: &Context) {
        let Some(modal) = self.modal else {
            return;
        };

        let pal = self.palette();
        let backdrop = Color32::from_black_alpha(if pal.light { 95 } else { 145 });
        let frame = Frame::NONE
            .fill(pal.bg_panel)
            .stroke(Stroke::new(1.0, pal.border_strong))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(14));
        let screen = ctx.content_rect();
        let mut close_clicked = false;

        let modal_resp = match modal {
            ActiveModal::WalletAndDeposit => {
                let title = tr(self.lang, "Wallet & Add Real Money", "本地加密钱包与充值");
                let max_w = (screen.width() - 48.0).clamp(320.0, 640.0);
                let max_h = (screen.height() - 120.0).clamp(240.0, 640.0);
                Modal::new(egui::Id::unique("wallet_deposit_modal"))
                    .backdrop_color(backdrop)
                    .frame(frame)
                    .show(ctx, |ui| {
                        ui.set_min_width(max_w.min(620.0));
                        ui.set_max_width(max_w);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(title).strong().size(15.0).color(pal.text_primary));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text(tr(self.lang, "Close (Esc)", "关闭 (Esc)")).clicked() {
                                    close_clicked = true;
                                }
                            });
                        });
                        ui.separator();
                        ui.add_space(4.0);
                        ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                            self.draw_wallet_and_deposit_modal(ui);
                        });
                    })
            }
            ActiveModal::SettingsAndUpdates => {
                let title = tr(self.lang, "Updates & Keyboard Hotkeys", "自动更新与快捷键");
                let max_w = (screen.width() - 48.0).clamp(320.0, 540.0);
                let max_h = (screen.height() - 120.0).clamp(240.0, 560.0);
                Modal::new(egui::Id::unique("settings_updates_modal"))
                    .backdrop_color(backdrop)
                    .frame(frame)
                    .show(ctx, |ui| {
                        ui.set_min_width(max_w.min(520.0));
                        ui.set_max_width(max_w);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(title).strong().size(15.0).color(pal.text_primary));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text(tr(self.lang, "Close (Esc)", "关闭 (Esc)")).clicked() {
                                    close_clicked = true;
                                }
                            });
                        });
                        ui.separator();
                        ui.add_space(4.0);
                        ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                            self.draw_rpc_section(ui);
                            ui.add_space(10.0);
                            self.draw_updater_section(ui);
                            ui.add_space(10.0);
                            self.draw_hotkeys_section(ui);
                        });
                    })
            }
            ActiveModal::CtfConfirm => {
                let title = tr(self.lang, "Confirm CTF Operation", "确认 CTF 链上 / 模拟操作");
                let max_w = (screen.width() - 48.0).clamp(320.0, 560.0);
                let max_h = (screen.height() - 120.0).clamp(220.0, 520.0);
                Modal::new(egui::Id::unique("ctf_confirm_modal"))
                    .backdrop_color(backdrop)
                    .frame(frame)
                    .show(ctx, |ui| {
                        ui.set_min_width(max_w.min(520.0));
                        ui.set_max_width(max_w);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(title).strong().size(15.0).color(pal.text_primary));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui
                                    .add_enabled(
                                        !self.ctf_in_flight,
                                        egui::Button::new("×").small(),
                                    )
                                    .on_hover_text(tr(self.lang, "Close (Esc)", "关闭 (Esc)"))
                                    .clicked()
                                {
                                    close_clicked = true;
                                }
                            });
                        });
                        ui.separator();
                        ui.add_space(4.0);
                        ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                            self.draw_ctf_confirm_modal(ui);
                        });
                    })
            }
        };

        let close_now = close_clicked
            || (!self.ctf_in_flight && !modal_resp.any_popup_open && modal_resp.should_close());
        if close_now {
            self.modal = None;
            // Clear & zeroize sensitive in-memory UI fields when closing the wallet window
            self.revealed_wallet = None;
            self.confirm_delete_vault = false;
            self.delete_vault_confirm_input.clear();
            self.renaming_profile = false;
            if !self.ctf_in_flight {
                self.pending_ctf = None;
                self.last_ctf_result = None;
            }
            zeroize::Zeroize::zeroize(&mut self.reveal_pass_input);
            zeroize::Zeroize::zeroize(&mut self.wallet_pk_input);
            zeroize::Zeroize::zeroize(&mut self.vault_pass_input);
        }
    }

    fn draw_wallet_and_deposit_modal(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();
        let acc = self.account.clone();

        // Security Guarantee Banner
        Frame::NONE
            .fill(ThemePalette::tint(pal.green, if pal.light { 18 } else { 16 }))
            .stroke(Stroke::new(1.0, ThemePalette::tint(pal.green, 85)))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(tr(
                        l,
                        "100% On-Device Self-Custody: Your secret phrase & private key are generated and encrypted locally (Argon2id + AES-256-GCM) and NEVER leave your device. Only signed EIP-712 orders are sent to Polymarket.",
                        "100% 纯本机非托管：助记词与私钥均在本机随机生成并用 Argon2id + AES-256-GCM 加密保存，永不上传任何服务器；仅在本地完成 EIP-712 签名后发送订单。",
                    ))
                    .small()
                    .color(pal.green_text),
                );
            });

        ui.add_space(8.0);

        // Secret Phrase & Private Key Backup Card (shown immediately after creating a wallet or revealing backup)
        if let Some(w) = self.revealed_wallet.clone() {
            Frame::NONE
                .fill(ThemePalette::tint(pal.amber, if pal.light { 20 } else { 18 }))
                .stroke(Stroke::new(1.0, pal.amber))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(tr(
                                l,
                                "SAVE YOUR BACKUP NOW (100% Local Only)",
                                "请立即抄写保存备份（仅在本机显示）",
                            ))
                            .strong()
                            .size(13.5)
                            .color(pal.amber),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button(tr(l, "I Saved Them — Hide", "已保存，隐藏")).clicked() {
                                self.revealed_wallet = None;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(tr(
                            l,
                            "Write down this 12-word Secret Phrase & Private Key offline. Never share them with anyone. You can import this exact 12-word phrase into MetaMask or Rabby anytime to manage or withdraw funds.",
                            "请离线抄写保存下方的 12 词助记词与私钥，绝不要发给任何人。你随时可以把这 12 个助记词导入 MetaMask / Rabby 钱包自由管理或提现。",
                        ))
                        .small()
                        .color(pal.text_secondary),
                    );
                    ui.add_space(8.0);

                    egui::Grid::new("backup_keys_grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label(RichText::new(tr(l, "Local Address", "本地钱包地址")).strong().small());
                        ui.label(RichText::new(&w.address).monospace().small().color(pal.text_primary));
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(w.address.clone());
                            self.push_toast(tr(l, "Copied local address", "已复制本地钱包地址").into(), false);
                        }
                        ui.end_row();

                        if let Some(ref phrase) = w.mnemonic {
                            ui.label(RichText::new(tr(l, "Secret Phrase (12w)", "12词助记词")).strong().small());
                            ui.add(
                                egui::Label::new(
                                    RichText::new(phrase).monospace().small().color(pal.text_primary),
                                )
                                .wrap(),
                            );
                            if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                                ui.ctx().copy_text(phrase.clone());
                                self.push_toast(tr(l, "Copied 12-word secret phrase", "已复制 12 词助记词").into(), false);
                            }
                            ui.end_row();
                        }

                        ui.label(RichText::new(tr(l, "Private Key", "私钥 (0x...)")).strong().small());
                        let pk_short = if w.private_key.len() > 26 {
                            format!("{}...{}", &w.private_key[..14], &w.private_key[w.private_key.len() - 10..])
                        } else {
                            w.private_key.clone()
                        };
                        ui.label(RichText::new(pk_short).monospace().small().color(pal.red_text))
                            .on_hover_text(tr(l, "Click Copy to copy your full private key", "点击右侧「复制」获取完整私钥"));
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(w.private_key.clone());
                            self.push_toast(tr(l, "Copied private key", "已复制私钥").into(), false);
                        }
                        ui.end_row();
                    });
                });
            ui.add_space(8.0);
        }

        // C1: Explicit warning banner when live orders may still be active or cancel failed
        if acc.live_orders_warning.is_some()
            || (acc.mode == TradingMode::Paper && !acc.live_open_orders.is_empty())
        {
            Frame::NONE
                .fill(ThemePalette::tint(pal.red, if pal.light { 22 } else { 28 }))
                .stroke(Stroke::new(1.0, pal.red))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(tr(
                            l,
                            "LIVE ORDERS MAY STILL BE ACTIVE ON POLYMARKET",
                            "警告：实盘挂单可能仍在 POLYMARKET 生效",
                        ))
                        .strong()
                        .size(13.0)
                        .color(pal.red_text),
                    );
                    if let Some(ref warn_txt) = acc.live_orders_warning {
                        ui.label(RichText::new(warn_txt).small().color(pal.text_primary));
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "{} {} {}",
                                tr(l, "Cached active live orders:", "缓存未撤实盘挂单数："),
                                acc.live_open_orders.len(),
                                tr(l, "order(s)", "笔")
                            ))
                            .small()
                            .color(pal.text_primary),
                        );
                    }
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if acc.live_connected {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(tr(l, "Retry Cancel All", "重试全撤实盘挂单"))
                                            .strong()
                                            .color(pal.red_text),
                                    )
                                    .stroke(Stroke::new(1.0, pal.red)),
                                )
                                .clicked()
                            {
                                self.trigger_retry_cancel_all_live();
                            }
                            if ui
                                .button(tr(
                                    l,
                                    "Force Lock Keys (orders stay on exchange)",
                                    "强制锁定私钥（挂单保留在交易所）",
                                ))
                                .clicked()
                            {
                                self.revealed_wallet = None;
                                self.trigger_force_lock_wallet();
                            }
                        } else if acc.live_open_orders.is_empty()
                            && ui.button(tr(l, "Dismiss Warning", "忽略警告")).clicked()
                        {
                            self.core.execution.clear_live_warning_if_empty();
                        }
                    });
                });
            ui.add_space(8.0);
        }

        // 1. Paper Simulator Card
        card_frame(pal).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Paper Simulator", "模拟盘账户")).strong().size(13.5).color(pal.amber));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format_usd(acc.paper.cash_usd, false)).monospace().strong().size(13.5));
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr(
                    l,
                    "Practice against real live Polymarket L2 books with fake pUSD before using real funds.",
                    "对接 Polymarket 实时盘口零风险练习下单，熟悉后再切换实盘。",
                ))
                .small()
                .color(pal.text_muted),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut self.paper_reset_input).desired_width(96.0));
                if ui.button(tr(l, "Reset Balance", "重置模拟余额")).clicked() {
                    let bal = self.paper_reset_input.parse::<f64>().unwrap_or(1000.0);
                    self.core.execution.reset_paper(bal);
                    self.push_toast(format!("Paper balance reset to {}", format_usd(bal, false)), false);
                }
                if acc.live_connected && acc.mode == TradingMode::Live {
                    if ui.button(tr(l, "Switch to PAPER", "切回模拟盘")).clicked() {
                        self.trigger_set_mode(TradingMode::Paper);
                    }
                } else if acc.live_connected && acc.mode == TradingMode::Paper {
                    if ui.button(RichText::new(tr(l, "Switch to LIVE", "切换实盘")).color(pal.green)).clicked() {
                        self.trigger_set_mode(TradingMode::Live);
                    }
                }
            });
        });

        ui.add_space(8.0);

        // 1b. Multi-Wallet Vault Profiles Switcher
        card_frame(pal).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(tr(l, "Vault Profiles (Multi-Wallet)", "多钱包配置文件"))
                        .strong()
                        .size(13.5)
                        .color(pal.blue),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("{}: {}", tr(l, "Active", "当前"), acc.active_vault_profile))
                            .monospace()
                            .small()
                            .strong()
                            .color(pal.text_primary),
                    );
                });
            });
            ui.add_space(6.0);

            let mut entries: Vec<(String, Option<String>)> = acc
                .vault_profiles
                .iter()
                .map(|e| (e.name.clone(), e.metadata.address_hint.clone()))
                .collect();
            if !entries.iter().any(|(n, _)| n == &acc.active_vault_profile) {
                entries.push((acc.active_vault_profile.clone(), acc.vault.address_hint.clone()));
            }
            let mut switch_to: Option<String> = None;
            ui.horizontal_wrapped(|ui| {
                for (name, hint) in entries {
                    let selected = name == acc.active_vault_profile;
                    let label = match hint {
                        Some(h) if !h.is_empty() => format!("{name} ({h})"),
                        _ => format!("{name} ({})", tr(l, "empty", "未设密钥")),
                    };
                    let btn = egui::Button::new(
                        RichText::new(label)
                            .monospace()
                            .small()
                            .color(if selected { pal.blue } else { pal.text_primary }),
                    )
                    .fill(if selected {
                        ThemePalette::tint(pal.blue, if pal.light { 24 } else { 32 })
                    } else {
                        pal.bg_card
                    })
                    .stroke(Stroke::new(1.0, if selected { pal.blue } else { pal.border }));
                    if ui.add(btn).clicked() && !selected {
                        switch_to = Some(name);
                    }
                }
            });
            if let Some(target) = switch_to {
                self.trigger_switch_vault_profile(target);
            }

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    TextEdit::singleline(&mut self.new_profile_input)
                        .hint_text(tr(l, "new-profile (a-z, 0-9, _-)", "新建配置名 (字母/数字/_-)"))
                        .desired_width(185.0),
                );
                if ui.button(tr(l, "Switch / Create", "切换 / 新建")).clicked() {
                    let target = std::mem::take(&mut self.new_profile_input);
                    self.trigger_switch_vault_profile(target);
                }
                if !self.renaming_profile && ui.button(tr(l, "Rename Active", "重命名当前")).clicked() {
                    self.renaming_profile = true;
                    self.rename_profile_input = acc.active_vault_profile.clone();
                }
            });
            if self.renaming_profile {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr(l, "New name:", "新名称:")).small());
                    ui.add(TextEdit::singleline(&mut self.rename_profile_input).desired_width(150.0));
                    if ui.button(tr(l, "Save", "保存")).clicked() {
                        let old = acc.active_vault_profile.clone();
                        let new_n = std::mem::take(&mut self.rename_profile_input);
                        self.renaming_profile = false;
                        self.trigger_rename_vault_profile(old, new_n);
                    }
                    if ui.button(tr(l, "Cancel", "取消")).clicked() {
                        self.renaming_profile = false;
                        self.rename_profile_input.clear();
                    }
                });
            }
        });

        ui.add_space(8.0);

        // 2. Local Encrypted Live Vault (Create New Random Wallet OR Import Private Key/Phrase)
        card_frame(pal).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · [{}]",
                        tr(l, "Live Trading Vault (Local Only)", "实盘本地加密钱包"),
                        acc.active_vault_profile
                    ))
                    .strong()
                    .size(13.5)
                    .color(pal.green),
                );
                if acc.live_connected {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let addr = acc.wallet_address.as_deref().unwrap_or("LIVE");
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(addr.to_string());
                            self.push_toast(tr(l, "Copied wallet address", "已复制钱包地址").into(), false);
                        }
                        ui.label(RichText::new(addr).monospace().small().color(pal.green_text));
                    });
                }
            });
            ui.add_space(6.0);

            if acc.live_connected {
                let live_bal = acc.live_balance_usd.map(|b| format_usd(b, false)).unwrap_or_else(|| "—".into());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{}: {}", tr(l, "Live pUSD Balance", "实盘 pUSD 余额"), live_bal)).monospace().strong());
                    ui.add_space(8.0);
                    if ui.button(tr(l, "Sync Balance & Allowance", "刷新余额与授权")).clicked() {
                        self.trigger_sync_allowance();
                    }
                    if ui.button(tr(l, "Lock Wallet", "锁定钱包")).clicked() {
                        self.revealed_wallet = None;
                        self.trigger_lock_wallet();
                    }
                });

                if acc.vault.exists {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            TextEdit::singleline(&mut self.reveal_pass_input)
                                .password(true)
                                .hint_text(tr(l, "Vault password to reveal backup", "输入加密密码查看助记词/私钥"))
                                .desired_width(240.0),
                        );
                        if ui.button(tr(l, "Reveal Phrase / Key", "查看助记词/私钥")).clicked() {
                            self.trigger_reveal_backup();
                        }
                    });
                }
            } else if acc.vault.exists {
                ui.label(format!(
                    "{}: {}",
                    tr(l, "Encrypted local vault found", "检测到本机加密钱包"),
                    acc.vault.address_hint.as_deref().unwrap_or("encrypted")
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.vault_pass_input).password(true).hint_text(tr(l, "Vault password", "输入解密密码")).desired_width(220.0));
                    if ui.button(RichText::new(tr(l, "Unlock Live", "解锁实盘")).color(pal.green)).clicked() {
                        self.confirm_delete_vault = false;
                        self.delete_vault_confirm_input.clear();
                        self.trigger_unlock_vault();
                    }
                    if !self.confirm_delete_vault
                        && ui.button(tr(l, "Delete Vault", "删除重设")).clicked()
                    {
                        self.confirm_delete_vault = true;
                        self.delete_vault_confirm_input.clear();
                    }
                });
                if self.confirm_delete_vault {
                    ui.add_space(6.0);
                    Frame::NONE
                        .fill(ThemePalette::tint(pal.red, if pal.light { 16 } else { 22 }))
                        .stroke(Stroke::new(1.0, pal.red))
                        .corner_radius(CornerRadius::same(4))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(format!(
                                    "{} [{}]",
                                    tr(
                                        l,
                                        "Permanent action! Type DELETE below to erase profile:",
                                        "危险操作！请在下方输入 DELETE 确认永久删除配置：",
                                    ),
                                    acc.active_vault_profile
                                ))
                                .small()
                                .color(pal.red_text),
                            );
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                ui.add(
                                    TextEdit::singleline(&mut self.delete_vault_confirm_input)
                                        .hint_text("DELETE")
                                        .desired_width(120.0),
                                );
                                let ready = self.delete_vault_confirm_input.trim() == "DELETE";
                                if ui
                                    .add_enabled(
                                        ready,
                                        egui::Button::new(
                                            RichText::new(tr(l, "Confirm Delete", "确认删除"))
                                                .color(pal.red_text),
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.revealed_wallet = None;
                                    self.confirm_delete_vault = false;
                                    self.delete_vault_confirm_input.clear();
                                    self.trigger_wipe_vault();
                                }
                                if ui.button(tr(l, "Cancel", "取消")).clicked() {
                                    self.confirm_delete_vault = false;
                                    self.delete_vault_confirm_input.clear();
                                }
                            });
                        });
                }
            } else {
                // Clean 2-tab setup: Generate Random Local Wallet vs Import Private Key / Phrase
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut self.wallet_setup_tab,
                        WalletSetupTab::CreateNew,
                        RichText::new(tr(l, "Create New Local Wallet", "一键生成随机本地钱包")).strong(),
                    );
                    ui.selectable_value(
                        &mut self.wallet_setup_tab,
                        WalletSetupTab::ImportExisting,
                        RichText::new(tr(l, "Import Private Key / Phrase", "导入已有私钥 / 助记词")).strong(),
                    );
                });
                ui.add_space(8.0);

                match self.wallet_setup_tab {
                    WalletSetupTab::CreateNew => {
                        ui.label(
                            RichText::new(tr(
                                l,
                                "Generates a brand-new random Polygon address, 12-word Secret Recovery Phrase, and Private Key locally on your device, and automatically creates direct USDT / USDC deposit addresses for it.",
                                "在本地随机生成全新的 Polygon 钱包地址、12 词助记词与私钥，并自动生成专属 USDT / USDC 直充地址，转入即可开始实盘交易。",
                            ))
                            .small()
                            .color(pal.text_muted),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(tr(l, "Local Vault Password (≥12 chars)", "设置本机加密密码 (≥12位)"));
                            ui.add(
                                TextEdit::singleline(&mut self.vault_pass_input)
                                    .password(true)
                                    .hint_text(tr(l, "Min 12 characters", "至少12位密码"))
                                    .desired_width(220.0),
                            );
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.wallet_persist_vault, tr(l, "Save encrypted locally", "加密保存到本机"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(
                                    egui::Button::new(
                                        RichText::new(tr(l, "Generate Random Local Wallet", "生成随机本地钱包"))
                                            .strong()
                                            .color(pal.green),
                                    )
                                    .fill(ThemePalette::tint(pal.green, 22))
                                    .stroke(Stroke::new(1.0, pal.green))
                                    .corner_radius(CornerRadius::same(4)),
                                ).clicked() {
                                    self.trigger_create_new_wallet();
                                }
                            });
                        });
                    }
                    WalletSetupTab::ImportExisting => {
                        egui::Grid::new("vault_grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                            ui.label(tr(l, "Wallet Type", "钱包类型"));
                            egui::ComboBox::from_id_salt("sig_type_cb")
                                .width(340.0)
                                .selected_text(format!("{:?}", self.wallet_sig_type))
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut self.wallet_sig_type, WalletSignatureType::Eoa, "EOA (MetaMask / Rabby / Private Key / Phrase)");
                                    ui.selectable_value(&mut self.wallet_sig_type, WalletSignatureType::Proxy, "Polymarket Proxy Wallet");
                                    ui.selectable_value(&mut self.wallet_sig_type, WalletSignatureType::GnosisSafe, "Polymarket Gnosis Safe");
                                    ui.selectable_value(&mut self.wallet_sig_type, WalletSignatureType::Poly1271, "Poly1271 / Deposit Wallet");
                                });
                            ui.end_row();

                            ui.label(tr(l, "Private Key or 12w Phrase", "私钥 (0x...) 或 12词助记词"));
                            ui.add(
                                TextEdit::singleline(&mut self.wallet_pk_input)
                                    .password(true)
                                    .hint_text("0x... or 12-word recovery phrase")
                                    .desired_width(340.0),
                            );
                            ui.end_row();

                            ui.label(tr(l, "Proxy / Funder (optional)", "代理地址 (EOA可留空)"));
                            ui.add(TextEdit::singleline(&mut self.wallet_funder_input).desired_width(340.0));
                            ui.end_row();

                            ui.label(tr(l, "Local Vault Password (≥12 chars)", "本地加密密码 (≥12位)"));
                            ui.add(
                                TextEdit::singleline(&mut self.vault_pass_input)
                                    .password(true)
                                    .hint_text(tr(l, "Min 12 chars (if saving locally)", "加密保存需至少12位"))
                                    .desired_width(340.0),
                            );
                            ui.end_row();
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.wallet_persist_vault, tr(l, "Save encrypted locally", "加密保存到本机"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(RichText::new(tr(l, "Unlock LIVE Wallet", "解锁并进入实盘")).strong().color(pal.green)).clicked() {
                                    self.trigger_connect_wallet();
                                }
                            });
                        });
                    }
                }
            }

            ui.add_space(6.0);
            let mut hb = self.core.config.read().enable_heartbeats;
            if ui.checkbox(&mut hb, tr(l, "Dead-Man's-Switch Heartbeats (auto-cancel open orders if disconnected)", "开启断线防失控心跳（断网自动撤销全部挂单）")).changed() {
                let _ = self.core.execution.update_config_and_save(|c| c.enable_heartbeats = hb);
            }
        });

        ui.add_space(8.0);

        // 3. Direct USDT / USDC Deposit (Official Polymarket Bridge)
        card_frame(pal).show(ui, |ui| {
            ui.label(
                RichText::new(tr(
                    l,
                    "Direct USDT / USDC Deposit (Official Polymarket Bridge)",
                    "直接转入 USDT / USDC 充值 (Polymarket 官方跨链桥)",
                ))
                .strong()
                .size(13.5)
                .color(pal.blue),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr(
                    l,
                    "Send USDT or USDC directly from Binance, OKX, Coinbase, MetaMask, or Phantom to your personal Bridge deposit address below (Polygon / BSC / Base / Arbitrum / Tron TRC-20 / Solana, min $2) — Polymarket auto-converts 1:1 into Polygon pUSD inside your local wallet. Then click Sync Balance & Allowance.",
                    "直接从币安 / 欧易 OKX / Coinbase / MetaMask / Phantom 向下方专属跨链桥地址转入 USDT 或 USDC（支持 Polygon / BSC / Base / Arbitrum / 波场 TRC-20 / Solana，最低仅 $2），Polymarket 会自动 1:1 转为 Polygon pUSD 到账你的本地钱包，然后点击「刷新余额与授权」即可实盘交易。",
                ))
                .small()
                .color(pal.text_muted),
            );
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if self.deposit_wallet_input.is_empty()
                    && let Some(addr) = acc.funder_address.as_ref().or(acc.wallet_address.as_ref())
                {
                    self.deposit_wallet_input = addr.clone();
                }
                ui.add(TextEdit::singleline(&mut self.deposit_wallet_input).hint_text("0xYourPolygonOrProxyAddress...").desired_width(360.0));
                if ui.button(RichText::new(tr(l, "Get USDT/USDC Addresses", "获取 USDT/USDC 充值地址")).color(pal.blue)).clicked() {
                    self.trigger_fetch_deposit();
                }
            });

            if let Some(dep) = self.deposit_info.clone() {
                ui.add_space(8.0);
                egui::Grid::new("dep_addr_grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("EVM · USDT / USDC (Poly/BSC/Base/Arb/ETH)").strong().small());
                    ui.label(RichText::new(&dep.evm_address).monospace().small().color(pal.green_text));
                    if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                        ui.ctx().copy_text(dep.evm_address.clone());
                        self.push_toast("Copied EVM USDT/USDC deposit address".into(), false);
                    }
                    ui.end_row();

                    if let Some(ref tron) = dep.tron_address {
                        ui.label(RichText::new("Tron TRC-20 · USDT (min $2)").strong().small());
                        ui.label(RichText::new(tron).monospace().small().color(pal.red_text));
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(tron.clone());
                            self.push_toast("Copied Tron TRC-20 USDT address".into(), false);
                        }
                        ui.end_row();
                    }

                    ui.label(RichText::new("Solana · USDT / USDC / SOL").strong().small());
                    ui.label(RichText::new(&dep.solana_address).monospace().small().color(pal.blue));
                    if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                        ui.ctx().copy_text(dep.solana_address.clone());
                        self.push_toast("Copied Solana USDT/USDC address".into(), false);
                    }
                    ui.end_row();

                    ui.label(RichText::new("Bitcoin · BTC (min $3.6)").strong().small());
                    ui.label(RichText::new(&dep.btc_address).monospace().small().color(pal.amber));
                    if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                        ui.ctx().copy_text(dep.btc_address.clone());
                        self.push_toast("Copied BTC deposit address".into(), false);
                    }
                    ui.end_row();
                });
            }
        });
    }

    fn draw_ctf_confirm_modal(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();
        let acc = self.account.clone();

        if let Some(res) = self.last_ctf_result.clone() {
            card_frame(pal).show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} · [{}]",
                        tr(l, "CTF Operation Complete", "CTF 操作已完成"),
                        res.mode_label
                    ))
                    .strong()
                    .size(14.0)
                    .color(pal.green),
                );
                ui.add_space(6.0);
                ui.label(RichText::new(&res.summary).color(pal.text_primary));
                if let Some(ref tx_hash) = res.tx_hash {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Tx:").small().color(pal.text_muted));
                        ui.label(RichText::new(tx_hash).monospace().small().color(pal.green_text));
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(tx_hash.clone());
                            self.push_toast(tr(l, "Copied transaction hash", "已复制交易哈希").into(), false);
                        }
                    });
                }
                if let Some(ref url) = res.polygonscan_url {
                    ui.add_space(4.0);
                    ui.hyperlink_to(
                        tr(l, "View on Polygonscan ↗", "在 Polygonscan 查看交易 ↗"),
                        url,
                    );
                }
                ui.add_space(10.0);
                if ui.button(RichText::new(tr(l, "Done", "完成")).strong()).clicked() {
                    self.last_ctf_result = None;
                    self.pending_ctf = None;
                    self.modal = None;
                }
            });
            return;
        }

        let Some(req) = self.pending_ctf.clone() else {
            ui.label(RichText::new(tr(l, "No pending CTF operation.", "当前没有待确认的 CTF 操作。")).color(pal.text_muted));
            if ui.button(tr(l, "Close", "关闭")).clicked() {
                self.modal = None;
            }
            return;
        };

        let is_live = acc.mode == TradingMode::Live;
        let action_label = match req.action {
            CtfAction::Split => tr(l, "CTF Split (pUSD → YES + NO)", "CTF 拆分 (pUSD → YES + NO)"),
            CtfAction::Merge => tr(l, "CTF Merge (YES + NO → pUSD)", "CTF 合并 (YES + NO → pUSD)"),
            CtfAction::Redeem => tr(l, "CTF 1-Click Redeem Winnings", "CTF 一键赎回获胜代币"),
        };

        Frame::NONE
            .fill(ThemePalette::tint(if is_live { pal.red } else { pal.amber }, if pal.light { 18 } else { 24 }))
            .stroke(Stroke::new(1.0, if is_live { pal.red } else { pal.amber }))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(if is_live {
                        tr(
                            l,
                            "2-Step On-Chain Confirmation: This submits a signed Polygon transaction using your unlocked EOA key and consumes POL gas.",
                            "二次确认：即将使用当前已解锁的 EOA 私钥发送 Polygon 链上交易，并消耗少量 POL 作为 Gas。",
                        )
                    } else {
                        tr(
                            l,
                            "Paper Simulation Confirmation: Conserves simulated pUSD + matched outcome pairs risk-free.",
                            "模拟盘确认：本地等值模拟拆分/合并（pUSD 与双边配对数量严格守恒），无链上风险。",
                        )
                    })
                    .small()
                    .color(if is_live { pal.red_text } else { pal.amber }),
                );
            });
        ui.add_space(8.0);

        card_frame(pal).show(ui, |ui| {
            egui::Grid::new(egui::Id::unique("ctf_confirm_grid"))
                .num_columns(2)
                .spacing([14.0, 6.0])
                .show(ui, |ui| {
                    ui.label(RichText::new(tr(l, "Action", "操作")).small().color(pal.text_muted));
                    ui.label(RichText::new(action_label).strong().color(pal.blue));
                    ui.end_row();

                    ui.label(RichText::new(tr(l, "Mode", "模式")).small().color(pal.text_muted));
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(if is_live { "LIVE · Polygon" } else { "PAPER" })
                                .monospace()
                                .strong()
                                .color(if is_live { pal.green } else { pal.amber }),
                        );
                        if req.neg_risk {
                            ui.label(RichText::new("NegRisk").monospace().small().color(pal.amber));
                        }
                    });
                    ui.end_row();

                    ui.label(RichText::new(tr(l, "Market", "市场")).small().color(pal.text_muted));
                    ui.label(RichText::new(&req.market_question).color(pal.text_primary));
                    ui.end_row();

                    ui.label(RichText::new("Condition ID").small().color(pal.text_muted));
                    ui.label(RichText::new(truncate_address(&req.condition_id)).monospace().small());
                    ui.end_row();

                    ui.label(RichText::new(tr(l, "Amount", "数量")).small().color(pal.text_muted));
                    let amt_text = match req.action {
                        CtfAction::Split => format!("${:.2} pUSD -> {:.2} {} + {:.2} {}", req.amount, req.amount, req.yes_outcome, req.amount, req.no_outcome),
                        CtfAction::Merge => format!("{:.2} {} + {:.2} {} -> ${:.2} pUSD", req.amount, req.yes_outcome, req.amount, req.no_outcome, req.amount),
                        CtfAction::Redeem => format!("{}: {:.2} · {}: {:.2}", req.yes_outcome, req.yes_shares, req.no_outcome, req.no_shares),
                    };
                    ui.label(RichText::new(amt_text).monospace().strong());
                    ui.end_row();

                    if is_live {
                        ui.label(RichText::new(tr(l, "EOA Wallet", "EOA 钱包")).small().color(pal.text_muted));
                        let addr = acc.wallet_address.as_deref().map(truncate_address).unwrap_or_else(|| "—".into());
                        ui.label(RichText::new(addr).monospace().color(pal.green_text));
                        ui.end_row();

                        ui.label(RichText::new("Polygon RPC").small().color(pal.text_muted));
                        let redacted = redact_rpc_url(&self.core.config.read().polygon_rpc_url);
                        ui.label(RichText::new(redacted).monospace().small().color(pal.text_muted));
                        ui.end_row();
                    }
                });
        });

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let confirm_txt = if self.ctf_in_flight {
                tr(l, "Submitting…", "正在提交…")
            } else if is_live {
                tr(l, "Confirm & Sign On-Chain", "确认并签名上链")
            } else {
                tr(l, "Confirm Paper Execution", "确认模拟执行")
            };
            if ui
                .add_enabled(
                    !self.ctf_in_flight,
                    egui::Button::new(RichText::new(confirm_txt).strong().color(pal.green)),
                )
                .clicked()
            {
                self.trigger_execute_ctf(req);
            }
            if ui
                .add_enabled(!self.ctf_in_flight, egui::Button::new(tr(l, "Cancel", "取消")))
                .clicked()
            {
                self.pending_ctf = None;
                self.modal = None;
            }
            if self.ctf_in_flight {
                ui.spinner();
            }
        });
    }

    fn draw_rpc_section(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();
        card_frame(pal).show(ui, |ui| {
            ui.label(
                RichText::new(tr(l, "Polygon RPC Endpoint (CTF On-Chain)", "Polygon RPC 节点（用于 CTF 链上交互）"))
                    .strong()
                    .size(13.5)
                    .color(pal.text_primary),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label("HTTPS RPC:");
                ui.add(
                    TextEdit::singleline(&mut self.polygon_rpc_input)
                        .hint_text("https://polygon-bor-rpc.publicnode.com")
                        .desired_width(265.0),
                );
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(tr(l, "Save RPC", "保存节点")).clicked() {
                    let trimmed = self.polygon_rpc_input.trim().to_string();
                    match validate_polygon_rpc_url(&trimmed) {
                        Err(e) => {
                            self.push_toast(e.to_string(), true);
                        }
                        Ok(valid) => {
                            match self.core.execution.update_config_and_save(|c| c.polygon_rpc_url = valid) {
                                Ok(saved) => {
                                    self.polygon_rpc_input = saved.polygon_rpc_url;
                                    self.push_toast(
                                        tr(l, "Saved Polygon RPC URL", "已保存 Polygon RPC 节点").into(),
                                        false,
                                    );
                                }
                                Err(e) => self.push_toast(e.to_string(), true),
                            }
                        }
                    }
                }
                if ui.button(tr(l, "Reset Default", "恢复默认")).clicked() {
                    let def = default_polygon_rpc_url();
                    self.polygon_rpc_input = def.clone();
                    if let Ok(saved) = self.core.execution.update_config_and_save(|c| c.polygon_rpc_url = def) {
                        self.polygon_rpc_input = saved.polygon_rpc_url;
                    }
                    self.push_toast(tr(l, "Reset Polygon RPC to default", "已恢复默认 Polygon RPC").into(), false);
                }
            });
        });
    }

    fn draw_updater_section(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();
        card_frame(pal).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "GitHub Releases Auto-Updater", "GitHub 自动更新")).strong().size(13.5).color(pal.text_primary));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("v{APP_VERSION} · {BUILD_TARGET}")).monospace().small().color(pal.text_muted));
                });
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(tr(l, "Repo:", "仓库:"));
                ui.add(TextEdit::singleline(&mut self.github_repo_input).hint_text("owner/repo").desired_width(250.0));
                if ui.button(tr(l, "Check Updates", "检查更新")).clicked() {
                    let repo = self.github_repo_input.trim().to_string();
                    let _ = self.core.execution.update_config_and_save(|c| c.github_repo = repo);
                    self.trigger_check_update();
                }
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(tr(
                    l,
                    "Download integrity: verifies SHA-256 against SHA256SUMS.txt fetched from GitHub Releases over HTTPS (not detached code-signed).",
                    "下载完整性校验：通过 HTTPS 比对 GitHub Releases 的 SHA256SUMS.txt（非独立代码签名）。",
                ))
                .small()
                .color(pal.text_muted),
            );
            if let Some(ref u) = self.update_status {
                ui.add_space(6.0);
                if let Some(ref err) = u.error {
                    ui.label(RichText::new(err).small().color(pal.red));
                } else if u.update_available {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("v{} {}", u.latest_version.as_deref().unwrap_or(""), tr(l, "available", "可更新"))).strong().color(pal.green));
                        if ui.button(tr(l, "Verify SHA-256 & Install", "校验 SHA-256 并自动更新")).clicked() {
                            self.trigger_apply_update();
                        }
                    });
                } else {
                    ui.label(RichText::new(tr(l, "You are on the latest version.", "当前已是最新版本。")).small().color(pal.green));
                }
            }
        });
    }

    fn draw_hotkeys_section(&self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();
        card_frame(pal).show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Keyboard Hotkeys (Instant Keydown)", "键盘快捷键（按下即触发）")).strong().size(13.5));
            ui.add_space(6.0);
            let rows = [
                ("B / S", tr(l, "Join Best Bid / Best Ask", "买一 / 卖一挂单")),
                ("Shift+B / Shift+S", tr(l, "Cross Ask (FAK Buy) / Cross Bid (FAK Sell)", "吃卖一 / 砸买一市价单")),
                ("X / Shift+X", tr(l, "Cancel Market / Panic Cancel All", "撤本市场单 / 全局紧急全撤")),
                ("G / F", tr(l, "Green-Up Cash-Out / Flatten Position", "一键锁盈 / 市价平仓")),
                ("Space", tr(l, "Recenter Price Ladder", "盘口居中")),
                ("O / L", tr(l, "Cycle Odds Format / Switch EN-中文", "切换赔率格式 / 切换中英文")),
                ("1 – 5", tr(l, "Quick Stake Presets 1–5", "快速切换 1–5 号金额预设")),
                ("Shift + Click", tr(l, "Force Post-Only Maker on Ladder", "盘口强制只做 Maker 挂单")),
            ];
            egui::Grid::new("hk_grid").num_columns(2).spacing([18.0, 6.0]).show(ui, |ui| {
                for (k, desc) in rows {
                    ui.label(RichText::new(k).monospace().strong().color(pal.blue));
                    ui.label(desc);
                    ui.end_row();
                }
            });
        });
    }
}
