import SwiftUI

// MARK: - v0.3.1: Chainlink oracle strip (5m / 15m Up-Down)

struct OracleStripView: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.colorScheme) private var scheme

    private var muted: Color {
        scheme == .light ? Color.black.opacity(0.55) : Color.white.opacity(0.55)
    }
    private var amber: Color {
        Color(red: 0.98, green: 0.75, blue: 0.25)
    }

    private func spotColor(_ f: ShortFrame) -> Color {
        if f.stale == true { return muted }
        guard let beat = f.beat, let spot = f.spot else { return .primary }
        return spot >= beat ? .green : .red
    }

    var body: some View {
        Group {
            if let f = engine.shortFrame, f.active {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 8) {
                        Text((f.symbol ?? "").uppercased())
                            .font(.caption2.monospaced().bold())
                            .foregroundColor(muted)
                        Text(EngineBridge.formatOracleUsd(f.spot))
                            .font(.system(.title3, design: .monospaced).weight(.bold))
                            .foregroundColor(spotColor(f))
                        if let beat = f.beat, let spot = f.spot {
                            let diff = spot - beat
                            let pct = beat != 0 ? diff / beat * 100.0 : 0.0
                            Text(String(format: "%+.2f (%+.2f%%)", diff, pct))
                                .font(.caption.monospacedDigit().bold())
                                .foregroundColor(f.stale == true ? muted : (diff >= 0 ? .green : .red))
                        }
                        if let m = engine.activeMarket {
                            let idx = engine.activeOutcomeIdx < m.outcomes.count ? engine.activeOutcomeIdx : 0
                            let staticPx = idx < m.outcome_prices.count ? m.outcome_prices[idx] : nil
                            let px = engine.book?.mid ?? engine.book?.best_ask ?? staticPx
                            Text("· \(m.displayOutcome(idx, zh: engine.zh)) \(engine.oddsFmt.format(px, tick: m.tick_size))")
                                .font(.caption.monospacedDigit())
                                .foregroundColor(muted)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 4)
                        if let secs = f.secs_left {
                            Text(String(format: "%02d:%02d", secs / 60, secs % 60))
                                .font(.caption.monospacedDigit().bold())
                                .foregroundColor(secs <= 30 ? .red : (secs <= 60 ? amber : .primary))
                        }
                        Button(engine.t("Next Round", "下一轮")) { engine.advanceNextRound() }
                            .font(.caption2.bold())
                            .buttonStyle(.bordered)
                    }
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 10) {
                            if let beat = f.beat {
                                Text("\(engine.t("Price to Beat", "目标基准价")) \(EngineBridge.formatOracleUsd(beat))")
                                    .font(.caption2.monospacedDigit().bold())
                                    .foregroundColor(amber)
                            }
                            if let twap = f.twap_60s {
                                Text("60s TWAP \(EngineBridge.formatOracleUsd(twap))")
                                    .font(.caption2.monospacedDigit())
                                    .foregroundColor(.secondary)
                            }
                            if let bm = f.binance_mid {
                                Text("Binance \(EngineBridge.formatOracleUsd(bm))")
                                    .font(.caption2.monospacedDigit())
                                    .foregroundColor(.secondary)
                            }
                            if f.stale == true {
                                Text(engine.t("STALE / RECONNECTING", "行情中断 / 重连中"))
                                    .font(.caption2.monospacedDigit())
                                    .foregroundColor(muted)
                            }
                        }
                    }
                }
                .padding(.horizontal, 9)
                .padding(.vertical, 6)
                .background(Color.gray.opacity(0.10))
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(Color.gray.opacity(0.35), lineWidth: 1)
                )
                .cornerRadius(6)
            }
        }
    }
}

// MARK: - v0.3.1: 5m / 15m 4-up grid (BTC / ETH / SOL / XRP)

struct Grid4UpView: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var scheme
    @State private var interval: String = "5m"

    private var muted: Color {
        scheme == .light ? Color.black.opacity(0.55) : Color.white.opacity(0.55)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 8) {
                Picker("", selection: $interval) {
                    Text("5m").tag("5m")
                    Text("15m").tag("15m")
                }
                .pickerStyle(.segmented)
                .padding(.horizontal, 12)
                .padding(.top, 8)
                .onChange(of: interval) { val in
                    engine.loadGrid(interval: val)
                }

                ScrollView {
                    LazyVGrid(
                        columns: [GridItem(.flexible(), spacing: 10), GridItem(.flexible(), spacing: 10)],
                        spacing: 10
                    ) {
                        ForEach(engine.gridMarkets) { entry in
                            tile(entry)
                        }
                    }
                    .padding(12)
                }
            }
            .navigationTitle(engine.t("5m / 15m 4-Up", "5m / 15m 四联分屏"))
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(engine.t("Close", "关闭")) { dismiss() }
                }
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        engine.loadGrid(interval: interval)
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                }
            }
        }
        .onAppear { engine.loadGrid(interval: interval) }
    }

    private func tile(_ entry: GridEntry) -> some View {
        let m = entry.market
        let f = entry.oracle
        let cid = m.condition_id
        let up = engine.activeMarket?.condition_id == cid
        return VStack(alignment: .leading, spacing: 6) {
            Text((f.coin ?? "").uppercased())
                .font(.caption.bold().monospaced())
                .foregroundColor(.primary)
            Text(m.displayQuestion(zh: engine.zh))
                .font(.caption2)
                .foregroundColor(.secondary)
                .lineLimit(2)
            HStack(spacing: 6) {
                Text(EngineBridge.formatOracleUsd(f.spot))
                    .font(.system(.body, design: .monospaced).weight(.bold))
                    .foregroundColor(colourFor(f))
                if let secs = f.secs_left {
                    Text(String(format: "%02d:%02d", secs / 60, secs % 60))
                        .font(.caption2.monospacedDigit())
                        .foregroundColor(secs <= 30 ? .red : muted)
                }
            }
            if let beat = f.beat {
                Text("\(engine.t("Beat", "基准")) \(EngineBridge.formatOracleUsd(beat))")
                    .font(.caption2.monospacedDigit())
                    .foregroundColor(Color(red: 0.98, green: 0.75, blue: 0.25))
            }
            if let twap = f.twap_60s {
                Text("TWAP \(EngineBridge.formatOracleUsd(twap))")
                    .font(.caption2.monospacedDigit())
                    .foregroundColor(muted)
            }
            HStack(spacing: 6) {
                ForEach(Array(m.outcomes.enumerated()), id: \.offset) { idx, _ in
                    let px: Double? = idx < m.outcome_prices.count ? m.outcome_prices[idx] : nil
                    Button {
                        engine.selectMarket(m, outcomeIdx: idx)
                        dismiss()
                    } label: {
                        Text("\(m.displayOutcome(idx, zh: engine.zh)) \(engine.oddsFmt.format(px, tick: m.tick_size))")
                            .font(.caption2.monospacedDigit().bold())
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 5)
                            .background((idx == 0 ? Color.green : Color.red).opacity(0.18))
                            .foregroundColor(idx == 0 ? .green : .red)
                            .cornerRadius(5)
                    }
                }
            }
        }
        .padding(9)
        .background(up ? Color.blue.opacity(0.12) : Color.gray.opacity(0.08))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(up ? Color.blue : Color.gray.opacity(0.35), lineWidth: 1)
        )
        .cornerRadius(8)
    }

    private func colourFor(_ f: ShortFrame) -> Color {
        if f.stale == true { return muted }
        guard let beat = f.beat, let spot = f.spot else { return .primary }
        return spot >= beat ? .green : .red
    }
}
