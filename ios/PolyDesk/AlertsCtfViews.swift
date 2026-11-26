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

// MARK: - v0.3.1: On-chain CTF split / merge / redeem

struct CtfPaneView: View {
    @ObservedObject var engine: EngineBridge
    @State private var amountText: String = "10"

    private var amount: Double {
        Double(amountText.replacingOccurrences(of: ",", with: ".")) ?? 0
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(engine.t(
                "Mint or burn complete Yes+No sets on-chain. LIVE EOA wallets only — use Cash Out in Paper mode.",
                "链上铸造/销毁完整 Yes+No 组合，仅限实盘 EOA 钱包；模拟盘请使用市价锁盈。"
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
            }

            HStack(spacing: 8) {
                Button(engine.t("Split", "拆分")) {
                    engine.executeCtf(action: "split", amountUsd: amount)
                }
                .buttonStyle(.borderedProminent)
                .disabled(engine.ctfBusy)

                Button(engine.t("Merge", "合并")) {
                    engine.executeCtf(action: "merge", amountUsd: amount)
                }
                .buttonStyle(.bordered)
                .disabled(engine.ctfBusy)

                Button(engine.t("Redeem", "赎回")) {
                    engine.executeCtf(action: "redeem", amountUsd: amount)
                }
                .buttonStyle(.bordered)
                .disabled(engine.ctfBusy)
            }
            .font(.caption.bold())

            if engine.ctfBusy {
                Text(engine.t("Submitting on-chain transaction…", "正在提交链上交易…"))
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
    }
}
