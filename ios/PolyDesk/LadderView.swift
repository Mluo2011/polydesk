import SwiftUI

struct LadderView: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.colorScheme) private var scheme
    @State private var centerPrice: Double = 0.50
    @State private var initialized: Bool = false
    @State private var autoCenter: Bool = true

    private let presets: [Double] = [5, 10, 25, 50, 100]
    private let otypes = ["GTC", "FAK", "FOK", "TIMED_FOK", "SWEEP"]

    var body: some View {
        VStack(spacing: 6) {
            // Order type & PostOnly strip
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 5) {
                    ForEach(otypes, id: \.self) { ot in
                        Button(ot == "TIMED_FOK" ? "tFOK" : ot) {
                            engine.orderType = ot
                            if ot != "GTC" { engine.postOnly = false }
                        }
                        .font(.system(size: 11, weight: .semibold, design: .monospaced))
                        .padding(.horizontal, 8).padding(.vertical, 5)
                        .background(engine.orderType == ot ? Color.blue.opacity(0.22) : (scheme == .light ? Color.black.opacity(0.05) : Color.white.opacity(0.06)))
                        .cornerRadius(6)
                    }
                    Toggle(engine.t("PostOnly", "挂单"), isOn: $engine.postOnly)
                        .toggleStyle(.button)
                        .font(.system(size: 11, weight: .semibold))
                }
                .padding(.horizontal, 8)
            }

            // Stake Presets + Quick Actions
            HStack(spacing: 5) {
                ForEach(presets, id: \.self) { p in
                    Button("$\(Int(p))") { engine.stakeUsd = p }
                        .font(.system(size: 11, weight: .bold, design: .monospaced))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 6)
                        .background(abs(engine.stakeUsd - p) < 0.1 ? Color.blue.opacity(0.24) : (scheme == .light ? Color.black.opacity(0.05) : Color.white.opacity(0.06)))
                        .cornerRadius(6)
                }
                Button(engine.t("Green-Up", "锁盈")) { engine.executeGreenUp(strategy: "close") }
                    .font(.system(size: 11, weight: .bold))
                    .padding(.horizontal, 8).padding(.vertical, 6)
                    .background(Color.orange.opacity(0.22))
                    .foregroundColor(.orange)
                    .cornerRadius(6)
            }
            .padding(.horizontal, 8)

            // Transparent read-only fee preview bar (fixed monospacedDigit -> zero jitter!)
            let takerBps = engine.account?.builder_taker_bps ?? 0
            let makerBps = engine.account?.builder_maker_bps ?? 0
            let activeBps = (engine.orderType == "GTC" || engine.postOnly) ? makerBps : takerBps
            let estCut = engine.stakeUsd * Double(activeBps) / 10000.0
            HStack(spacing: 6) {
                Text(String(format: "%@ %4.1f¢", engine.t("Spread", "点差"), (engine.book?.spread ?? 0) * 100))
                Spacer()
                Text(String(format: "Dronepilot %dbp ($%.3f)", activeBps, estCut))
                    .foregroundColor(.secondary)
                    .lineLimit(1)
                Button(engine.t("Center", "居中")) {
                    if let m = engine.book?.mid { centerPrice = m }
                }
                .font(.system(size: 10.5, weight: .semibold))
                Button(engine.t("Auto", "跟踪")) {
                    autoCenter.toggle()
                    if autoCenter, let m = engine.book?.mid { centerPrice = m }
                }
                .font(.system(size: 10.5, weight: .semibold))
                .foregroundColor(autoCenter ? .cyan : .secondary)
            }
            .font(.system(size: 10.5).monospacedDigit())
            .foregroundColor(.secondary)
            .padding(.horizontal, 10)

            // Ladder Header
            HStack(spacing: 2) {
                Text(engine.t("MyB", "买挂")).frame(width: 40)
                Text(engine.t("Bid", "买深度")).frame(maxWidth: .infinity)
                Text(engine.t("Price", "价格")).frame(width: 70)
                Text(engine.t("Ask", "卖深度")).frame(maxWidth: .infinity)
                Text(engine.t("MyA", "卖挂")).frame(width: 40)
                Text("P&L").frame(width: 56)
            }
            .font(.system(size: 10, weight: .semibold))
            .foregroundColor(.secondary)
            .padding(.horizontal, 6)

            // Pooled Native Scroll Ladder (one-tap BUY on Bid col, one-tap SELL on Ask col!)
            // Uses latched centerPrice so rows NEVER jump under your finger on every live tick!
            let step = max(0.001, engine.ladderStep)
            let centerTick = Int((centerPrice / step).rounded())
            let ticks = ((-14)...14).reversed().map { centerTick + $0 }
            let bidMap = aggregate(engine.book?.bids ?? [], step: step, floorMode: true)
            let askMap = aggregate(engine.book?.asks ?? [], step: step, floorMode: false)
            let maxDepth = max(10.0, (bidMap.values.max() ?? 0), (askMap.values.max() ?? 0))

            ScrollView {
                LazyVStack(spacing: 1) {
                    ForEach(ticks, id: \.self) { k in
                        let price = ((Double(k) * step) * 10000).rounded() / 10000
                        if price >= 0.001 && price <= 0.999 {
                            let bSz = bidMap[k] ?? 0
                            let aSz = askMap[k] ?? 0
                            let myB = engine.activeOpenOrders.filter { $0.side == "BUY" && Int(($0.price / step).rounded()) == k }.reduce(0) { $0 + $1.remaining_size }
                            let myA = engine.activeOpenOrders.filter { $0.side == "SELL" && Int(($0.price / step).rounded()) == k }.reduce(0) { $0 + $1.remaining_size }

                            HStack(spacing: 2) {
                                // My Bid (tap to cancel)
                                Button {
                                    if myB > 0 { engine.cancelLevel(side: "BUY", price: price) }
                                } label: {
                                    Text(myB > 0 ? "×\(Int(myB))" : "")
                                        .font(.system(size: 10, weight: .bold, design: .monospaced))
                                        .foregroundColor(.orange)
                                        .frame(width: 40, height: 32)
                                        .background(myB > 0 ? Color.orange.opacity(0.18) : Color.clear)
                                }

                                // Bid Depth -> One-Tap BUY
                                Button {
                                    engine.placeOrder(side: "BUY", price: price)
                                } label: {
                                    ZStack(alignment: .trailing) {
                                        GeometryReader { g in
                                            Rectangle()
                                                .fill(Color.green.opacity(0.24))
                                                .frame(width: g.size.width * min(1, bSz / maxDepth))
                                                .frame(maxWidth: .infinity, alignment: .trailing)
                                        }
                                        Text(bSz > 0 ? fmtSz(bSz) : "")
                                            .font(.system(size: 11.5, design: .monospaced))
                                            .foregroundColor(scheme == .light ? Color(red: 0.02, green: 0.47, blue: 0.34) : Color(red: 0.6, green: 0.97, blue: 0.84))
                                            .padding(.trailing, 5)
                                    }
                                    .frame(height: 32)
                                }

                                // Center Price
                                Text(engine.oddsFmt.format(price, tick: step))
                                    .font(.system(size: 12, weight: .bold, design: .monospaced))
                                    .foregroundColor(.primary)
                                    .frame(width: 70, height: 32)
                                    .background(scheme == .light ? Color(red: 0.945, green: 0.961, blue: 0.976) : Color(red: 0.07, green: 0.09, blue: 0.14))

                                // Ask Depth -> One-Tap SELL
                                Button {
                                    engine.placeOrder(side: "SELL", price: price)
                                } label: {
                                    ZStack(alignment: .leading) {
                                        GeometryReader { g in
                                            Rectangle()
                                                .fill(Color.red.opacity(0.24))
                                                .frame(width: g.size.width * min(1, aSz / maxDepth))
                                        }
                                        Text(aSz > 0 ? fmtSz(aSz) : "")
                                            .font(.system(size: 11.5, design: .monospaced))
                                            .foregroundColor(scheme == .light ? Color(red: 0.74, green: 0.07, blue: 0.24) : Color(red: 1.0, green: 0.68, blue: 0.75))
                                            .padding(.leading, 5)
                                    }
                                    .frame(height: 32)
                                }

                                // My Ask (tap to cancel)
                                Button {
                                    if myA > 0 { engine.cancelLevel(side: "SELL", price: price) }
                                } label: {
                                    Text(myA > 0 ? "×\(Int(myA))" : "")
                                        .font(.system(size: 10, weight: .bold, design: .monospaced))
                                        .foregroundColor(.orange)
                                        .frame(width: 40, height: 32)
                                        .background(myA > 0 ? Color.orange.opacity(0.18) : Color.clear)
                                }

                                // What-If P&L
                                Group {
                                    if let pos = engine.activePosition {
                                        let pnl = pos.shares * (price - pos.avg_entry)
                                        Text(String(format: "%@$%.1f", pnl >= 0 ? "+" : "-", abs(pnl)))
                                            .foregroundColor(pnl >= 0 ? .green : .red)
                                    } else {
                                        Text("")
                                    }
                                }
                                .font(.system(size: 10, design: .monospaced))
                                .frame(width: 56, alignment: .trailing)
                            }
                            .padding(.horizontal, 4)
                            .background(scheme == .light ? Color.black.opacity(0.025) : Color.white.opacity(0.02))
                        }
                    }
                }
            }
            .transaction { $0.animation = nil }
        }
        .background(scheme == .light ? Color.white : Color(red: 0.04, green: 0.05, blue: 0.08))
        .onChange(of: engine.book?.mid) { newMid in
            guard let mid = newMid else { return }
            let step = max(0.001, engine.ladderStep)
            if !initialized {
                centerPrice = mid
                initialized = true
            } else if autoCenter && abs(mid - centerPrice) >= step * 6.0 {
                centerPrice = mid
            }
        }
        .onChange(of: engine.activeAssetId) { _ in
            initialized = false
            autoCenter = true
        }
    }

    private func aggregate(_ levels: [[Double]], step: Double, floorMode: Bool) -> [Int: Double] {
        var m: [Int: Double] = [:]
        for lvl in levels where lvl.count >= 2 {
            let k = floorMode ? Int(floor(lvl[0] / step + 1e-9)) : Int(ceil(lvl[0] / step - 1e-9))
            m[k, default: 0] += lvl[1]
        }
        return m
    }

    private func fmtSz(_ v: Double) -> String {
        if v >= 10000 { return String(format: "%.1fk", v / 1000) }
        if v >= 100 { return String(format: "%.0f", v) }
        return String(format: "%.1f", v)
    }
}
