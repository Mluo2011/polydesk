import SwiftUI

struct ChartView: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.colorScheme) private var scheme
    @State private var showCandles = false
    @State private var showEma = false
    @State private var showSma = false
    @State private var showBB = false
    @State private var showVwap = false
    @State private var showRsi = false

    private let intervals = ["1h", "6h", "1d", "1w", "1m", "max"]

    var body: some View {
        VStack(spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 5) {
                    Toggle("EMA9", isOn: $showEma).toggleStyle(.button).font(.caption2.monospaced())
                    Toggle("SMA20", isOn: $showSma).toggleStyle(.button).font(.caption2.monospaced())
                    Toggle("BB(20,2)", isOn: $showBB).toggleStyle(.button).font(.caption2.monospaced())
                    Toggle("VWAP", isOn: $showVwap).toggleStyle(.button).font(.caption2.monospaced())
                    Toggle("RSI(14)", isOn: $showRsi).toggleStyle(.button).font(.caption2.monospaced())

                    Divider().frame(height: 16)

                    Button(showCandles ? engine.t("Candles", "K线") : engine.t("Line", "折线")) {
                        showCandles.toggle()
                    }
                    .font(.caption2.bold())
                    .buttonStyle(.bordered)

                    ForEach(intervals, id: \.self) { iv in
                        Button(iv.uppercased()) {
                            engine.loadHistory(interval: iv)
                        }
                        .font(.caption2.monospaced())
                        .padding(.horizontal, 6).padding(.vertical, 4)
                        .background(engine.chartInterval == iv ? Color.blue.opacity(0.22) : (scheme == .light ? Color.black.opacity(0.05) : Color.white.opacity(0.05)))
                        .cornerRadius(5)
                    }
                }
            }

            Canvas { ctx, size in
                let bg = Path(CGRect(origin: .zero, size: size))
                let bgCol = scheme == .light ? Color.white : Color(red: 24.0 / 255.0, green: 26.0 / 255.0, blue: 31.0 / 255.0)
                ctx.fill(bg, with: .color(bgCol))

                let pts = engine.history
                guard pts.count >= 2 else { return }
                let prices = pts.map(\.p)

                let padR: CGFloat = 50
                let rsiH: CGFloat = (showRsi && size.height > 170) ? 46 : 0
                let plotW = max(10, size.width - padR - 10)
                let plotH = max(10, size.height - 20 - rsiH - (rsiH > 0 ? 6 : 0))

                var minP = prices.min() ?? 0.01
                var maxP = prices.max() ?? 0.99
                if let pos = engine.activePosition {
                    minP = min(minP, pos.avg_entry)
                    maxP = max(maxP, pos.avg_entry)
                }
                let span = max(0.02, (maxP - minP) * 0.15)
                minP = max(0.001, minP - span)
                maxP = min(0.999, maxP + span)
                let rng = max(0.005, maxP - minP)

                let minT = pts.map(\.t).min() ?? 0
                let maxT = max(minT + 1, pts.map(\.t).max() ?? (minT + 1))
                let spanT = Double(maxT - minT)

                func yOf(_ p: Double) -> CGFloat { 10 + CGFloat((maxP - p) / rng) * plotH }
                func xOf(_ i: Int) -> CGFloat {
                    let frac = Double(pts[i].t - minT) / spanT
                    return 8 + CGFloat(min(1.0, max(0.0, frac))) * plotW
                }

                // Bollinger Bands (20, 2σ)
                if showBB && prices.count >= 20 {
                    var upP = Path()
                    var loP = Path()
                    var started = false
                    for i in 19..<prices.count {
                        let slice = prices[(i - 19)...i]
                        let mean = slice.reduce(0, +) / 20.0
                        let sd = sqrt(slice.map { pow($0 - mean, 2) }.reduce(0, +) / 20.0)
                        let pu = CGPoint(x: xOf(i), y: yOf(min(0.999, mean + 2 * sd)))
                        let pl = CGPoint(x: xOf(i), y: yOf(max(0.001, mean - 2 * sd)))
                        if !started { upP.move(to: pu); loP.move(to: pl); started = true }
                        else { upP.addLine(to: pu); loP.addLine(to: pl) }
                    }
                    ctx.stroke(upP, with: .color(.indigo.opacity(0.6)), lineWidth: 1)
                    ctx.stroke(loP, with: .color(.indigo.opacity(0.6)), lineWidth: 1)
                }

                // Price curve or Candles
                let up = (prices.last ?? 0) >= (prices.first ?? 0)
                let lineCol: Color = up ? .green : .red
                if showCandles {
                    let spanT = max(60, (pts.last?.t ?? 0) - (pts.first?.t ?? 0))
                    let bucket = max(15, spanT / 40)
                    var buckets: [(k: Int64, o: Double, h: Double, l: Double, c: Double)] = []
                    for pt in pts {
                        let k = (pt.t / bucket) * bucket
                        if let lastIdx = buckets.indices.last, buckets[lastIdx].k == k {
                            buckets[lastIdx].h = max(buckets[lastIdx].h, pt.p)
                            buckets[lastIdx].l = min(buckets[lastIdx].l, pt.p)
                            buckets[lastIdx].c = pt.p
                        } else {
                            buckets.append((k, pt.p, pt.p, pt.p, pt.p))
                        }
                    }
                    let n = max(1, buckets.count)
                    let barW = min(12, max(3, (plotW / CGFloat(n)) * 0.65))
                    for (i, b) in buckets.enumerated() {
                        let cx = 6 + (CGFloat(i) + 0.5) / CGFloat(n) * plotW
                        let col: Color = b.c >= b.o ? .green : .red
                        var wick = Path()
                        wick.move(to: CGPoint(x: cx, y: yOf(b.h)))
                        wick.addLine(to: CGPoint(x: cx, y: yOf(b.l)))
                        ctx.stroke(wick, with: .color(col), lineWidth: 1)
                        let topY = yOf(max(b.o, b.c))
                        let botY = max(topY + 1.5, yOf(min(b.o, b.c)))
                        let body = CGRect(x: cx - barW / 2, y: topY, width: barW, height: botY - topY)
                        ctx.fill(Path(body), with: .color(col))
                    }
                } else {
                    var path = Path()
                    var area = Path()
                    for (i, p) in prices.enumerated() {
                        let pt = CGPoint(x: xOf(i), y: yOf(p))
                        if i == 0 {
                            path.move(to: pt)
                            area.move(to: CGPoint(x: pt.x, y: 10 + plotH))
                            area.addLine(to: pt)
                        } else {
                            path.addLine(to: pt)
                            area.addLine(to: pt)
                        }
                    }
                    if let lastIdx = prices.indices.last {
                        let lastPt = CGPoint(x: xOf(lastIdx), y: yOf(prices[lastIdx]))
                        area.addLine(to: CGPoint(x: lastPt.x, y: 10 + plotH))
                        area.closeSubpath()
                        ctx.fill(
                            area,
                            with: .linearGradient(
                                Gradient(colors: [lineCol.opacity(0.24), lineCol.opacity(0.0)]),
                                startPoint: CGPoint(x: 0, y: 10),
                                endPoint: CGPoint(x: 0, y: 10 + plotH)
                            )
                        )
                        ctx.stroke(path, with: .color(lineCol), lineWidth: 2.2)
                        let dot = CGRect(x: lastPt.x - 4, y: lastPt.y - 4, width: 8, height: 8)
                        ctx.fill(Path(ellipseIn: dot), with: .color(lineCol))
                        ctx.stroke(Path(ellipseIn: dot), with: .color(scheme == .light ? Color.white : Color(red: 0.067, green: 0.086, blue: 0.133)), lineWidth: 1.5)
                    }
                }

                // EMA 9
                if showEma {
                    var emaP = Path()
                    let k = 2.0 / 10.0
                    var prev = prices[0]
                    emaP.move(to: CGPoint(x: xOf(0), y: yOf(prev)))
                    for i in 1..<prices.count {
                        prev = prices[i] * k + prev * (1 - k)
                        emaP.addLine(to: CGPoint(x: xOf(i), y: yOf(prev)))
                    }
                    ctx.stroke(emaP, with: .color(.cyan), lineWidth: 1.2)
                }

                // SMA 20
                if showSma && prices.count >= 20 {
                    var smaP = Path()
                    var sum = 0.0
                    var started = false
                    for i in 0..<prices.count {
                        sum += prices[i]
                        if i >= 20 { sum -= prices[i - 20] }
                        if i >= 19 {
                            let pt = CGPoint(x: xOf(i), y: yOf(sum / 20.0))
                            if !started { smaP.move(to: pt); started = true } else { smaP.addLine(to: pt) }
                        }
                    }
                    ctx.stroke(smaP, with: .color(.yellow), lineWidth: 1.2)
                }

                // VWAP (running average)
                if showVwap {
                    var vwP = Path()
                    var sum = 0.0
                    for i in 0..<prices.count {
                        sum += prices[i]
                        let pt = CGPoint(x: xOf(i), y: yOf(sum / Double(i + 1)))
                        if i == 0 { vwP.move(to: pt) } else { vwP.addLine(to: pt) }
                    }
                    ctx.stroke(vwP, with: .color(.purple), lineWidth: 1.2)
                }

                // Avg entry & active SL/TP/Trail lines
                if let pos = engine.activePosition {
                    let y = yOf(pos.avg_entry)
                    var lp = Path(); lp.move(to: CGPoint(x: 6, y: y)); lp.addLine(to: CGPoint(x: 6 + plotW, y: y))
                    ctx.stroke(lp, with: .color(.cyan), style: StrokeStyle(lineWidth: 1.2, dash: [4, 3]))
                }
                for r in engine.rules where r.active && r.asset_id == engine.activeAssetId {
                    let y = yOf(r.trigger_price)
                    let col: Color = r.kind == "stop_loss" ? .red : (r.kind == "take_profit" ? .green : .orange)
                    var lp = Path(); lp.move(to: CGPoint(x: 6, y: y)); lp.addLine(to: CGPoint(x: 6 + plotW, y: y))
                    ctx.stroke(lp, with: .color(col), style: StrokeStyle(lineWidth: 1.2, dash: [4, 3]))
                }

                // RSI(14) Sub-pane
                if rsiH > 0 && prices.count > 14 {
                    let topY = 8 + plotH + 6
                    var g = 0.0, l = 0.0
                    for i in 1...14 {
                        let d = prices[i] - prices[i - 1]
                        if d >= 0 { g += d } else { l -= d }
                    }
                    g /= 14; l /= 14
                    func yRsi(_ v: Double) -> CGFloat { topY + CGFloat((100 - min(100, max(0, v))) / 100) * rsiH }
                    var rp = Path()
                    let r0 = l < 1e-12 ? 100.0 : 100.0 - 100.0 / (1.0 + g / l)
                    rp.move(to: CGPoint(x: xOf(14), y: yRsi(r0)))
                    for i in 15..<prices.count {
                        let d = prices[i] - prices[i - 1]
                        g = (g * 13 + max(0, d)) / 14
                        l = (l * 13 + max(0, -d)) / 14
                        let r = l < 1e-12 ? 100.0 : 100.0 - 100.0 / (1.0 + g / l)
                        rp.addLine(to: CGPoint(x: xOf(i), y: yRsi(r)))
                    }
                    ctx.stroke(rp, with: .color(.orange), lineWidth: 1.3)
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(scheme == .light ? Color.black.opacity(0.10) : Color.white.opacity(0.08), lineWidth: 1))
            .cornerRadius(10)
            .transaction { $0.animation = nil }
        }
    }
}
