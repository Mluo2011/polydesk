import SwiftUI
import UIKit

struct WalletDepositSheet: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.dismiss) private var dismiss

    @State private var setupMode = 0 // 0 = Create New Random Wallet, 1 = Import Private Key / Phrase
    @State private var pk = ""
    @State private var funder = ""
    @State private var sigType = "eoa"
    @State private var vaultPass = ""
    @State private var revealPass = ""
    @State private var depositWallet = ""
    @State private var paperReset = "1000"

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(engine.t("100% On-Device Self-Custody", "100% 纯本地私钥保管"))
                            .font(.subheadline.bold())
                            .foregroundColor(.green)
                        Text(engine.t(
                            "Your secret phrase & private key are generated and encrypted locally (Argon2id + AES-256-GCM) inside your device and NEVER sent online. Orders are signed locally via EIP-712.",
                            "您的助记词与私钥均在本机随机生成并使用 Argon2id + AES-256-GCM 加密保存，绝不上传至任何服务器；订单仅在本地完成 EIP-712 签名。"
                        ))
                        .font(.caption)
                        .foregroundColor(.secondary)
                    }
                    .padding(.vertical, 2)
                }

                if let w = engine.revealedWallet {
                    Section(engine.t("SAVE YOUR BACKUP NOW (Local Only)", "请立即抄写保存备份（仅在本机显示）")) {
                        Text(engine.t(
                            "Write down this 12-word Secret Phrase & Private Key offline. Never share them. You can import this 12-word phrase into MetaMask or Rabby anytime to withdraw.",
                            "请离线抄写保存下方 12 词助记词与私钥，绝不要分享给任何人。随时可导入 MetaMask / Rabby 钱包提现。"
                        ))
                        .font(.caption)
                        .foregroundColor(.orange)

                        copyRow(engine.t("Local Polygon Address", "本地钱包地址"), w.address)
                        if let phrase = w.mnemonic {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(engine.t("12-Word Secret Phrase", "12 词助记词")).font(.caption2).foregroundColor(.secondary)
                                Text(phrase).font(.caption.monospaced().bold()).foregroundColor(.orange)
                                Button(engine.t("Copy 12-Word Phrase", "复制 12 词助记词")) {
                                    UIPasteboard.general.string = phrase
                                    engine.showToast(engine.t("Copied secret phrase", "已复制助记词"))
                                }
                            }
                        }
                        copyRow(engine.t("Private Key (0x...)", "私钥 (0x...)"), w.private_key)
                        Button(engine.t("I Saved Them — Hide Keys", "已保存，隐藏私钥")) {
                            engine.revealedWallet = nil
                        }
                        .foregroundColor(.cyan)
                    }
                }

                Section(engine.t("Paper Simulator", "模拟盘账户")) {
                    HStack {
                        Text(engine.t("Balance", "余额"))
                        Spacer()
                        Text(String(format: "$%.2f", engine.account?.paper.cash_usd ?? 1000))
                            .font(.system(.body, design: .monospaced))
                    }
                    HStack {
                        TextField("1000", text: $paperReset).keyboardType(.decimalPad)
                        Button(engine.t("Reset Paper", "重置模拟余额")) {
                            engine.resetPaper(Double(paperReset) ?? 1000)
                        }
                    }
                }

                Section(engine.t("Self-Custody Local Encrypted Vault (LIVE)", "本地加密实盘钱包 (纯本地签名)")) {
                    if engine.account?.live_connected == true {
                        copyRow(engine.t("LIVE Wallet Address", "当前实盘地址"), engine.account?.wallet_address ?? "")
                        HStack {
                            Text(engine.t("pUSD Balance", "实盘余额"))
                            Spacer()
                            Text(String(format: "$%.2f", engine.account?.live_balance_usd ?? 0)).font(.body.monospaced())
                        }
                        Button(engine.t("Sync Balance & Allowance", "刷新余额与链上授权")) { engine.syncAllowance() }
                        if engine.account?.vault.exists == true {
                            SecureField(engine.t("Password to reveal phrase/key", "输入密码查看备份助记词/私钥"), text: $revealPass)
                            Button(engine.t("Reveal Phrase / Key", "查看助记词/私钥")) {
                                engine.revealBackup(password: revealPass)
                                revealPass = ""
                            }
                        }
                        Button(engine.t("Lock & Switch to Paper", "锁定并切回模拟盘"), role: .destructive) {
                            engine.revealedWallet = nil
                            engine.lockWallet()
                        }
                    } else if engine.account?.vault.exists == true {
                        SecureField(engine.t("Vault passphrase", "本地加密库密码"), text: $vaultPass)
                        Button(engine.t("Unlock Live Wallet", "解锁实盘钱包")) {
                            let p = vaultPass
                            vaultPass = ""
                            engine.unlockVault(password: p)
                        }
                    } else {
                        Picker("", selection: $setupMode) {
                            Text(engine.t("Create New Wallet", "生成随机新钱包")).tag(0)
                            Text(engine.t("Import Key / Phrase", "导入私钥/助记词")).tag(1)
                        }
                        .pickerStyle(.segmented)

                        if setupMode == 0 {
                            Text(engine.t(
                                "Creates a random local Polygon address + 12-word Secret Phrase + Private Key on this device and auto-generates direct USDT / USDC deposit addresses.",
                                "在本机随机生成全新 Polygon 地址 + 12 词助记词 + 私钥，并自动生成专属 USDT / USDC 直充地址。"
                            ))
                            .font(.caption).foregroundColor(.secondary)
                            SecureField(engine.t("Vault Password (min 12 chars)", "设置加密密码 (≥12位)"), text: $vaultPass)
                            Button(engine.t("Generate Random Local Wallet", "一键生成随机本地钱包")) {
                                engine.createNewWallet(password: vaultPass)
                                vaultPass = ""
                            }
                            .foregroundColor(.green)
                        } else {
                            Picker(engine.t("Wallet Type", "钱包类型"), selection: $sigType) {
                                Text("EOA").tag("eoa")
                                Text("Proxy").tag("proxy")
                                Text("Gnosis Safe").tag("gnosis_safe")
                                Text("Poly1271 Deposit").tag("poly1271")
                            }
                            SecureField(engine.t("Private Key (0x...) or 12-word Phrase", "私钥 (0x...) 或 12词助记词"), text: $pk)
                            TextField("Funder / Proxy (optional)", text: $funder)
                            SecureField(engine.t("Vault Password (min 12 chars)", "加密库密码 (≥12位)"), text: $vaultPass)
                            Button(engine.t("Authenticate & Unlock LIVE", "认证并解锁实盘")) {
                                let k = pk
                                let p = vaultPass
                                pk = ""
                                vaultPass = ""
                                engine.connectWallet(pk: k, funder: funder, sigType: sigType, password: p)
                            }
                        }
                    }
                }

                Section(engine.t("Direct USDT / USDC Deposit (Polymarket Bridge)", "直接转入 USDT / USDC 充值 (官方跨链桥)")) {
                    Text(engine.t(
                        "Transfer USDT or USDC directly from Binance, OKX, Coinbase, MetaMask, or Phantom to your personal Bridge deposit address below (Polygon / BSC / Base / Arbitrum / Tron TRC-20 / Solana, min $2). It converts 1:1 into Polygon pUSD automatically — then tap Sync Balance & Allowance.",
                        "直接向下方官方跨链桥专属地址转入 USDT 或 USDC（支持 Polygon / BSC / Base / Arbitrum / 波场 TRC-20 / Solana，最低 $2），系统自动 1:1 转为 Polygon pUSD 到账你的本地钱包，随后点击「刷新余额与链上授权」即可实盘下单。"
                    ))
                    .font(.caption).foregroundColor(.secondary)

                    TextField("0xPolygonOrProxyAddress...", text: $depositWallet)
                        .font(.caption.monospaced())
                    Button(engine.t("Get USDT / USDC Deposit Addresses", "获取 USDT / USDC 充值地址")) {
                        engine.fetchDepositAddresses(wallet: depositWallet)
                    }
                    if let dep = engine.depositInfo {
                        copyRow("EVM · USDT / USDC (Poly/BSC/Base/Arb/ETH)", dep.evm_address)
                        if let tron = dep.tron_address {
                            copyRow("Tron TRC-20 · USDT (min $2)", tron)
                        }
                        copyRow("Solana · USDT / USDC / SOL", dep.solana_address)
                        copyRow("Bitcoin · BTC", dep.btc_address)
                    }
                }
            }
            .navigationTitle(engine.t("Wallet & Add Money", "钱包与充值"))
            .toolbar { Button(engine.t("Done", "完成")) { dismiss() } }
            .onAppear {
                if depositWallet.isEmpty {
                    depositWallet = engine.account?.funder_address ?? engine.account?.wallet_address ?? ""
                }
            }
            .onDisappear {
                engine.revealedWallet = nil
                revealPass = ""
                pk = ""
                vaultPass = ""
            }
        }
    }

    private func copyRow(_ label: String, _ addr: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label).font(.caption2).foregroundColor(.secondary)
            HStack {
                Text(addr).font(.caption2.monospaced()).lineLimit(1).truncationMode(.middle)
                Spacer()
                Button(engine.t("Copy", "复制")) {
                    UIPasteboard.general.string = addr
                    engine.showToast("Copied \(label)")
                }
            }
        }
    }
}
