import SwiftUI

@main
struct PolyDeskApp: App {
    @StateObject private var engine = EngineBridge()

    var body: some Scene {
        WindowGroup {
            RootTerminalView(engine: engine)
                .preferredColorScheme(.dark)
        }
    }
}

struct RootTerminalView: View {
    @ObservedObject var engine: EngineBridge
    @Environment(\.horizontalSizeClass) private var hSize
    @State private var showWallet = false
    @State private var showMarketsRail = true
    @State private var rightMode = 0 // 0 = Quick Buy/Sell, 1 = Ladder, 2 = Pro Orders
    @State private var deckTab = 0   // 0 = Positions/Rules, 1 = Whales
    @State private var searchText = ""
    @State private var marketsOffset = 0
    @State private var selectedTab = 0

    @State private var quickBuy = true
    @State private var quickAmountText = "25"
    @State private var ruleKind = "stop_loss"
    @State private var triggerCents = 45.0
    @State private var trailCents = 3.0
    @State private var ruleShares = 25.0

    var body: some View {
        VStack(spacing: 0) {
            topBar
            Divider().overlay(Color.white.opacity(0.08))
            if hSize == .regular {
                // Modern, breathable iPad 3-Column Layout with collapsible Market Rail
                HStack(spacing: 0) {
                    if showMarketsRail {
                        marketsPane
                            .frame(width: 268)
                            .background(Color(red: 0.05, green: 0.07, blue: 0.11))
                        Divider().overlay(Color.white.opacity(0.08))
                    }
                    VStack(spacing: 10) {
                        marketBanner
                        ChartView(engine: engine).frame(minHeight: 230)
                        bottomDeckCard
                    }
                    .padding(12)
                    .background(Color(red: 0.04, green: 0.05, blue: 0.08))
                    Divider().overlay(Color.white.opacity(0.08))
                    VStack(spacing: 8) {
                        Picker("", selection: $rightMode) {
                            Text(engine.t("Buy / Sell", "快捷买卖")).tag(0)
                            Text(engine.t("Ladder", "盘口")).tag(1)
                            Text(engine.t("Pro", "条件单")).tag(2)
                        }
                        .pickerStyle(.segmented)
                        .padding(.horizontal, 10)
                        .padding(.top, 10)

                        if rightMode == 0 {
                            quickTradePane
                        } else if rightMode == 1 {
                            LadderView(engine: engine)
                        } else {
                            proOrdersPane
                        }
                    }
                    .frame(width: 405)
                    .background(Color(red: 0.05, green: 0.07, blue: 0.11))
                }
            } else {
                // Native iPhone / iPad Portrait Tabs
                TabView(selection: $selectedTab) {
                    VStack(spacing: 6) {
                        marketBanner
                        LadderView(engine: engine)
                    }
                    .tabItem { Label(engine.t("Ladder", "盘口"), systemImage: "list.bullet.rectangle") }
                    .tag(0)

                    VStack(spacing: 10) {
                        marketBanner
                        ChartView(engine: engine).frame(height: 250)
                        bottomDeckCard
                    }
                    .padding(10)
                    .tabItem { Label(engine.t("Chart", "图表"), systemImage: "chart.xyaxis.line") }
                    .tag(1)

                    marketsPane
                        .tabItem { Label(engine.t("Markets", "市场"), systemImage: "magnifyingglass") }
                        .tag(2)

                    proOrdersPane
                        .tabItem { Label(engine.t("Pro", "高级"), systemImage: "bolt.fill") }
                        .tag(3)
                }
            }
        }
        .background(Color(red: 0.04, green: 0.05, blue: 0.08))
        .overlay(alignment: .bottomTrailing) {
            if let msg = engine.toast {
                Text(msg)
                    .font(.caption.monospaced())
                    .padding(.horizontal, 12).padding(.vertical, 8)
                    .background(Color(red: 0.06, green: 0.08, blue: 0.12).opacity(0.96))
                    .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.cyan, lineWidth: 1))
                    .cornerRadius(8)
                    .padding(14)
            }
        }
        .sheet(isPresented: $showWallet) { WalletDepositSheet(engine: engine) }
    }

    private var topBar: some View {
        HStack(spacing: 10) {
            if hSize == .regular {
                Button { showMarketsRail.toggle() } label: {
                    Image(systemName: showMarketsRail ? "sidebar.left" : "line.3.horizontal")
                }
                .buttonStyle(.bordered)
            }

            Circle()
                .fill((engine.telemetry?.ws_connected ?? false) ? Color.green : Color.red)
                .frame(width: 9, height: 9)
            Text("PolyDesk").font(.headline.bold())

            let isLive = engine.account?.mode == "live"
            Button(isLive ? engine.t("LIVE", "实盘") : engine.t("PAPER", "模拟")) { showWallet = true }
                .font(.caption.bold())
                .padding(.horizontal, 10).padding(.vertical, 4)
                .background((isLive ? Color.green : Color.orange).opacity(0.2))
                .foregroundColor(isLive ? .green : .orange)
                .cornerRadius(99)

            let cash = isLive ? (engine.account?.live_balance_usd ?? 0) : (engine.account?.paper.cash_usd ?? 1000)
            let positions = (isLive ? engine.account?.live_positions : engine.account?.paper.positions) ?? []
            let posVal = positions.reduce(0.0) { $0 + $1.shares * $1.avg_entry }
            HStack(spacing: 8) {
                Text("\(engine.t("Portfolio", "总资产")) \(String(format: "$%.2f", cash + posVal))")
                    .font(.caption.monospacedDigit().bold())
                    .foregroundColor(.white)
                Text("\(engine.t("Cash", "余额")) \(String(format: "$%.2f", cash))")
                    .font(.caption.monospacedDigit().bold())
                    .foregroundColor(.green)
            }
            .padding(.horizontal, 10).padding(.vertical, 5)
            .background(Color.white.opacity(0.06))
            .cornerRadius(8)

            if let tel = engine.telemetry {
                Text(String(format: "CLOB %.0fms", tel.warm_ping_ms.p50))
                    .font(.caption2.monospacedDigit())
                    .foregroundColor(.secondary)
                    .frame(width: 76, alignment: .trailing)
            }

            Spacer()

            Button(engine.oddsFmt.label) { engine.oddsFmt = engine.oddsFmt.next() }
                .font(.caption.monospaced()).buttonStyle(.bordered)
            Button(engine.zh ? "EN" : "中文") {
                engine.zh.toggle()
                engine.loadMarkets(query: searchText.isEmpty ? nil : searchText, category: "all")
            }
            .font(.caption).buttonStyle(.bordered)
            Button(engine.t("Wallet / Deposit", "钱包 / 充值")) { showWallet = true }
                .font(.caption.bold()).buttonStyle(.borderedProminent)
            Button(engine.t("Cancel All", "全撤")) { engine.cancelAll() }
                .font(.caption.bold()).foregroundColor(.red).buttonStyle(.bordered)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        .background(Color(red: 0.05, green: 0.07, blue: 0.11))
        .transaction { $0.animation = nil }
    }

    private var marketBanner: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(engine.activeMarket?.displayQuestion(zh: engine.zh) ?? engine.t("Select a market", "请选择市场"))
                .font(.headline)
                .lineLimit(1)
            if let m = engine.activeMarket {
                HStack(spacing: 8) {
                    ForEach(Array(m.outcomes.enumerated()), id: \.offset) { idx, _ in
                        let name = m.displayOutcome(idx, zh: engine.zh)
                        let staticPx = idx < m.outcome_prices.count ? m.outcome_prices[idx] : nil
                        let px = (idx == engine.activeOutcomeIdx ? engine.book?.mid : nil) ?? staticPx
                        Button {
                            engine.selectMarket(m, outcomeIdx: idx)
                        } label: {
                            Text("\(name)  \(engine.oddsFmt.format(px, tick: m.tick_size))")
                                .font(.caption.monospacedDigit().bold())
                                .lineLimit(1)
                                .frame(width: 132)
                                .padding(.horizontal, 8).padding(.vertical, 6)
                                .background(engine.activeOutcomeIdx == idx ? (idx == 0 ? Color.green.opacity(0.24) : Color.red.opacity(0.24)) : Color.white.opacity(0.06))
                                .foregroundColor(idx == 0 ? .green : .red)
                                .cornerRadius(7)
                        }
                    }
                    Spacer()
                }
            }
        }
        .transaction { $0.animation = nil }
    }

    private var marketsPane: some View {
        VStack(spacing: 8) {
            HStack {
                TextField(engine.t("Search EN / 中文...", "搜索市场 (支持中/英文)..."), text: $searchText)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { engine.loadMarkets(query: searchText, category: "all") }
                Button {
                    engine.loadMarkets(query: searchText, category: "all")
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.bordered)
            }
            .padding([.horizontal, .top], 10)

            ScrollView {
                LazyVStack(spacing: 7) {
                    ForEach(engine.markets) { m in
                        let sel = engine.activeMarket?.condition_id == m.condition_id
                        Button {
                            engine.selectMarket(m)
                            selectedTab = 0
                        } label: {
                            VStack(alignment: .leading, spacing: 5) {
                                Text(m.displayQuestion(zh: engine.zh))
                                    .font(.caption)
                                    .foregroundColor(.white)
                                    .lineLimit(2)
                                    .multilineTextAlignment(.leading)
                                HStack {
                                    Text(engine.oddsFmt.format(m.outcome_prices.first, tick: m.tick_size)).foregroundColor(.green)
                                    Text("/").foregroundColor(.secondary)
                                    Text(engine.oddsFmt.format(m.outcome_prices.dropFirst().first, tick: m.tick_size)).foregroundColor(.red)
                                    Spacer()
                                    Text(String(format: "$%.0fk", m.volume_24hr / 1000)).foregroundColor(.secondary)
                                }
                                .font(.caption2.monospacedDigit())
                            }
                            .padding(10)
                            .background(sel ? Color.cyan.opacity(0.14) : Color.white.opacity(0.04))
                            .overlay(RoundedRectangle(cornerRadius: 8).stroke(sel ? Color.cyan : Color.white.opacity(0.07), lineWidth: 1))
                            .cornerRadius(8)
                        }
                    }
                    if searchText.isEmpty && !engine.markets.isEmpty {
                        Button(engine.t("Load More Markets (+80)", "加载更多市场 (+80)")) {
                            marketsOffset += 80
                            engine.loadMarkets(query: nil, category: "all", offset: marketsOffset, append: true)
                        }
                        .font(.caption.bold())
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 8)
                        .background(Color.white.opacity(0.06))
                        .cornerRadius(8)
                    }
                }
                .padding(.horizontal, 10).padding(.bottom, 10)
            }
        }
    }

    private var quickTradePane: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Picker("", selection: $quickBuy) {
                    Text(engine.t("Buy", "买入")).tag(true)
                    Text(engine.t("Sell", "卖出")).tag(false)
                }
                .pickerStyle(.segmented)

                if let m = engine.activeMarket {
                    HStack(spacing: 8) {
                        ForEach(Array(m.outcomes.prefix(2).enumerated()), id: \.offset) { idx, _ in
                            let name = m.displayOutcome(idx, zh: engine.zh)
                            let staticPx = idx < m.outcome_prices.count ? m.outcome_prices[idx] : nil
                            let px = (idx == engine.activeOutcomeIdx ? (quickBuy ? engine.book?.best_ask : engine.book?.best_bid) ?? engine.book?.mid : nil) ?? staticPx
                            Button {
                                engine.selectMarket(m, outcomeIdx: idx)
                            } label: {
                                VStack(spacing: 3) {
                                    Text(name).font(.caption.bold()).lineLimit(1)
                                    Text(engine.oddsFmt.format(px, tick: m.tick_size)).font(.subheadline.monospacedDigit().bold())
                                }
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 10)
                                .background(engine.activeOutcomeIdx == idx ? (idx == 0 ? Color.green.opacity(0.26) : Color.red.opacity(0.26)) : Color.white.opacity(0.06))
                                .foregroundColor(idx == 0 ? .green : .red)
                                .cornerRadius(10)
                            }
                        }
                    }

                    let amt = Double(quickAmountText) ?? 0
                    let px = (quickBuy ? (engine.book?.best_ask ?? engine.book?.mid) : (engine.book?.best_bid ?? engine.book?.mid))
                        ?? (engine.activeOutcomeIdx < m.outcome_prices.count ? m.outcome_prices[engine.activeOutcomeIdx] : 0.5)
                    let shares = quickBuy ? (px > 0.001 ? amt / px : 0) : amt

                    Text(quickBuy ? engine.t("Amount ($)", "买入金额 ($)") : engine.t("Shares to Sell", "卖出份数"))
                        .font(.caption).foregroundColor(.secondary)
                    TextField("25", text: $quickAmountText)
                        .keyboardType(.decimalPad)
                        .font(.title3.monospacedDigit().bold())
                        .padding(10)
                        .background(Color.white.opacity(0.06))
                        .cornerRadius(9)

                    HStack(spacing: 6) {
                        ForEach([1, 10, 50, 100], id: \.self) { v in
                            Button("+$\(v)") {
                                quickAmountText = String(format: "%.0f", amt + Double(v))
                            }
                            .font(.caption.monospacedDigit().bold())
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 7)
                            .background(Color.white.opacity(0.07))
                            .cornerRadius(7)
                        }
                    }

                    VStack(spacing: 6) {
                        HStack {
                            Text(engine.t("Avg Price", "预计均价")).foregroundColor(.secondary)
                            Spacer()
                            Text(engine.oddsFmt.format(px, tick: m.tick_size)).monospacedDigit().bold()
                        }
                        HStack {
                            Text(engine.t("Est. Shares", "预计份数")).foregroundColor(.secondary)
                            Spacer()
                            Text(String(format: "%.2f", shares)).monospacedDigit().bold()
                        }
                        HStack {
                            Text(quickBuy ? engine.t("To Win", "获胜预期回报") : engine.t("Est. Receive", "预计到账")).bold()
                            Spacer()
                            Text(String(format: "$%.2f", quickBuy ? shares : shares * px))
                                .monospacedDigit().bold().foregroundColor(.green)
                        }
                    }
                    .font(.caption)
                    .padding(12)
                    .background(Color.white.opacity(0.04))
                    .cornerRadius(10)

                    Button {
                        engine.placeQuickOrder(side: quickBuy ? "BUY" : "SELL", price: px, shares: shares, isLimit: false)
                    } label: {
                        Text(quickBuy
                             ? "\(engine.t("Buy", "立即买入")) \(m.displayOutcome(engine.activeOutcomeIdx, zh: engine.zh)) · $\(String(format: "%.0f", amt))"
                             : "\(engine.t("Sell", "立即卖出")) \(m.displayOutcome(engine.activeOutcomeIdx, zh: engine.zh)) · \(String(format: "%.1f", shares)) sh")
                            .font(.headline.bold())
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 13)
                            .background(quickBuy ? Color.green : Color.red)
                            .foregroundColor(quickBuy ? .black : .white)
                            .cornerRadius(11)
                    }
                }
            }
            .padding(12)
        }
    }

    private var bottomDeckCard: some View {
        VStack(alignment: .leading, spacing: 8) {
            Picker("", selection: $deckTab) {
                Text(engine.t("Positions & Rules", "持仓与条件单")).tag(0)
                Text(engine.t("Whales & Copy", "大户与跟单")).tag(1)
            }
            .pickerStyle(.segmented)

            ScrollView {
                if deckTab == 0 {
                    VStack(alignment: .leading, spacing: 8) {
                        let positions = (engine.account?.mode == "live" ? engine.account?.live_positions : engine.account?.paper.positions) ?? []
                        if positions.isEmpty {
                            Text(engine.t("Tap Bid/Ask on the Price Ladder to open a position.", "点击盘口深度格即可极速下单。"))
                                .font(.caption).foregroundColor(.secondary)
                        } else {
                            ForEach(positions) { p in
                                HStack {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(p.market_question).font(.caption).lineLimit(1)
                                        Text(String(format: "%@ · %.1f sh @ %.1f¢", p.outcome, p.shares, p.avg_entry * 100))
                                            .font(.caption2.monospacedDigit()).foregroundColor(.secondary)
                                    }
                                    Spacer()
                                    Button(engine.t("Cash Out", "锁盈")) { engine.executeGreenUp(strategy: "close") }
                                        .font(.caption2).buttonStyle(.bordered)
                                }
                            }
                        }
                        ForEach(engine.rules.filter(\.active)) { r in
                            HStack {
                                Text("\(r.kind.uppercased()) \(r.side) @ \(String(format: "%.1f¢", r.trigger_price * 100))")
                                    .font(.caption2.monospacedDigit()).foregroundColor(.orange)
                                Spacer()
                                Button("×") { engine.cancelRule(r.id) }.foregroundColor(.red)
                            }
                        }
                    }
                } else if let wf = engine.whales {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text(engine.t("Net Whale Flow", "鲸鱼净流向")).font(.caption.bold())
                            Spacer()
                            Text(String(format: "%@$%.0f", wf.net_flow_usd >= 0 ? "+" : "-", abs(wf.net_flow_usd)))
                                .font(.caption.monospacedDigit().bold())
                                .foregroundColor(wf.net_flow_usd >= 0 ? .green : .red)
                        }
                        ForEach(wf.top_holders.prefix(10)) { h in
                            let lbl = !h.name.isEmpty ? h.name : (!h.pseudonym.isEmpty ? h.pseudonym : String(h.proxy_wallet.prefix(10)))
                            HStack {
                                Text("\(lbl) (\(h.outcome_index == 0 ? "Y" : "N") \(Int(h.amount)))").font(.caption).lineLimit(1)
                                Spacer()
                                Button(engine.t("Copy", "跟单")) {
                                    engine.addCopyTarget(wallet: h.proxy_wallet, label: lbl, mult: 0.05, maxUsd: 25)
                                }
                                .font(.caption2).buttonStyle(.bordered)
                            }
                        }
                    }
                }
            }
        }
        .padding(12)
        .background(Color(red: 0.07, green: 0.09, blue: 0.14))
        .overlay(RoundedRectangle(cornerRadius: 10).stroke(Color.white.opacity(0.08), lineWidth: 1))
        .cornerRadius(10)
    }

    private var proOrdersPane: some View {
        Form {
            Section(engine.t("Rust Engine Conditional Exit", "引擎条件单（本地微秒触发）")) {
                Picker(engine.t("Type", "策略"), selection: $ruleKind) {
                    Text(engine.t("Stop-Loss", "止损")).tag("stop_loss")
                    Text(engine.t("Take-Profit", "止盈")).tag("take_profit")
                    Text(engine.t("Trailing Stop", "移动止损")).tag("trailing_stop")
                }
                Stepper("\(engine.t("Trigger", "触发价")): \(String(format: "%.1f¢", triggerCents))", value: $triggerCents, in: 1...99, step: 1)
                if ruleKind == "trailing_stop" {
                    Stepper("\(engine.t("Trail", "回撤")): \(String(format: "%.1f¢", trailCents))", value: $trailCents, in: 1...30, step: 0.5)
                }
                Stepper("\(engine.t("Shares", "份数")): \(Int(ruleShares))", value: $ruleShares, in: 5...5000, step: 5)
                Button(engine.t("Arm Engine Rule", "部署条件单")) {
                    engine.armRule(kind: ruleKind, side: "SELL", triggerCents: triggerCents, trailCents: trailCents, shares: ruleShares)
                }
            }

            Section(engine.t("Fee Schedule (Dronepilot · Hardcoded)", "透明费率 (Dronepilot · 锁定)")) {
                HStack {
                    Text(engine.t("Taker / Maker", "吃单 / 挂单"))
                    Spacer()
                    Text("\(engine.account?.builder_taker_bps ?? 0)bp / \(engine.account?.builder_maker_bps ?? 0)bp")
                        .font(.caption.monospacedDigit()).foregroundColor(.secondary)
                }
            }
        }
    }
}
