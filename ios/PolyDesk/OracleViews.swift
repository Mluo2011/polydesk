import SwiftUI

// MARK: - v0.3.1: Chainlink oracle strip (5m / 15m Up-Down)

struct OracleStripView: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.colorScheme) private var scheme

    private func spotColor(_ f: ShortFrame) -> Color {
        if f.stale == true { return PolyTheme.textMuted(scheme) }
        guard let beat = f.beat, let spot = f.spot else { return .primary }
        return spot >= beat ? PolyTheme.emerald(scheme) : PolyTheme.crimson(scheme)
    }

    var body: some View {
        Group {
            if let f = engine.shortFrame, f.active {
                let muted = PolyTheme.textMuted(scheme)
                let amber = PolyTheme.amber(scheme)
                let stale = f.stale == true
                VStack(alignment: .leading, spacing: 6) {
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
                            let dCol = stale ? muted : (diff >= 0 ? PolyTheme.emeraldText(scheme) : PolyTheme.crimsonText(scheme))
                            let dBg = stale ? PolyTheme.subBg(scheme) : (diff >= 0 ? PolyTheme.emerald(scheme) : PolyTheme.crimson(scheme)).opacity(0.16)
                            Text(String(format: "%+.2f (%+.2f%%)", diff, pct))
                                .font(.caption2.monospacedDigit().bold())
                                .padding(.horizontal, 6).padding(.vertical, 2)
                                .background(dBg)
                                .foregroundColor(dCol)
                                .cornerRadius(4)
                        }
                        if let m = engine.activeMarket {
                            let idx = engine.activeOutcomeIdx < m.outcomes.count ? engine.activeOutcomeIdx : 0
                            let staticPx = idx < m.outcome_prices.count ? m.outcome_prices[idx] : nil
                            let px = engine.book?.mid ?? engine.book?.best_ask ?? staticPx
                            Text("\(m.displayOutcome(idx, zh: engine.zh)) \(engine.oddsFmt.format(px, tick: m.tick_size))")
                                .font(.caption2.monospacedDigit().bold())
                                .padding(.horizontal, 6).padding(.vertical, 2)
                                .background(PolyTheme.subBg(scheme))
                                .overlay(RoundedRectangle(cornerRadius: 4).stroke(PolyTheme.border(scheme), lineWidth: 1))
                                .foregroundColor(.secondary)
                                .lineLimit(1)
                        }
                        Spacer(minLength: 4)
                        if let secs = f.secs_left {
                            let total = Double(max(1, f.interval_mins ?? 5) * 60)
                            let frac = max(0.0, min(1.0, 1.0 - Double(secs) / total))
                            let tCol: Color = secs <= 30 ? PolyTheme.crimson(scheme) : (secs <= 60 ? amber : .primary)
                            VStack(alignment: .trailing, spacing: 2) {
                                Text(String(format: "%02d:%02d", secs / 60, secs % 60))
                                    .font(.caption.monospacedDigit().bold())
                                    .foregroundColor(tCol)
                                GeometryReader { g in
                                    ZStack(alignment: .leading) {
                                        Capsule().fill(PolyTheme.chipBg(scheme))
                                        Capsule().fill(tCol).frame(width: g.size.width * frac)
                                    }
                                }
                                .frame(width: 42, height: 3)
                            }
                        }
                        Button(engine.t("Next →", "下轮 →")) { engine.advanceNextRound() }
                            .font(.caption2.bold())
                            .buttonStyle(.bordered)
                    }
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 14) {
                            if let beat = f.beat {
                                VStack(alignment: .leading, spacing: 1) {
                                    Text(engine.t("PRICE TO BEAT", "目标基准价"))
                                        .font(.system(size: 9, weight: .semibold))
                                        .foregroundColor(muted)
                                    Text(EngineBridge.formatOracleUsd(beat))
                                        .font(.caption2.monospacedDigit().bold())
                                        .foregroundColor(amber)
                                }
                            }
                            if let twap = f.twap_60s {
                                VStack(alignment: .leading, spacing: 1) {
                                    Text("60s TWAP")
                                        .font(.system(size: 9, weight: .semibold))
                                        .foregroundColor(muted)
                                    Text(EngineBridge.formatOracleUsd(twap))
                                        .font(.caption2.monospacedDigit().bold())
                                        .foregroundColor(stale ? muted : (f.beat.map { twap >= $0 ? PolyTheme.emeraldText(scheme) : PolyTheme.crimsonText(scheme) } ?? .primary))
                                }
                            }
                            if !stale, let bm = f.binance_mid {
                                VStack(alignment: .leading, spacing: 1) {
                                    Text("BINANCE")
                                        .font(.system(size: 9, weight: .semibold))
                                        .foregroundColor(muted)
                                    Text(EngineBridge.formatOracleUsd(bm))
                                        .font(.caption2.monospacedDigit())
                                        .foregroundColor(.secondary)
                                }
                            }
                            if stale {
                                Text(engine.t("STALE / RECONNECTING", "行情中断 / 重连中"))
                                    .font(.caption2.monospacedDigit())
                                    .foregroundColor(muted)
                            }
                        }
                    }
                }
                .padding(.horizontal, 11)
                .padding(.vertical, 8)
                .background(PolyTheme.cardBg(scheme))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(PolyTheme.border(scheme), lineWidth: 1)
                )
                .cornerRadius(8)
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
            .background(PolyTheme.canvasBg(scheme))
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
        let muted = PolyTheme.textMuted(scheme)
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
                        .foregroundColor(secs <= 30 ? PolyTheme.crimson(scheme) : muted)
                }
            }
            if let beat = f.beat {
                Text("\(engine.t("Beat", "基准")) \(EngineBridge.formatOracleUsd(beat))")
                    .font(.caption2.monospacedDigit())
                    .foregroundColor(PolyTheme.amber(scheme))
            }
            if let twap = f.twap_60s {
                Text("TWAP \(EngineBridge.formatOracleUsd(twap))")
                    .font(.caption2.monospacedDigit())
                    .foregroundColor(muted)
            }
            HStack(spacing: 6) {
                ForEach(Array(m.outcomes.enumerated()), id: \.offset) { idx, _ in
                    let px: Double? = idx < m.outcome_prices.count ? m.outcome_prices[idx] : nil
                    let oc = idx == 0 ? PolyTheme.emerald(scheme) : PolyTheme.crimson(scheme)
                    Button {
                        engine.selectMarket(m, outcomeIdx: idx)
                        dismiss()
                    } label: {
                        Text("\(m.displayOutcome(idx, zh: engine.zh)) \(engine.oddsFmt.format(px, tick: m.tick_size))")
                            .font(.caption2.monospacedDigit().bold())
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 6)
                            .background(oc.opacity(0.16))
                            .foregroundColor(oc)
                            .cornerRadius(6)
                    }
                }
            }
        }
        .padding(10)
        .background(up ? PolyTheme.azure(scheme).opacity(0.12) : PolyTheme.cardBg(scheme))
        .overlay(
            RoundedRectangle(cornerRadius: 8)
                .stroke(up ? PolyTheme.azure(scheme) : PolyTheme.border(scheme), lineWidth: 1)
        )
        .cornerRadius(8)
    }

    private func colourFor(_ f: ShortFrame) -> Color {
        if f.stale == true { return PolyTheme.textMuted(scheme) }
        guard let beat = f.beat, let spot = f.spot else { return .primary }
        return spot >= beat ? PolyTheme.emerald(scheme) : PolyTheme.crimson(scheme)
    }
}
