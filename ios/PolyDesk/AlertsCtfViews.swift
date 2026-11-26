import SwiftUI

// MARK: - v0.3.1: Price alerts

struct AlertsPaneView: View {
    @ObservedObject var engine: EngineBridge
    @State private var kind: String = "cross_above"
    @State private var thresholdText: String = "55"

    private var thresholdCents: Double {
        Double(thresholdText.replacingOccurrences(of: ",", with: ".")) ?? 0
    }

    private func conditionLabel(_ c: AlertCond) -> String {
        let cents = String(format: "%.1f¢", c.threshold * 100)
        switch c.kind {
        case "cross_above": return "\(engine.t("Above", "上穿")) \(cents)"
        case "cross_below": return "\(engine.t("Below", "下穿")) \(cents)"
        case "spread_above": return "\(engine.t("Spread", "价差")) > \(cents)"
        default: return c.kind
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Picker("", selection: $kind) {
                Text(engine.t("Cross Above", "上穿")).tag("cross_above")
                Text(engine.t("Cross Below", "下穿")).tag("cross_below")
                Text(engine.t("Spread Above", "价差大于")).tag("spread_above")
            }
            .pickerStyle(.segmented)

            HStack(spacing: 8) {
                Text(engine.t("Threshold (¢)", "阈值 (¢)"))
                    .font(.caption)
                TextField("55", text: $thresholdText)
                    .textFieldStyle(.roundedBorder)
                    .keyboardType(.decimalPad)
                    .frame(width: 90)
                Button(engine.t("Arm Alert", "部署预警")) {
                    engine.addAlert(kind: kind, thresholdCents: thresholdCents)
                }
                .font(.caption.bold())
                .buttonStyle(.borderedProminent)
            }

            if engine.alerts.isEmpty {
                Text(engine.t("No alerts armed yet.", "暂无预警。"))
                    .font(.caption2)
                    .foregroundColor(.secondary)
            } else {
                ForEach(engine.alerts) { a in
                    HStack(spacing: 6) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(a.outcome) · \(a.market_question)")
                                .font(.caption2)
                                .lineLimit(1)
                            Text("\(conditionLabel(a.condition))\(a.active ? "" : " · \(engine.t("fired", "已触发"))")")
                                .font(.caption2.monospacedDigit())
                                .foregroundColor(a.active ? .orange : .secondary)
                        }
                        Spacer()
                        Button("×") { engine.removeAlert(a.id) }
                            .foregroundColor(.red)
                    }
                }
            }
        }
        .onAppear { engine.loadAlerts() }
    }
}

// MARK: - v0.3.1: CTF split / merge / redeem

struct CtfPaneView: View {
    @ObservedObject var engine: EngineBridge
    @State private var amountText: String = "10"
    @State private var pendingAction: String? = nil

    private var amount: Double {
        Double(amountText.replacingOccurrences(of: ",", with: ".")) ?? 0
    }

    private var marketShares: (yes: Double, no: Double) {
        guard let m = engine.activeMarket, m.token_ids.count >= 2 else { return (0, 0) }
        let isLive = engine.account?.mode == "live"
        let list = (isLive ? engine.account?.live_positions : engine.account?.paper.positions) ?? []
        let y = list.first(where: { $0.asset_id == m.token_ids[0] })?.shares ?? 0
        let n = list.first(where: { $0.asset_id == m.token_ids[1] })?.shares ?? 0
        return (y, n)
    }

    private func actionTitle(_ act: String) -> String {
        switch act {
        case "split": return engine.t("Confirm CTF Split", "确认 CTF 拆分")
        case "merge": return engine.t("Confirm CTF Merge", "确认 CTF 合并")
        case "redeem": return engine.t("Confirm CTF Redeem", "确认 CTF 赎回")
        default: return engine.t("Confirm CTF Action", "确认 CTF 操作")
        }
    }

    private func confirmationMessage(_ act: String) -> String {
        let isLive = engine.account?.mode == "live"
        let mode = isLive ? engine.t("LIVE EOA · Polygon Mainnet", "实盘 EOA · Polygon 主网") : engine.t("PAPER Simulation", "模拟盘演练")
        let q = engine.activeMarket?.displayQuestion(zh: engine.zh) ?? "—"
        let sh = marketShares
        let amtDesc = act == "redeem"
            ? String(format: "%.2f %@", sh.yes + sh.no, engine.t("shares", "份"))
            : String(format: "%.2f", amount)
        let wallet = engine.account?.wallet_address ?? "—"
        return "\(mode)\n\(q)\n\(engine.t("Amount", "数量")): \(amtDesc)\n\(engine.t("Wallet", "钱包")): \(wallet)"
    }

    var body: some View {
        let sh = marketShares
        let isLive = engine.account?.mode == "live"
        VStack(alignment: .leading, spacing: 8) {
            Text(engine.t(
                "Paper mode simulates Split/Merge locally. Live on-chain Split/Merge/Redeem requires an unlocked EOA wallet + POL gas.",
                "模拟盘支持本地 Split/Merge 演练；实盘链上拆分/合并/赎回需解锁 EOA 钱包并预留 POL 矿工费。"
            ))
            .font(.caption2)
            .foregroundColor(.secondary)

            HStack(spacing: 8) {
                Text(engine.t("Amount (USDC / shares)", "数量 (USDC / 份)"))
                    .font(.caption)
                TextField("10", text: $amountText)
                    .textFieldStyle(.roundedBorder)
                    .keyboardType(.decimalPad)
                    .frame(width: 90)
                if sh.yes + sh.no > 0 {
                    Text(String(format: "Y %.1f · N %.1f", sh.yes, sh.no))
                        .font(.caption2.monospacedDigit())
                        .foregroundColor(.secondary)
                }
            }

            HStack(spacing: 8) {
                Button(engine.t("Split", "拆分")) {
                    pendingAction = "split"
                }
                .buttonStyle(.borderedProminent)
                .disabled(engine.ctfBusy)

                Button(engine.t("Merge", "合并")) {
                    pendingAction = "merge"
                }
                .buttonStyle(.bordered)
                .disabled(engine.ctfBusy)

                Button(engine.t("Redeem", "赎回")) {
                    pendingAction = "redeem"
                }
                .buttonStyle(.bordered)
                .disabled(engine.ctfBusy || !isLive || (sh.yes + sh.no) <= 0)
            }
            .font(.caption.bold())

            if engine.ctfBusy {
                Text(engine.t("Submitting transaction…", "正在提交交易…"))
                    .font(.caption2)
                    .foregroundColor(.orange)
            }
            if let r = engine.lastCtf {
                VStack(alignment: .leading, spacing: 2) {
                    Text(r.summary)
                        .font(.caption2.monospacedDigit())
                        .foregroundColor(.green)
                    if let hash = r.tx_hash {
                        Text(hash)
                            .font(.caption2.monospaced())
                            .foregroundColor(.secondary)
                            .lineLimit(1)
                    }
                }
            }
        }
        .alert(
            actionTitle(pendingAction ?? ""),
            isPresented: Binding(
                get: { pendingAction != nil },
                set: { if !$0 { pendingAction = nil } }
            )
        ) {
            Button(engine.t("Confirm", "确认执行"), role: isLive ? .destructive : nil) {
                if let act = pendingAction {
                    pendingAction = nil
                    engine.executeCtf(action: act, amountUsd: amount)
                }
            }
            Button(engine.t("Cancel", "取消"), role: .cancel) {
                pendingAction = nil
            }
        } message: {
            Text(confirmationMessage(pendingAction ?? ""))
        }
    }
}
