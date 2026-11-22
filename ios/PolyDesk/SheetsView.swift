import SwiftUI
import UIKit

struct WalletDepositSheet: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.dismiss) private var dismiss

    @State private var pk = ""
    @State private var funder = ""
    @State private var sigType = "eoa"
    @State private var vaultPass = ""
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
                            "Your private key is encrypted locally with Argon2id + AES-256-GCM inside your device storage and NEVER sent online. Orders are signed locally via EIP-712 inside Rust.",
                            "您的私钥仅使用 Argon2id + AES-256-GCM 加密保存在本机存储中，绝不上传至任何服务器。所有订单均在本地 Rust 引擎内完成 EIP-712 签名。"
                        ))
                        .font(.caption)
                        .foregroundColor(.secondary)
                    }
                    .padding(.vertical, 2)
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
                        Text("LIVE: \(engine.account?.wallet_address ?? "")").font(.caption.monospaced())
                        HStack {
                            Text(engine.t("pUSD Balance", "实盘余额"))
                            Spacer()
                            Text(String(format: "$%.2f", engine.account?.live_balance_usd ?? 0)).font(.body.monospaced())
                        }
                        Button(engine.t("Sync Balance & Allowance", "刷新余额与链上授权")) { engine.syncAllowance() }
                        Button(engine.t("Lock & Switch to Paper", "锁定并切回模拟盘"), role: .destructive) { engine.lockWallet() }
                    } else if engine.account?.vault.exists == true {
                        SecureField(engine.t("Vault passphrase", "本地加密库密码"), text: $vaultPass)
                        Button(engine.t("Unlock Live Wallet", "解锁实盘钱包")) { engine.unlockVault(password: vaultPass) }
                    } else {
                        Picker(engine.t("Wallet Type", "钱包类型"), selection: $sigType) {
                            Text("EOA").tag("eoa")
                            Text("Proxy").tag("proxy")
                            Text("Gnosis Safe").tag("gnosis_safe")
                            Text("Poly1271 Deposit").tag("poly1271")
                        }
                        SecureField("Polygon Private Key (0x...)", text: $pk)
                        TextField("Funder / Proxy (optional)", text: $funder)
                        SecureField(engine.t("Vault Password (min 6 chars)", "加密库密码 (≥6位)"), text: $vaultPass)
                        Button(engine.t("Authenticate & Unlock LIVE", "认证并解锁实盘")) {
                            engine.connectWallet(pk: pk, funder: funder, sigType: sigType, password: vaultPass)
                        }
                    }
                }

                Section(engine.t("How to Add Real Money (Polygon pUSD / Bridge)", "如何充值真实资金 (Polygon pUSD / 官方跨链桥)")) {
                    Text(engine.t(
                        "Send USDC on Polygon, Arbitrum, Base, Ethereum, or Solana (or BTC) to your personal Polymarket Bridge address below — it converts 1:1 into Polygon pUSD collateral automatically. Then tap Sync Balance & Allowance.",
                        "向下方 Polymarket 官方跨链桥专属地址转入 Polygon/Arbitrum/Base/ETH/Solana USDC 或 BTC，系统会自动 1:1 转为 Polygon pUSD 保证金，到账后点击「刷新余额与链上授权」即可实盘交易。"
                    ))
                    .font(.caption).foregroundColor(.secondary)

                    TextField("0xPolygonOrProxyAddress...", text: $depositWallet)
                        .font(.caption.monospaced())
                    Button(engine.t("Fetch Official Deposit Addresses", "获取官方充值地址")) {
                        engine.fetchDepositAddresses(wallet: depositWallet)
                    }
                    if let dep = engine.depositInfo {
                        copyRow("EVM (Polygon/Arb/Base/ETH)", dep.evm_address)
                        copyRow("Solana (USDC / SOL)", dep.solana_address)
                        copyRow("Bitcoin (BTC)", dep.btc_address)
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
