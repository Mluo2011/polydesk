use std::sync::atomic::Ordering;

use egui::{Color32, Context, CornerRadius, Frame, Margin, Modal, RichText, ScrollArea, Stroke, TextEdit, Vec2};

use polydesk::config::{
    APP_VERSION, BUILD_TARGET, default_polygon_rpc_url, default_quick_buy_presets, default_stake_presets,
    redact_rpc_url, validate_polygon_rpc_url,
};
use polydesk::ctf::CtfAction;
use polydesk::execution::{TradingMode, truncate_address};
use polydesk::i18n::{Lang, tr};
use polydesk::odds::{OddsFormat, format_odds, format_usd};
use polydesk::vault::WalletSignatureType;

use super::chart::ShortChartView;
use super::theme::{ThemePalette, apply_theme};
use super::{ActiveModal, OrderChoice, PolyDeskDesktop, SettingsTab, WalletSetupTab};

/// Human-readable summary of the endpoints a hot patch would change, or `None` when it
/// carries no endpoint overrides. Rendered next to the "Apply" button so the operator
/// can see the blast radius before consenting.
fn hot_patch_endpoint_diff(ep: &polydesk::updater::HotEndpointOverrides) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |label: &str, value: &Option<String>| {
        if let Some(v) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            parts.push(format!("{label}={v}"));
        }
    };
    push("gamma", &ep.gamma_host);
    push("clob", &ep.clob_host);
    push("data", &ep.data_host);
    push("ws", &ep.ws_market_url);
    push("rpc", &ep.polygon_rpc_url);
    push("binance-fut", &ep.binance_futures_ws_base);
    push("binance-spot", &ep.binance_spot_ws_base);
    (!parts.is_empty()).then(|| parts.join("  "))
}

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
                let title = tr(self.lang, "Settings & Customization", "设置与个性化定制");
                let max_w = (screen.width() - 48.0).clamp(360.0, 680.0);
                let max_h = (screen.height() - 90.0).clamp(320.0, 720.0);
                Modal::new(egui::Id::unique("settings_updates_modal"))
                    .backdrop_color(backdrop)
                    .frame(frame)
                    .show(ctx, |ui| {
                        ui.set_min_width(max_w.min(640.0));
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
                        self.draw_settings_tab_bar(ui);
                        ui.add_space(6.0);
                        ScrollArea::vertical()
                            .max_height(max_h)
                            .min_scrolled_height(max_h.min(540.0))
                            .show(ui, |ui| {
                                self.draw_settings_modal_body(ui);
                            });
                    })
            }
            ActiveModal::OrderConfirm => {
                let title = tr(self.lang, "Confirm Order & Book Impact", "下单确认与盘口冲击评估");
                let max_w = (screen.width() - 48.0).clamp(340.0, 520.0);
                let max_h = (screen.height() - 120.0).clamp(240.0, 560.0);
                Modal::new(egui::Id::unique("order_confirm_modal"))
                    .backdrop_color(backdrop)
                    .frame(frame)
                    .show(ctx, |ui| {
                        ui.set_min_width(max_w.min(480.0));
                        ui.set_max_width(max_w);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(title).strong().size(15.0).color(pal.text_primary));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("×").on_hover_text(tr(self.lang, "Cancel (Esc)", "取消 (Esc)")).clicked() {
                                    close_clicked = true;
                                }
                            });
                        });
                        ui.separator();
                        ui.add_space(4.0);
                        ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                            self.draw_order_confirm_modal(ui);
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
            self.pending_order_confirm = None;
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
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(tr(
                        l,
                        "GitHub Auto-Updater (Hot + Cold)",
                        "GitHub 自动更新（热更新 + 冷更新）",
                    ))
                    .strong()
                    .size(13.5)
                    .color(pal.text_primary),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let hot_badge = match &self.active_hot_patch {
                        Some(hp) => format!("Hot rev #{} ({})", hp.revision, hp.patch_id),
                        None => tr(l, "Hot: none", "热补丁: 无").to_string(),
                    };
                    ui.label(
                        RichText::new(format!("v{APP_VERSION} · {BUILD_TARGET} · {hot_badge}"))
                            .monospace()
                            .small()
                            .color(pal.text_muted),
                    );
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr(
                    l,
                    "• Hot Updates (zero restart): live translations, announcements & verified feed/API endpoints apply instantly.\n• Cold Updates (restart required): native Rust binaries stage silently in the background with SHA-256 verification and switch cleanly on restart.",
                    "• 热更新（无需重启）：译文优化、公告通知与经安全校验的节点/行情源可实时热生效。\n• 冷更新（需重启程序）：原生 Rust 主程序会在后台校验 SHA-256 并预装，点击重启或下次打开即自动升级。",
                ))
                .small()
                .color(pal.text_muted),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(tr(l, "Repo:", "仓库:"));
                ui.add(
                    TextEdit::singleline(&mut self.github_repo_input)
                        .hint_text("Mluo2011/polydirect")
                        .desired_width(235.0),
                );
                let check_lbl = if self.update_check_in_flight {
                    tr(l, "Checking...", "检查中...")
                } else {
                    tr(l, "Check Updates", "立即检查更新")
                };
                if ui
                    .add_enabled(!self.update_check_in_flight, egui::Button::new(check_lbl))
                    .clicked()
                {
                    let repo = self.github_repo_input.trim().to_string();
                    let _ = self.core.execution.update_config_and_save(|c| {
                        c.github_repo = repo;
                        c.github_repo_user_set = Some(true);
                    });
                    self.trigger_check_update(false);
                }
            });

            ui.add_space(6.0);
            if ui
                .checkbox(
                    &mut self.auto_check_updates,
                    tr(
                        l,
                        "Auto-check GitHub Releases (startup + every 30 min)",
                        "启动及每 30 分钟自动检查 GitHub 更新",
                    ),
                )
                .changed()
            {
                let v = self.auto_check_updates;
                let _ = self.core.execution.update_config_and_save(|c| c.auto_check_updates = v);
            }
            if ui
                .checkbox(
                    &mut self.auto_apply_hot_updates,
                    tr(
                        l,
                        "Auto-apply Hot Updates live without restarting (NOT recommended: a hot patch can re-point API endpoints)",
                        "自动静默应用热更新（不推荐：热补丁可变更接口地址）",
                    ),
                )
                .changed()
            {
                let v = self.auto_apply_hot_updates;
                let _ = self.core.execution.update_config_and_save(|c| {
                    c.auto_apply_hot_updates = v;
                    c.auto_apply_hot_updates_opt_in = Some(v);
                });
            }
            if ui
                .checkbox(
                    &mut self.auto_stage_cold_updates,
                    tr(
                        l,
                        "Auto-download & stage Cold Updates in background",
                        "后台自动下载并校验预装冷更新（不打断交易）",
                    ),
                )
                .changed()
            {
                let v = self.auto_stage_cold_updates;
                let _ = self.core.execution.update_config_and_save(|c| c.auto_stage_cold_updates = v);
            }

            ui.add_space(6.0);
            ui.label(
                RichText::new(tr(
                    l,
                    "Download integrity: verifies a detached secp256k1 signature over SHA256SUMS.txt (when a release key is compiled in) and then the SHA-256 of the payload. Release key: ",
                    "下载完整性校验：先验证 SHA256SUMS.txt 的独立 secp256k1 签名（需内置发布公钥），再校验载荷 SHA-256。发布公钥：",
                ))
                .small()
                .color(pal.text_muted),
            );
            let key_state = match polydesk::updater::RELEASE_PUBKEY_HEX {
                Some(k) if !k.trim().is_empty() => {
                    let short = k.trim();
                    let shown = if short.len() > 16 {
                        format!("{}…{}", &short[..10], &short[short.len() - 6..])
                    } else {
                        short.to_string()
                    };
                    (shown, pal.green_text)
                }
                _ => (
                    tr(l, "NOT CONFIGURED (SHA-256 only)", "未配置（仅 SHA-256）").to_string(),
                    pal.amber,
                ),
            };
            ui.label(RichText::new(key_state.0).monospace().small().color(key_state.1));

            // Verification / rate-limit failures must be visible, not silently folded
            // into "up to date".
            if let Some(err) = self
                .update_status
                .as_ref()
                .and_then(|u| u.hot_patch_error.clone())
            {
                ui.add_space(4.0);
                Frame::NONE
                    .fill(ThemePalette::tint(pal.amber, if pal.light { 18 } else { 24 }))
                    .stroke(Stroke::new(1.0, ThemePalette::tint(pal.amber, 140)))
                    .corner_radius(CornerRadius::same(4))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!(
                                "{}: {err}",
                                tr(l, "Hot-patch channel warning", "热更新通道警告")
                            ))
                            .small()
                            .color(pal.amber),
                        );
                    });
            }

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            // Channel 1: Hot Update (zero restart)
            let pending_hot = self
                .update_status
                .as_ref()
                .filter(|u| u.hot_patch_available)
                .and_then(|u| u.verified_hot_patch.clone());
            let hot_needs_cold = self
                .update_status
                .as_ref()
                .is_some_and(|u| u.hot_patch_requires_cold);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(tr(l, "Hot Update Channel:", "热更新通道:"))
                        .strong()
                        .small()
                        .color(pal.cyan),
                );
                if let Some(patch) = pending_hot {
                    let sum = if l == Lang::Zh {
                        patch
                            .summary_zh
                            .as_deref()
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or(&patch.summary)
                    } else {
                        &patch.summary
                    };
                    ui.label(
                        RichText::new(format!(
                            "rev #{} ({}) {}",
                            patch.revision,
                            patch.patch_id,
                            if sum.is_empty() { "" } else { sum }
                        ))
                        .small()
                        .color(pal.green_text),
                    );
                    let mut apply_now = false;
                    if ui
                        .button(tr(l, "Apply Hot Update Now", "立即免重启热更新"))
                        .clicked()
                    {
                        apply_now = true;
                    }
                    // Explicit review surface: a hot patch can move every REST/WS/RPC
                    // endpoint, so show exactly what it would change before applying.
                    if let Some(eps) = patch.endpoints.as_ref()
                        && let Some(diff) = hot_patch_endpoint_diff(eps)
                    {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(format!(
                                "{} {diff}",
                                tr(l, "Endpoint changes:", "接口变更：")
                            ))
                            .monospace()
                            .small()
                            .color(pal.amber),
                        );
                    }
                    if apply_now {
                        self.apply_verified_hot_patch(patch);
                    }
                } else if let Some(ref hp) = self.active_hot_patch {
                    ui.label(
                        RichText::new(format!(
                            "{} rev #{} ({})",
                            tr(l, "Active:", "当前生效:"),
                            hp.revision,
                            hp.patch_id
                        ))
                        .small()
                        .color(pal.green_text),
                    );
                    if ui
                        .small_button(tr(l, "Reset Hot Patch", "重置热补丁"))
                        .clicked()
                    {
                        self.reset_hot_patch();
                    }
                } else if hot_needs_cold {
                    ui.label(
                        RichText::new(tr(
                            l,
                            "Latest hot patch requires binary upgrade (Cold Update) first.",
                            "最新热更新需要新版原生程序（请先完成下方冷更新）。",
                        ))
                        .small()
                        .color(pal.amber),
                    );
                } else {
                    ui.label(
                        RichText::new(tr(l, "Up to date (no pending hot patch).", "当前暂无新热更新。"))
                            .small()
                            .color(pal.text_muted),
                    );
                }
            });

            ui.add_space(6.0);

            // Channel 2: Cold Binary Channel (restart required)
            let status_snapshot = self.update_status.clone();
            let staged_snapshot = self.staged_cold_update.clone();
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(tr(l, "Cold Binary Channel:", "冷更新通道（原生程序）:"))
                        .strong()
                        .small()
                        .color(pal.blue),
                );
                if let Some(ref staged) = staged_snapshot {
                    ui.label(
                        RichText::new(format!(
                            "v{} {}",
                            staged.version,
                            tr(l, "verified & staged on disk", "已校验并预装就绪")
                        ))
                        .strong()
                        .small()
                        .color(pal.green_text),
                    );
                    if ui
                        .button(tr(l, "Restart Now to Apply", "立即重启切换新版"))
                        .clicked()
                    {
                        self.try_relaunch_staged_update();
                    }
                } else if self.cold_update_in_flight {
                    ui.label(
                        RichText::new(tr(
                            l,
                            "Downloading & verifying SHA-256...",
                            "正在后台下载并校验 SHA-256...",
                        ))
                        .small()
                        .color(pal.amber),
                    );
                } else if let Some(ref u) = status_snapshot {
                    if let Some(ref err) = u.error {
                        ui.label(RichText::new(err).small().color(pal.red_text));
                    } else if u.cold_update_available {
                        let ver = u.latest_version.as_deref().unwrap_or("");
                        ui.label(
                            RichText::new(format!(
                                "v{ver} {}",
                                tr(l, "available (restart required)", "有新版本（需重启）")
                            ))
                            .strong()
                            .small()
                            .color(pal.green_text),
                        );
                        if ui
                            .button(tr(l, "Download & Stage", "后台校验并预装"))
                            .clicked()
                        {
                            self.trigger_stage_cold_update(false);
                        }
                        if ui
                            .button(tr(l, "Update & Restart", "更新并立即重启"))
                            .clicked()
                        {
                            self.trigger_stage_cold_update(true);
                        }
                    } else {
                        ui.label(
                            RichText::new(format!(
                                "{} (v{APP_VERSION})",
                                tr(l, "Binary is up to date", "主程序已是最新版本")
                            ))
                            .small()
                            .color(pal.green_text),
                        );
                    }
                } else if !self.update_check_in_flight {
                    ui.label(
                        RichText::new(tr(l, "Not checked yet.", "尚未检查。"))
                            .small()
                            .color(pal.text_muted),
                    );
                }
            });

            if staged_snapshot.is_some() {
                ui.add_space(2.0);
                ui.label(
                    RichText::new(tr(
                        l,
                        "You can keep trading uninterrupted — the new binary will launch automatically next time you open PolyDirect.",
                        "您也可以继续交易不中断 — 下次启动 PolyDirect 时将自动使用新版本。",
                    ))
                    .small()
                    .color(pal.text_muted),
                );
            } else if let Some(ref u) = status_snapshot
                && u.cold_update_available
                && let Some(ref notes) = u.release_notes
            {
                let preview: String = notes
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .take(4)
                    .collect::<Vec<_>>()
                    .join("\n");
                if !preview.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new(preview).small().color(pal.text_secondary));
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

    fn draw_order_confirm_modal(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();
        let Some(p) = self.pending_order_confirm.clone() else {
            ui.label(RichText::new(tr(l, "No pending order.", "当前没有待确认的订单。")).color(pal.text_muted));
            if ui.button(tr(l, "Close", "关闭")).clicked() {
                self.modal = None;
            }
            return;
        };

        let is_live = self.account.mode == TradingMode::Live;
        let is_buy = p.side.eq_ignore_ascii_case("BUY");
        let side_col = if is_buy { pal.green } else { pal.red };
        let sh_unit = tr(l, "shares", "份");
        let tick = p.market.tick_size;

        // Header banner: Paper vs Live + Side + Outcome
        Frame::NONE
            .fill(ThemePalette::tint(if is_live { pal.red } else { side_col }, if pal.light { 16 } else { 22 }))
            .stroke(Stroke::new(1.0, ThemePalette::tint(if is_live { pal.red } else { side_col }, 135)))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let mode_tag = if is_live {
                        tr(l, "LIVE ORDER · REAL USDC", "实盘订单 · 真实 USDC")
                    } else {
                        tr(l, "PAPER ORDER · SIMULATED", "模拟盘订单 · 本地撮合")
                    };
                    ui.label(RichText::new(mode_tag).monospace().strong().small().color(if is_live { pal.red_text } else { pal.amber }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(p.choice.label(l)).small().color(pal.text_secondary));
                    });
                });
                ui.add_space(4.0);
                ui.label(RichText::new(&p.market.question).strong().color(pal.text_primary));
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "{} {}  ·  {:.2} {}",
                        if is_buy { tr(l, "BUY", "买入") } else { tr(l, "SELL", "卖出") },
                        p.outcome_name,
                        p.est_shares,
                        sh_unit
                    ))
                    .font(ThemePalette::semibold(14.5))
                    .color(side_col),
                );
            });

        ui.add_space(8.0);

        let high_slip = !p.is_limit
            && (!p.filled_completely || p.levels_consumed > 1 || p.slippage_bps >= self.slippage_warn_bps);

        card_frame(pal).show(ui, |ui| {
            egui::Grid::new("ord_confirm_grid").num_columns(2).spacing([20.0, 6.0]).show(ui, |ui| {
                let quote_lbl = if is_buy {
                    tr(l, "Best Ask Quote", "当前卖一报价")
                } else {
                    tr(l, "Best Bid Quote", "当前买一报价")
                };
                ui.label(RichText::new(quote_lbl).small().color(pal.text_muted));
                let best_txt = match (p.best_price, p.best_level_shares) {
                    (Some(bp), Some(sz)) => format!("{}  ({:.1} {})", format_odds(Some(bp), self.odds_fmt, tick), sz, sh_unit),
                    (Some(bp), None) => format_odds(Some(bp), self.odds_fmt, tick),
                    _ => format_odds(Some(p.raw_price), self.odds_fmt, tick),
                };
                let depth_short = p.best_level_shares.is_some_and(|sz| !p.is_limit && p.est_shares > sz + 0.01);
                ui.label(
                    RichText::new(best_txt)
                        .monospace()
                        .small()
                        .color(if depth_short { pal.amber } else { pal.text_primary }),
                );
                ui.end_row();

                ui.label(RichText::new(tr(l, "Est. Avg Fill (VWAP)", "预计成交均价 (VWAP)")).small().color(pal.text_muted));
                let avg_px = p.vwap_price.unwrap_or(p.raw_price);
                let slip_tag = if p.slippage_bps >= 0.5 {
                    format!("  (+{:.0}bp)", p.slippage_bps)
                } else {
                    String::new()
                };
                ui.label(
                    RichText::new(format!("{}{}", format_odds(Some(avg_px), self.odds_fmt, tick), slip_tag))
                        .monospace()
                        .strong()
                        .color(if p.slippage_bps >= self.slippage_warn_bps { pal.amber } else { pal.text_primary }),
                );
                ui.end_row();

                if !p.is_limit
                    && let Some(wp) = p.worst_price
                {
                    ui.label(RichText::new(tr(l, "Book Levels / Worst Price", "吃单档数 / 最差成交价")).small().color(pal.text_muted));
                    ui.label(
                        RichText::new(format!(
                            "{} {} · {}",
                            p.levels_consumed.max(1),
                            tr(l, "lvl(s)", "档"),
                            format_odds(Some(wp), self.odds_fmt, tick)
                        ))
                        .monospace()
                        .small()
                        .color(if p.levels_consumed > 1 { pal.amber } else { pal.text_secondary }),
                    );
                    ui.end_row();
                }

                ui.label(RichText::new(tr(l, "Est. Notional", "预计成交总额")).small().color(pal.text_muted));
                let cap_note = p
                    .max_notional_usd
                    .filter(|&cap| (cap - p.est_usd).abs() > 0.02)
                    .map(|cap| format!("  (cap {})", format_usd(cap, false)))
                    .unwrap_or_default();
                ui.label(RichText::new(format!("{}{}", format_usd(p.est_usd, false), cap_note)).monospace().strong());
                ui.end_row();

                ui.label(RichText::new(tr(l, "Est. Fees", "预计手续费")).small().color(pal.text_muted));
                ui.label(RichText::new(format_usd(p.fee_usd, false)).monospace().small().color(pal.text_muted));
                ui.end_row();
            });
        });

        if high_slip {
            ui.add_space(8.0);
            let col = if !p.filled_completely { pal.red } else { pal.amber };
            let txt_col = if !p.filled_completely { pal.red_text } else { pal.amber };
            let msg = if !p.filled_completely {
                if l == Lang::Zh {
                    "警告：当前盘口深度不足以按原预期全部吃单！剩余部分可能无法成交，或均价明显变差。"
                } else {
                    "Warning: Resting depth on the book is thinner than your order size. Only part of the order can fill at worse levels."
                }
            } else if l == Lang::Zh {
                "滑点警告：买一/卖一挂单量不足，本次订单会扫过多个价格档位，请核对上方平均成交价 (VWAP)。"
            } else {
                "Slippage Warning: Top-of-book depth is insufficient for your size, so this order sweeps deeper levels at a worse average price."
            };
            Frame::NONE
                .fill(ThemePalette::tint(col, if pal.light { 18 } else { 24 }))
                .stroke(Stroke::new(1.0, ThemePalette::tint(col, 140)))
                .corner_radius(CornerRadius::same(6))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.label(RichText::new(msg).small().strong().color(txt_col));
                });
        }

        ui.add_space(8.0);
        let mut ask_confirm = self.confirm_before_order;
        if ui
            .checkbox(
                &mut ask_confirm,
                tr(l, "Always show confirmation before Buy/Sell", "买入 / 卖出前始终显示确认弹窗"),
            )
            .changed()
        {
            self.confirm_before_order = ask_confirm;
            if !ask_confirm {
                self.confirm_on_high_slippage = false;
                self.confirm_ladder_orders = false;
            }
            let high_slip_flag = self.confirm_on_high_slippage;
            let ladder_flag = self.confirm_ladder_orders;
            let _ = self.core.execution.update_config_and_save(|c| {
                c.confirm_before_order = ask_confirm;
                c.confirm_on_high_slippage = high_slip_flag;
                c.confirm_ladder_orders = ladder_flag;
            });
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let confirm_lbl = if is_buy {
                format!("{} {}", tr(l, "Confirm Buy", "确认买入"), format_usd(p.est_usd, false))
            } else {
                format!("{} {:.1} {}", tr(l, "Confirm Sell", "确认卖出"), p.est_shares, sh_unit)
            };
            let btn_txt = ThemePalette::contrast_on(side_col);
            if ui
                .add_sized(
                    Vec2::new(190.0, 36.0),
                    egui::Button::new(RichText::new(confirm_lbl).strong().color(btn_txt))
                        .fill(side_col)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(6)),
                )
                .clicked()
            {
                self.confirm_and_execute_pending_order();
            }
            ui.add_space(8.0);
            if ui
                .add_sized(
                    Vec2::new(96.0, 36.0),
                    egui::Button::new(tr(l, "Cancel", "取消"))
                        .fill(pal.bg_sub)
                        .stroke(Stroke::new(1.0, pal.border))
                        .corner_radius(CornerRadius::same(6)),
                )
                .clicked()
            {
                self.pending_order_confirm = None;
                self.modal = None;
            }
        });
    }

    fn draw_settings_tab_bar(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();
        pal.pill_container().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.selectable_value(
                    &mut self.settings_tab,
                    SettingsTab::Trading,
                    tr(l, "Trading", "交易参数"),
                );
                ui.selectable_value(
                    &mut self.settings_tab,
                    SettingsTab::ChartsSpeed,
                    tr(l, "Charts & Speed", "图表与延迟"),
                );
                ui.selectable_value(
                    &mut self.settings_tab,
                    SettingsTab::Appearance,
                    tr(l, "Appearance", "界面外观"),
                );
                ui.selectable_value(
                    &mut self.settings_tab,
                    SettingsTab::NetworkUpdates,
                    tr(l, "Network & Updates", "节点更新快捷键"),
                );
            });
        });
    }

    fn draw_settings_modal_body(&mut self, ui: &mut egui::Ui) {
        match self.settings_tab {
            SettingsTab::Trading => self.draw_settings_trading_tab(ui),
            SettingsTab::ChartsSpeed => self.draw_settings_charts_speed_tab(ui),
            SettingsTab::Appearance => self.draw_settings_appearance_tab(ui),
            SettingsTab::NetworkUpdates => {
                self.draw_updater_section(ui);
                ui.add_space(10.0);
                self.draw_rpc_section(ui);
                ui.add_space(10.0);
                self.draw_hotkeys_section(ui);
            }
        }
    }

    fn draw_settings_trading_tab(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();

        card_frame(pal).show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Order Execution & Safety Checks", "下单模式与风险确认")).strong().size(13.5));
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Default Order Mode:", "默认挂单/成交模式:")).small().color(pal.text_muted));
                for choice in [
                    OrderChoice::Sweep,
                    OrderChoice::Gtc,
                    OrderChoice::PostOnly,
                    OrderChoice::Fak,
                    OrderChoice::Fok,
                ] {
                    if ui.selectable_label(self.order_choice == choice, choice.label(l)).clicked() {
                        self.order_choice = choice;
                        let s = choice.to_config_str().to_string();
                        let _ = self.core.execution.update_config_and_save(|c| c.default_order_type = s);
                    }
                }
            });

            ui.add_space(8.0);
            if ui
                .checkbox(
                    &mut self.confirm_before_order,
                    tr(l, "Show order confirmation & average fill price before Buy / Sell", "买入 / 卖出前弹窗确认并展示预估均价 (VWAP)"),
                )
                .changed()
            {
                let v = self.confirm_before_order;
                let _ = self.core.execution.update_config_and_save(|c| c.confirm_before_order = v);
            }
            if ui
                .checkbox(
                    &mut self.confirm_on_high_slippage,
                    tr(l, "Also pop up confirmation on high slippage / thin book", "滑点过大或盘口较薄时额外弹出确认窗"),
                )
                .changed()
            {
                let v = self.confirm_on_high_slippage;
                let _ = self.core.execution.update_config_and_save(|c| c.confirm_on_high_slippage = v);
            }
            if ui
                .checkbox(
                    &mut self.confirm_ladder_orders,
                    tr(l, "Also confirm when clicking Price Ladder levels", "点击价格阶梯 (Ladder) 下单时也弹出确认窗"),
                )
                .changed()
            {
                let v = self.confirm_ladder_orders;
                let _ = self.core.execution.update_config_and_save(|c| c.confirm_ladder_orders = v);
            }

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Slippage Warning Threshold:", "滑点警告阈值:")).small().color(pal.text_muted));
                let mut bps = self.slippage_warn_bps;
                if ui
                    .add(egui::DragValue::new(&mut bps).range(5.0..=1000.0).speed(5.0).suffix(" bp"))
                    .changed()
                {
                    self.slippage_warn_bps = bps;
                    let _ = self.core.execution.update_config_and_save(|c| c.slippage_warn_bps = bps);
                }
                for preset in [25.0, 50.0, 100.0, 250.0] {
                    if ui
                        .selectable_label((self.slippage_warn_bps - preset).abs() < 0.5, format!("{preset:.0}bp"))
                        .clicked()
                    {
                        self.slippage_warn_bps = preset;
                        let _ = self.core.execution.update_config_and_save(|c| c.slippage_warn_bps = preset);
                    }
                }
            });
            ui.label(
                RichText::new(tr(l, "100 bp = 1.00%. Thin books or multi-level sweeps always highlight estimated average price.", "100 bp = 1.00%。盘口深度不足或扫过多个档位时会自动标黄/标红提示实际均价。"))
                    .small()
                    .color(pal.text_muted),
            );
        });

        ui.add_space(10.0);

        card_frame(pal).show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Custom Quick-Buy & Ladder Presets", "快捷买入金额与盘口份数预设")).strong().size(13.5));
            ui.add_space(6.0);

            if self.quick_buy_presets.len() < 4 {
                self.quick_buy_presets = default_quick_buy_presets();
            }
            let mut quick_changed = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Quick Buy ($):", "快捷买入 ($):")).small().color(pal.text_muted));
                for v in self.quick_buy_presets.iter_mut().take(4) {
                    if ui
                        .add(egui::DragValue::new(v).range(1.0..=50_000.0).speed(1.0).prefix("+$"))
                        .changed()
                    {
                        quick_changed = true;
                    }
                }
                if ui.small_button(tr(l, "Reset", "默认")).clicked() {
                    self.quick_buy_presets = default_quick_buy_presets();
                    quick_changed = true;
                }
            });
            if quick_changed {
                let copy = self.quick_buy_presets.clone();
                let _ = self.core.execution.update_config_and_save(|c| c.quick_buy_presets = copy);
            }

            ui.add_space(6.0);
            if self.stake_presets.len() < 5 {
                self.stake_presets = default_stake_presets();
            }
            let mut stake_changed = false;
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Ladder Presets:", "阶梯快捷键 (1–5):")).small().color(pal.text_muted));
                for v in self.stake_presets.iter_mut().take(5) {
                    if ui
                        .add(egui::DragValue::new(v).range(1.0..=50_000.0).speed(1.0))
                        .changed()
                    {
                        stake_changed = true;
                    }
                }
                if ui.small_button(tr(l, "Reset", "默认")).clicked() {
                    self.stake_presets = default_stake_presets();
                    stake_changed = true;
                }
            });
            if stake_changed {
                let copy = self.stake_presets.clone();
                let _ = self.core.execution.update_config_and_save(|c| c.stake_presets = copy);
            }

            ui.add_space(6.0);
            let mut whale = self.core.config.read().whale_threshold_usd;
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Whale Alert Threshold:", "巨鲸监控阈值:")).small().color(pal.text_muted));
                if ui
                    .add(egui::DragValue::new(&mut whale).range(500.0..=500_000.0).speed(100.0).prefix("$"))
                    .changed()
                {
                    self.core.feed.set_whale_threshold(whale);
                    let _ = self.core.execution.update_config_and_save(|c| c.whale_threshold_usd = whale);
                }
            });
        });
    }

    fn draw_settings_charts_speed_tab(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();

        card_frame(pal).show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Feed Latency & UI Frame Pacing", "行情刷新速度与延迟优化")).strong().size(13.5));
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "UI Repaint Coalescer:", "界面重绘间隔:")).small().color(pal.text_muted));
                for (ms, lbl) in [
                    (4u64, tr(l, "Max 240Hz (4ms)", "极速 240Hz (4ms)")),
                    (8u64, tr(l, "Ultra 120Hz (8ms)", "高频 120Hz (8ms)")),
                    (16u64, tr(l, "Balanced 60Hz (16ms)", "均衡 60Hz (16ms)")),
                    (33u64, tr(l, "Eco 30Hz (33ms)", "省电 30Hz (33ms)")),
                ] {
                    if ui.selectable_label(self.ui_repaint_ms == ms, lbl).clicked() {
                        self.ui_repaint_ms = ms;
                        self.repaint_gap_atomic.store(ms, Ordering::Relaxed);
                        let _ = self.core.execution.update_config_and_save(|c| c.ui_repaint_ms = ms);
                    }
                }
                if ui
                    .add(egui::DragValue::new(&mut self.ui_repaint_ms).range(4..=50).speed(1).suffix("ms"))
                    .changed()
                {
                    let v = self.ui_repaint_ms.clamp(4, 50);
                    self.ui_repaint_ms = v;
                    self.repaint_gap_atomic.store(v, Ordering::Relaxed);
                    let _ = self.core.execution.update_config_and_save(|c| c.ui_repaint_ms = v);
                }
            });

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Live Share Overlay Ticks:", "份额实时曲线采样:")).small().color(pal.text_muted));
                for ms in [50u64, 100, 250, 500] {
                    if ui.selectable_label(self.live_tick_ms == ms, format!("{ms}ms")).clicked() {
                        self.live_tick_ms = ms;
                        let _ = self.core.execution.update_config_and_save(|c| c.live_tick_ms = ms);
                    }
                }
            });

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "5m/15m Share Candle Bucket:", "短周期份额步长:")).small().color(pal.text_muted));
                for sec in [1i64, 2, 5] {
                    if ui.selectable_label(self.short_chart_bucket_secs == sec, format!("{sec}s")).clicked() {
                        self.short_chart_bucket_secs = sec;
                        let _ = self.core.execution.update_config_and_save(|c| c.short_chart_bucket_secs = sec);
                    }
                }
            });
        });

        ui.add_space(10.0);

        card_frame(pal).show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Crypto Chart Layout & Fast-Lead Curve", "加密货币图表模式与零延迟先导曲线")).strong().size(13.5));
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Primary BTC Price Source:", "主币价实时源:")).small().color(pal.text_muted));
                if ui
                    .selectable_label(
                        self.primary_btc_live_lead,
                        tr(l, "Live Calibrated (~100ms)", "零延迟校准 (~100ms 推荐)"),
                    )
                    .clicked()
                {
                    self.primary_btc_live_lead = true;
                    let _ = self
                        .core
                        .execution
                        .update_config_and_save(|c| c.primary_btc_live_lead = true);
                }
                if ui
                    .selectable_label(
                        !self.primary_btc_live_lead,
                        tr(l, "Chainlink Settlement (~1–2s)", "Chainlink 结算源 (~1–2s)"),
                    )
                    .clicked()
                {
                    self.primary_btc_live_lead = false;
                    let _ = self
                        .core
                        .execution
                        .update_config_and_save(|c| c.primary_btc_live_lead = false);
                }
            });

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Default Synced Window:", "默认对齐时间窗:")).small().color(pal.text_muted));
                for (secs, lbl) in [(60_i64, "1M (60s)"), (180_i64, "3M (180s)"), (300_i64, "5M (300s)")] {
                    if ui.selectable_label(self.short_chart_window_secs == secs, lbl).clicked() {
                        self.short_chart_window_secs = secs;
                        let _ = self
                            .core
                            .execution
                            .update_config_and_save(|c| c.short_chart_window_secs = secs);
                    }
                }
            });

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "Default Short-TF Layout:", "默认视图布局:")).small().color(pal.text_muted));
                for (view, code, lbl) in [
                    (ShortChartView::Both, "both", tr(l, "Both Charts", "双图同显")),
                    (ShortChartView::Overlay, "overlay", tr(l, "Overlay (1 Graph)", "同框叠加")),
                    (ShortChartView::Spot, "spot", tr(l, "Coin Price Only", "仅币价图")),
                    (ShortChartView::Shares, "shares", tr(l, "Shares Only", "仅份额图")),
                ] {
                    if ui.selectable_label(self.short_chart_view == view, lbl).clicked() {
                        self.short_chart_view = view;
                        let s = code.to_string();
                        let _ = self.core.execution.update_config_and_save(|c| c.default_short_chart_view = s);
                    }
                }
            });

            ui.add_space(8.0);
            if ui
                .checkbox(
                    &mut self.show_oracle_fast_lead,
                    tr(l, "Show Binance Futures calibrated Live curve", "显示币安合约校准实时币价曲线"),
                )
                .changed()
            {
                let v = self.show_oracle_fast_lead;
                let _ = self.core.execution.update_config_and_save(|c| c.show_oracle_fast_lead = v);
            }
            if ui
                .checkbox(
                    &mut self.show_oracle_cl_ref,
                    tr(l, "Show Chainlink settlement reference line (CL)", "显示 Chainlink 结算参考蓝线 (CL)"),
                )
                .changed()
            {
                let v = self.show_oracle_cl_ref;
                let _ = self.core.execution.update_config_and_save(|c| c.show_oracle_cl_ref = v);
            }
            if ui
                .checkbox(
                    &mut self.show_oracle_twap,
                    tr(l, "Show Chainlink 60s TWAP line (TWAP60)", "显示 Chainlink 60秒均价线 (TWAP60)"),
                )
                .changed()
            {
                let v = self.show_oracle_twap;
                let _ = self.core.execution.update_config_and_save(|c| c.show_oracle_twap = v);
            }
            if ui
                .checkbox(
                    &mut self.show_oracle_lag_badge,
                    tr(l, "Show Chainlink LAG <ms> readout in header", "在图表顶部显示 Chainlink 延迟读数 (LAG ms)"),
                )
                .changed()
            {
                let v = self.show_oracle_lag_badge;
                let _ = self.core.execution.update_config_and_save(|c| c.show_oracle_lag_badge = v);
            }

            ui.add_space(6.0);
            ui.label(
                RichText::new(tr(
                    l,
                    "How latency optimization works: Polymarket CLOB orderbook deltas use O(1) top-of-book push notifications + TCP_NODELAY; Binance Fast Lead adjusts Binance bookTicker (~50–150ms) by its live EMA spread vs Chainlink settlement (~1s).",
                    "优化原理：Polymarket 订单簿 WebSocket 启用了 TCP_NODELAY 与 O(1) 买一/卖一直推；FAST 蓝线基于币安逐笔盘口（约 50–150ms）动态消除与 Chainlink 结算源（约 1s 更新）之间的基差。",
                ))
                .small()
                .color(pal.text_muted),
            );
        });
    }

    fn draw_settings_appearance_tab(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let pal = self.palette();

        card_frame(pal).show(ui, |ui| {
            ui.label(RichText::new(tr(l, "Theme, Language & Readability", "主题、语言与缩放设置")).strong().size(13.5));
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Color Theme:", "界面主题:")).small().color(pal.text_muted));
                if ui.selectable_label(!self.light_mode, tr(l, "Obsidian Dark", "深空黑夜")).clicked() {
                    self.light_mode = false;
                    apply_theme(ui.ctx(), false);
                    let _ = self.core.execution.update_config_and_save(|c| c.theme = "dark".into());
                }
                if ui.selectable_label(self.light_mode, tr(l, "Swiss Light", "纯净浅色")).clicked() {
                    self.light_mode = true;
                    apply_theme(ui.ctx(), true);
                    let _ = self.core.execution.update_config_and_save(|c| c.theme = "light".into());
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Interface Language:", "显示语言:")).small().color(pal.text_muted));
                if ui.selectable_label(self.lang == Lang::En, "English").clicked() {
                    self.lang = Lang::En;
                    let _ = self.core.execution.update_config_and_save(|c| c.language = "en".into());
                    self.trigger_translate_visible();
                }
                if ui.selectable_label(self.lang == Lang::Zh, "简体中文").clicked() {
                    self.lang = Lang::Zh;
                    let _ = self.core.execution.update_config_and_save(|c| c.language = "zh".into());
                    self.trigger_translate_visible();
                }
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Odds Display:", "赔率显示:")).small().color(pal.text_muted));
                for (fmt, lbl) in [
                    (OddsFormat::Cents, tr(l, "Cents (56¢)", "美分 (56¢)")),
                    (OddsFormat::Prob, tr(l, "Percent (56%)", "百分比 (56%)")),
                    (OddsFormat::Decimal, tr(l, "Decimal (0.56)", "小数 (0.56)")),
                    (OddsFormat::Moneyline, tr(l, "Moneyline", "美式盘口")),
                ] {
                    if ui.selectable_label(self.odds_fmt == fmt, lbl).clicked() {
                        self.odds_fmt = fmt;
                        let s = fmt.as_str_code().to_string();
                        let _ = self.core.execution.update_config_and_save(|c| c.odds_format = s);
                    }
                }
            });

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(tr(l, "UI Scale:", "界面缩放:")).small().color(pal.text_muted));
                for z in [0.90f32, 1.00, 1.10, 1.20] {
                    if ui
                        .selectable_label((self.ui_zoom - z).abs() < 0.01, format!("{:.0}%", z * 100.0))
                        .clicked()
                    {
                        self.ui_zoom = z;
                        ui.ctx().set_zoom_factor(z);
                        let _ = self.core.execution.update_config_and_save(|c| c.ui_zoom = z);
                    }
                }
                let mut z = self.ui_zoom;
                if ui
                    .add(egui::DragValue::new(&mut z).range(0.85..=1.25).speed(0.01).suffix("x"))
                    .changed()
                {
                    let clamped = z.clamp(0.85, 1.25);
                    self.ui_zoom = clamped;
                    ui.ctx().set_zoom_factor(clamped);
                    let _ = self.core.execution.update_config_and_save(|c| c.ui_zoom = clamped);
                }
            });
        });
    }
}
