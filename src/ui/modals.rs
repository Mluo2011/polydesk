use egui::{Color32, Context, CornerRadius, Frame, Margin, RichText, ScrollArea, Stroke, TextEdit, Window};

use polydesk::config::{APP_VERSION, BUILD_TARGET};
use polydesk::execution::TradingMode;
use polydesk::i18n::tr;
use polydesk::odds::format_usd;
use polydesk::vault::WalletSignatureType;

use super::{ActiveModal, PolyDeskDesktop, WalletSetupTab};

fn card_frame() -> Frame {
    Frame::NONE
        .fill(Color32::from_rgb(20, 27, 40))
        .stroke(Stroke::new(1.0, Color32::from_rgb(31, 41, 55)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
}

impl PolyDeskDesktop {
    pub fn render_modals(&mut self, ctx: &Context) {
        let Some(modal) = self.modal else {
            return;
        };

        let mut open = true;
        match modal {
            ActiveModal::WalletAndDeposit => {
                let title = tr(self.lang, "Wallet & Add Real Money", "本地加密钱包与充值");
                Window::new(title)
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .default_width(620.0)
                    .show(ctx, |ui| {
                        ScrollArea::vertical().max_height(640.0).show(ui, |ui| {
                            self.draw_wallet_and_deposit_modal(ui);
                        });
                    });
            }
            ActiveModal::SettingsAndUpdates => {
                let title = tr(self.lang, "Updates & Keyboard Hotkeys", "自动更新与快捷键");
                Window::new(title)
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(true)
                    .default_width(520.0)
                    .show(ctx, |ui| {
                        ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                            self.draw_updater_section(ui);
                            ui.add_space(12.0);
                            self.draw_hotkeys_section(ui);
                        });
                    });
            }
        }

        if !open {
            self.modal = None;
            // Clear sensitive in-memory UI display when closing the wallet window
            self.revealed_wallet = None;
            self.reveal_pass_input.clear();
        }
    }

    fn draw_wallet_and_deposit_modal(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        let acc = self.account.clone();

        // Security Guarantee Banner
        Frame::NONE
            .fill(Color32::from_rgba_unmultiplied(16, 185, 129, 18))
            .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(16, 185, 129, 80)))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(tr(
                        l,
                        "100% On-Device Self-Custody: Your secret phrase & private key are generated and encrypted locally (Argon2id + AES-256-GCM) and NEVER leave your device. Only signed EIP-712 orders are sent to Polymarket.",
                        "100% 纯本机非托管：助记词与私钥均在本机随机生成并用 Argon2id + AES-256-GCM 加密保存，永不上传任何服务器；仅在本地完成 EIP-712 签名后发送订单。",
                    ))
                    .small()
                    .color(Color32::from_rgb(167, 243, 208)),
                );
            });

        ui.add_space(10.0);

        // Secret Phrase & Private Key Backup Card (shown immediately after creating a wallet or revealing backup)
        if let Some(w) = self.revealed_wallet.clone() {
            Frame::NONE
                .fill(Color32::from_rgba_unmultiplied(245, 158, 11, 20))
                .stroke(Stroke::new(1.2, Color32::from_rgb(251, 191, 36)))
                .corner_radius(CornerRadius::same(10))
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
                            .color(Color32::from_rgb(251, 191, 36)),
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
                        .color(Color32::from_rgb(253, 230, 138)),
                    );
                    ui.add_space(8.0);

                    egui::Grid::new("backup_keys_grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label(RichText::new(tr(l, "Local Address", "本地钱包地址")).strong().small());
                        ui.label(RichText::new(&w.address).monospace().small().color(Color32::from_rgb(56, 189, 248)));
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(w.address.clone());
                            self.push_toast(tr(l, "Copied local address", "已复制本地钱包地址").into(), false);
                        }
                        ui.end_row();

                        if let Some(ref phrase) = w.mnemonic {
                            ui.label(RichText::new(tr(l, "Secret Phrase (12w)", "12词助记词")).strong().small());
                            ui.add(
                                egui::Label::new(
                                    RichText::new(phrase).monospace().small().color(Color32::from_rgb(254, 243, 199)),
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
                        ui.label(RichText::new(pk_short).monospace().small().color(Color32::from_rgb(252, 165, 165)))
                            .on_hover_text(&w.private_key);
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(w.private_key.clone());
                            self.push_toast(tr(l, "Copied private key", "已复制私钥").into(), false);
                        }
                        ui.end_row();
                    });
                });
            ui.add_space(10.0);
        }

        // 1. Paper Simulator Card
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Paper Simulator", "模拟盘账户")).strong().size(13.5).color(Color32::from_rgb(251, 191, 36)));
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
                .color(Color32::from_rgb(148, 163, 184)),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut self.paper_reset_input).desired_width(96.0));
                if ui.button(tr(l, "Reset Balance", "重置模拟余额")).clicked() {
                    let bal = self.paper_reset_input.parse::<f64>().unwrap_or(1000.0);
                    self.core.execution.reset_paper(bal);
                    self.push_toast(format!("Paper balance reset to {}", format_usd(bal, false)), false);
                }
                if acc.live_connected && ui.button(tr(l, "Switch to PAPER", "切回模拟盘")).clicked() {
                    let _ = self.core.execution.set_mode(TradingMode::Paper);
                }
            });
        });

        ui.add_space(10.0);

        // 2. Local Encrypted Live Vault (Create New Random Wallet OR Import Private Key/Phrase)
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "Live Trading Vault (Local Only)", "实盘本地加密钱包")).strong().size(13.5).color(Color32::from_rgb(16, 185, 129)));
                if acc.live_connected {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let addr = acc.wallet_address.as_deref().unwrap_or("LIVE");
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(addr.to_string());
                            self.push_toast(tr(l, "Copied wallet address", "已复制钱包地址").into(), false);
                        }
                        ui.label(RichText::new(addr).monospace().small().color(Color32::from_rgb(16, 185, 129)));
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
                    if ui.button(RichText::new(tr(l, "Unlock Live", "解锁实盘")).color(Color32::from_rgb(16, 185, 129))).clicked() {
                        self.trigger_unlock_vault();
                    }
                    if ui.button(tr(l, "Delete Vault", "删除重设")).clicked() {
                        self.revealed_wallet = None;
                        self.trigger_wipe_vault();
                    }
                });
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
                            .color(Color32::from_rgb(148, 163, 184)),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label(tr(l, "Local Vault Password", "设置本机加密密码 (≥6位)"));
                            ui.add(
                                TextEdit::singleline(&mut self.vault_pass_input)
                                    .password(true)
                                    .hint_text(tr(l, "Min 6 characters", "至少6位密码"))
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
                                            .color(Color32::from_rgb(16, 185, 129)),
                                    )
                                    .fill(Color32::from_rgba_unmultiplied(16, 185, 129, 26))
                                    .stroke(Stroke::new(1.0, Color32::from_rgb(16, 185, 129)))
                                    .corner_radius(CornerRadius::same(7)),
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

                            ui.label(tr(l, "Local Vault Password", "本地加密密码 (≥6位)"));
                            ui.add(TextEdit::singleline(&mut self.vault_pass_input).password(true).desired_width(340.0));
                            ui.end_row();
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut self.wallet_persist_vault, tr(l, "Save encrypted locally", "加密保存到本机"));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(RichText::new(tr(l, "Unlock LIVE Wallet", "解锁并进入实盘")).strong().color(Color32::from_rgb(16, 185, 129))).clicked() {
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

        ui.add_space(10.0);

        // 3. Direct USDT / USDC Deposit (Official Polymarket Bridge)
        card_frame().show(ui, |ui| {
            ui.label(
                RichText::new(tr(
                    l,
                    "Direct USDT / USDC Deposit (Official Polymarket Bridge)",
                    "直接转入 USDT / USDC 充值 (Polymarket 官方跨链桥)",
                ))
                .strong()
                .size(13.5)
                .color(Color32::from_rgb(56, 189, 248)),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr(
                    l,
                    "Send USDT or USDC directly from Binance, OKX, Coinbase, MetaMask, or Phantom to your personal Bridge deposit address below (Polygon / BSC / Base / Arbitrum / Tron TRC-20 / Solana, min $2) — Polymarket auto-converts 1:1 into Polygon pUSD inside your local wallet. Then click Sync Balance & Allowance.",
                    "直接从币安 / 欧易 OKX / Coinbase / MetaMask / Phantom 向下方专属跨链桥地址转入 USDT 或 USDC（支持 Polygon / BSC / Base / Arbitrum / 波场 TRC-20 / Solana，最低仅 $2），Polymarket 会自动 1:1 转为 Polygon pUSD 到账你的本地钱包，然后点击「刷新余额与授权」即可实盘交易。",
                ))
                .small()
                .color(Color32::from_rgb(148, 163, 184)),
            );
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if self.deposit_wallet_input.is_empty()
                    && let Some(addr) = acc.funder_address.as_ref().or(acc.wallet_address.as_ref())
                {
                    self.deposit_wallet_input = addr.clone();
                }
                ui.add(TextEdit::singleline(&mut self.deposit_wallet_input).hint_text("0xYourPolygonOrProxyAddress...").desired_width(360.0));
                if ui.button(RichText::new(tr(l, "Get USDT/USDC Addresses", "获取 USDT/USDC 充值地址")).color(Color32::from_rgb(56, 189, 248))).clicked() {
                    self.trigger_fetch_deposit();
                }
            });

            if let Some(dep) = self.deposit_info.clone() {
                ui.add_space(8.0);
                egui::Grid::new("dep_addr_grid").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label(RichText::new("EVM · USDT / USDC (Poly/BSC/Base/Arb/ETH)").strong().small());
                    ui.label(RichText::new(&dep.evm_address).monospace().small().color(Color32::from_rgb(16, 185, 129)));
                    if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                        ui.ctx().copy_text(dep.evm_address.clone());
                        self.push_toast("Copied EVM USDT/USDC deposit address".into(), false);
                    }
                    ui.end_row();

                    if let Some(ref tron) = dep.tron_address {
                        ui.label(RichText::new("Tron TRC-20 · USDT (min $2)").strong().small());
                        ui.label(RichText::new(tron).monospace().small().color(Color32::from_rgb(244, 114, 182)));
                        if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                            ui.ctx().copy_text(tron.clone());
                            self.push_toast("Copied Tron TRC-20 USDT address".into(), false);
                        }
                        ui.end_row();
                    }

                    ui.label(RichText::new("Solana · USDT / USDC / SOL").strong().small());
                    ui.label(RichText::new(&dep.solana_address).monospace().small().color(Color32::from_rgb(56, 189, 248)));
                    if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                        ui.ctx().copy_text(dep.solana_address.clone());
                        self.push_toast("Copied Solana USDT/USDC address".into(), false);
                    }
                    ui.end_row();

                    ui.label(RichText::new("Bitcoin · BTC (min $3.6)").strong().small());
                    ui.label(RichText::new(&dep.btc_address).monospace().small().color(Color32::from_rgb(251, 191, 36)));
                    if ui.small_button(tr(l, "Copy", "复制")).clicked() {
                        ui.ctx().copy_text(dep.btc_address.clone());
                        self.push_toast("Copied BTC deposit address".into(), false);
                    }
                    ui.end_row();
                });
            }
        });
    }

    fn draw_updater_section(&mut self, ui: &mut egui::Ui) {
        let l = self.lang;
        card_frame().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr(l, "GitHub Releases Auto-Updater", "GitHub 自动更新")).strong().size(13.5).color(Color32::from_rgb(56, 189, 248)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("v{APP_VERSION} · {BUILD_TARGET}")).monospace().small().color(Color32::GRAY));
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
            if let Some(ref u) = self.update_status {
                ui.add_space(6.0);
                if let Some(ref err) = u.error {
                    ui.label(RichText::new(err).small().color(Color32::from_rgb(244, 63, 94)));
                } else if u.update_available {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("v{} {}", u.latest_version.as_deref().unwrap_or(""), tr(l, "available", "可更新"))).strong().color(Color32::from_rgb(16, 185, 129)));
                        if ui.button(tr(l, "Verify SHA-256 & Install", "校验 SHA-256 并自动更新")).clicked() {
                            self.trigger_apply_update();
                        }
                    });
                } else {
                    ui.label(RichText::new(tr(l, "You are on the latest version.", "当前已是最新版本。")).small().color(Color32::from_rgb(16, 185, 129)));
                }
            }
        });
    }

    fn draw_hotkeys_section(&self, ui: &mut egui::Ui) {
        let l = self.lang;
        card_frame().show(ui, |ui| {
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
                    ui.label(RichText::new(k).monospace().strong().color(Color32::from_rgb(56, 189, 248)));
                    ui.label(desc);
                    ui.end_row();
                }
            });
        });
    }
}
