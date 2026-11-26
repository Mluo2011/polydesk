import Foundation
import Combine

// MARK: - Data Models (mirroring Rust polydesk-core JSON structs)

struct LatencyStats: Codable {
    let last: Double
    let p50: Double
    let p95: Double
}

struct TelemetrySnap: Codable {
    let ws_connected: Bool
    let ws_lag_ms: LatencyStats
    let book_apply_us: LatencyStats
    let sign_us: LatencyStats
    let order_rtt_ms: LatencyStats
    let warm_ping_ms: LatencyStats
    let routed_volume_usd: Double
    let total_protocol_fees_usd: Double
    let total_builder_fees_usd: Double
}

struct MarketItem: Codable, Identifiable {
    let id: String
    let question: String
    let question_zh: String?
    let slug: String
    let condition_id: String
    let outcomes: [String]
    let outcomes_zh: [String]?
    let token_ids: [String]
    let outcome_prices: [Double]
    let tick_size: Double
    let volume_24hr: Double
    let fees_enabled: Bool
    let category: String
    let event_slug: String?
    let neg_risk: Bool?
    let end_date: String?

    func displayQuestion(zh: Bool) -> String {
        if zh, let qz = question_zh, !qz.isEmpty { return qz }
        return question
    }

    func displayOutcome(_ idx: Int, zh: Bool) -> String {
        if zh, let oz = outcomes_zh, idx < oz.count, !oz[idx].isEmpty { return oz[idx] }
        return idx < outcomes.count ? outcomes[idx] : (idx == 0 ? (zh ? "是" : "Yes") : (zh ? "否" : "No"))
    }
}

struct BookSnap: Codable {
    let asset_id: String
    let tick_size: Double
    let best_bid: Double?
    let best_ask: Double?
    let mid: Double?
    let spread: Double?
    let imbalance: Double
    let last_trade_price: Double?
    let bids: [[Double]]
    let asks: [[Double]]
}

struct PricePt: Codable {
    let t: Int64
    let p: Double
}

struct OpenOrderItem: Codable, Identifiable {
    let id: String
    let asset_id: String
    let market_question: String
    let side: String
    let order_type: String
    let post_only: Bool
    let price: Double
    let original_size: Double
    let remaining_size: Double
}

struct PositionItem: Codable, Identifiable {
    var id: String { asset_id }
    let asset_id: String
    let condition_id: String
    let market_question: String
    let outcome: String
    let shares: Double
    let avg_entry: Double
    let realized_pnl: Double
}

struct PaperSnap: Codable {
    let cash_usd: Double
    let realized_pnl_usd: Double
    let open_orders: [OpenOrderItem]
    let positions: [PositionItem]
}

struct VaultMeta: Codable {
    let exists: Bool
    let address_hint: String?
}

struct AccountSnap: Codable {
    let mode: String
    let live_connected: Bool
    let wallet_address: String?
    let funder_address: String?
    let vault: VaultMeta
    let live_balance_usd: Double?
    let builder_code: String
    let builder_maker_bps: UInt32
    let builder_taker_bps: UInt32
    let builder_fee_locked: Bool
    let paper: PaperSnap
    let live_open_orders: [OpenOrderItem]
    let live_positions: [PositionItem]
}

struct AutoRuleItem: Codable, Identifiable {
    let id: String
    let kind: String
    let asset_id: String
    let side: String
    let trigger_price: Double
    let remaining_size: Double
    let active: Bool
    let status: String
}

struct CopyTargetItem: Codable, Identifiable {
    let id: String
    let wallet: String
    let label: String
    let multiplier: Double
    let max_order_usd: Double
    let enabled: Bool
    let copied_count: UInt32
}

struct GeneratedWalletInfo: Codable {
    let address: String
    let private_key: String
    let mnemonic: String?
}

struct DepositInfo: Codable {
    let target_wallet: String
    let evm_address: String
    let tron_address: String?
    let solana_address: String
    let btc_address: String
    let note: String?
}

private struct WalletCreatePayload: Decodable {
    let wallet: GeneratedWalletInfo
    let account: AccountSnap
    let deposit: DepositInfo?
}

struct GreenUpInfo: Codable {
    let close_price: Double
    let close_locked_pnl: Double
    let free_roll_remaining_shares: Double
    let can_free_roll: Bool
    let hedge_no_price: Double?
    let hedge_no_locked_pnl: Double?
}

struct HolderEntry: Codable, Identifiable {
    var id: String { proxy_wallet + token_id }
    let proxy_wallet: String
    let name: String
    let pseudonym: String
    let token_id: String
    let outcome_index: Int
    let amount: Double
}

struct WhaleSummary: Codable {
    let top_holders: [HolderEntry]
    let yes_whale_vol_usd: Double
    let no_whale_vol_usd: Double
    let net_flow_usd: Double
}

private struct FfiEnvelope<T: Decodable>: Decodable {
    let ok: Bool
    let data: T?
    let error: String?
}

private struct StatePayload: Decodable {
    let account: AccountSnap
    let book: BookSnap?
    let green_up: GreenUpInfo?
    let rules: [AutoRuleItem]
    let copy_targets: [CopyTargetItem]
    let telemetry: TelemetrySnap
}

enum OddsFmt: String, CaseIterable {
    case cents, prob, decimal, moneyline
    var label: String {
        switch self {
        case .cents: return "¢"
        case .prob: return "%"
        case .decimal: return "Dec"
        case .moneyline: return "ML"
        }
    }
    func next() -> OddsFmt {
        let all = Self.allCases
        return all[(all.firstIndex(of: self)! + 1) % all.count]
    }
    func format(_ p: Double?, tick: Double = 0.01) -> String {
        guard let raw = p, raw > 0 else { return "—" }
        let v = min(0.9999, max(0.0001, raw))
        let fmt = tick < 0.01 ? "%.1f" : "%.0f"
        switch self {
        case .cents: return String(format: "\(fmt)¢", v * 100)
        case .prob: return String(format: "\(fmt)%%", v * 100)
        case .decimal: return String(format: "%.2fx", 1.0 / v)
        case .moneyline:
            if v >= 0.5 { return "\(Int(-round((v / (1 - v)) * 100)))" }
            return "+\(Int(round(((1 - v) / v) * 100)))"
        }
    }
}

// MARK: - Direct In-Process Rust FFI Bridge (Zero Web, Zero HTTP Server)

final class EngineBridge: ObservableObject {
    @Published var zh: Bool = Locale.preferredLanguages.first?.hasPrefix("zh") == true
    @Published var oddsFmt: OddsFmt = .cents
    @Published var markets: [MarketItem] = []
    @Published var activeMarket: MarketItem?
    @Published var activeOutcomeIdx: Int = 0
    @Published var book: BookSnap?
    @Published var history: [PricePt] = []
    @Published var account: AccountSnap?
    @Published var telemetry: TelemetrySnap?
    @Published var rules: [AutoRuleItem] = []
    @Published var copyTargets: [CopyTargetItem] = []
    @Published var greenUp: GreenUpInfo?
    @Published var whales: WhaleSummary?
    @Published var depositInfo: DepositInfo?
    @Published var revealedWallet: GeneratedWalletInfo?
    @Published var toast: String?

    // v0.3.1 iOS parity
    @Published var shortFrame: ShortFrame?
    @Published var gridMarkets: [GridEntry] = []
    @Published var alerts: [AlertItem] = []
    @Published var ctfBusy: Bool = false
    @Published var lastCtf: CtfResult?

    @Published var orderType: String = "GTC"
    @Published var postOnly: Bool = false
    @Published var stakeUsd: Double = 25.0
    @Published var ladderStep: Double = 0.01
    @Published var chartInterval: String = "1d"

    private let queue = DispatchQueue(label: "com.polydesk.ffi", qos: .userInteractive)
    private var timer: Timer?
    private var oracleTimer: Timer?
    private var lastGridLoad: Date = .distantPast

    init() {
        let dataPath = Self.prepareSecureDataDirectory()
        dataPath.withCString { ptr in
            _ = polydesk_start(ptr)
        }
        loadMarkets(query: nil, category: "all")
        timer = Timer.scheduledTimer(withTimeInterval: 0.08, repeats: true) { [weak self] _ in
            self?.pollState()
        }
        oracleTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
            self?.pollShortFrame()
        }
    }

    private static func prepareSecureDataDirectory() -> String {
        let fm = FileManager.default
        let appSupport = fm.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? fm.urls(for: .documentDirectory, in: .userDomainMask).first!
        var dataDir = appSupport.appendingPathComponent("PolyDirect", isDirectory: true)
        let dirAlreadyExisted = fm.fileExists(atPath: dataDir.path)
        try? fm.createDirectory(at: dataDir, withIntermediateDirectories: true)

        var dirVals = URLResourceValues()
        dirVals.isExcludedFromBackup = true
        try? dataDir.setResourceValues(dirVals)

        let legacyAppSupportDir = appSupport.appendingPathComponent("PolyDesk", isDirectory: true)
        if !dirAlreadyExisted && fm.fileExists(atPath: legacyAppSupportDir.path) {
            for filename in ["vault.json", "config.toml"] {
                let legacyUrl = legacyAppSupportDir.appendingPathComponent(filename)
                var destUrl = dataDir.appendingPathComponent(filename)
                if fm.fileExists(atPath: legacyUrl.path) && !fm.fileExists(atPath: destUrl.path) {
                    do {
                        try fm.copyItem(at: legacyUrl, to: destUrl)
                        var fileVals = URLResourceValues()
                        fileVals.isExcludedFromBackup = true
                        try? destUrl.setResourceValues(fileVals)
                    } catch {
                        // Keep legacy file intact if copy failed
                    }
                }
            }
        }

        if let docsDir = fm.urls(for: .documentDirectory, in: .userDomainMask).first {
            for filename in ["vault.json", "config.toml"] {
                let legacyUrl = docsDir.appendingPathComponent(filename)
                var destUrl = dataDir.appendingPathComponent(filename)
                if fm.fileExists(atPath: legacyUrl.path) {
                    if !fm.fileExists(atPath: destUrl.path) {
                        do {
                            try fm.copyItem(at: legacyUrl, to: destUrl)
                            var fileVals = URLResourceValues()
                            fileVals.isExcludedFromBackup = true
                            try? destUrl.setResourceValues(fileVals)
                            try? fm.removeItem(at: legacyUrl)
                        } catch {
                            // Keep legacy file if copy failed
                        }
                    } else {
                        var fileVals = URLResourceValues()
                        fileVals.isExcludedFromBackup = true
                        try? destUrl.setResourceValues(fileVals)
                        let legacyData = try? Data(contentsOf: legacyUrl)
                        let destData = try? Data(contentsOf: destUrl)
                        if let ld = legacyData, let dd = destData, ld == dd {
                            try? fm.removeItem(at: legacyUrl)
                        } else {
                            var backupUrl = dataDir.appendingPathComponent("\(filename).legacy-backup")
                            if fm.fileExists(atPath: backupUrl.path) {
                                backupUrl = dataDir.appendingPathComponent("\(filename).legacy-backup-\(Int(Date().timeIntervalSince1970))")
                            }
                            do {
                                try fm.moveItem(at: legacyUrl, to: backupUrl)
                                var bakVals = URLResourceValues()
                                bakVals.isExcludedFromBackup = true
                                try? backupUrl.setResourceValues(bakVals)
                            } catch {
                                do {
                                    try fm.copyItem(at: legacyUrl, to: backupUrl)
                                    var bakVals = URLResourceValues()
                                    bakVals.isExcludedFromBackup = true
                                    try? backupUrl.setResourceValues(bakVals)
                                    try? fm.removeItem(at: legacyUrl)
                                } catch {
                                    // Keep differing legacy file intact if backup failed
                                }
                            }
                        }
                    }
                }
            }
        }
        return dataDir.path
    }

    deinit {
        timer?.invalidate()
        oracleTimer?.invalidate()
    }

    func t(_ en: String, _ cn: String) -> String { zh ? cn : en }

    var activeAssetId: String? {
        guard let m = activeMarket else { return nil }
        return activeOutcomeIdx < m.token_ids.count ? m.token_ids[activeOutcomeIdx] : m.token_ids.first
    }

    var oppositeAssetId: String? {
        guard let m = activeMarket, m.token_ids.count >= 2 else { return nil }
        return m.token_ids[activeOutcomeIdx == 0 ? 1 : 0]
    }

    var activePosition: PositionItem? {
        guard let aid = activeAssetId, let acc = account else { return nil }
        let list = acc.mode == "live" ? acc.live_positions : acc.paper.positions
        return list.first { $0.asset_id == aid && $0.shares > 1e-6 }
    }

    var activeOpenOrders: [OpenOrderItem] {
        guard let acc = account else { return [] }
        let list = acc.mode == "live" ? acc.live_open_orders : acc.paper.open_orders
        guard let aid = activeAssetId else { return list }
        return list.filter { $0.asset_id == aid }
    }

    private func call<T: Decodable>(_ payload: [String: Any], as type: T.Type, completion: @escaping (Result<T, Error>) -> Void) {
        queue.async {
            guard let jsonData = try? JSONSerialization.data(withJSONObject: payload),
                  let jsonStr = String(data: jsonData, encoding: .utf8) else { return }
            let rawPtr = jsonStr.withCString { polydesk_call($0) }
            guard let ptr = rawPtr else { return }
            let respStr = String(cString: ptr)
            polydesk_free_str(ptr)

            guard let data = respStr.data(using: .utf8) else { return }
            do {
                let env = try JSONDecoder().decode(FfiEnvelope<T>.self, from: data)
                DispatchQueue.main.async {
                    if env.ok, let d = env.data {
                        completion(.success(d))
                    } else {
                        completion(.failure(NSError(domain: "PolyDirect", code: 1, userInfo: [NSLocalizedDescriptionKey: env.error ?? "Unknown error"])))
                    }
                }
            } catch {
                DispatchQueue.main.async { completion(.failure(error)) }
            }
        }
    }

    func showToast(_ msg: String) {
        toast = msg
        DispatchQueue.main.asyncAfter(deadline: .now() + 3.0) { [weak self] in
            if self?.toast == msg { self?.toast = nil }
        }
    }

    func pollState() {
        var req: [String: Any] = ["op": "state"]
        if let aid = activeAssetId { req["active_asset_id"] = aid }
        if let opp = oppositeAssetId { req["opposite_asset_id"] = opp }
        call(req, as: StatePayload.self) { [weak self] res in
            guard let self, case .success(let s) = res else { return }
            self.account = s.account
            self.book = s.book
            self.greenUp = s.green_up
            self.rules = s.rules
            self.copyTargets = s.copy_targets
            self.telemetry = s.telemetry
        }
    }

    func loadMarkets(query: String?, category: String, offset: Int = 0, append: Bool = false) {
        var req: [String: Any] = [
            "op": "markets",
            "category": category,
            "limit": 80,
            "offset": offset,
            "lang": zh ? "zh" : "en",
        ]
        if let q = query, !q.isEmpty { req["query"] = q }
        call(req, as: [MarketItem].self) { [weak self] res in
            guard let self else { return }
            if case .success(let list) = res {
                if append {
                    for m in list where !self.markets.contains(where: { $0.condition_id == m.condition_id }) {
                        self.markets.append(m)
                    }
                } else {
                    self.markets = list
                }
                if let activeId = self.activeMarket?.condition_id,
                   let updated = self.markets.first(where: { $0.condition_id == activeId }) {
                    self.activeMarket = updated
                } else if self.activeMarket == nil, let first = self.markets.first {
                    self.selectMarket(first)
                }
            }
        }
    }

    func placeQuickOrder(side: String, price: Double, shares: Double, isLimit: Bool) {
        guard let m = activeMarket, let aid = activeAssetId, shares >= 0.1 else { return }
        let order: [String: Any] = [
            "asset_id": aid,
            "condition_id": m.condition_id,
            "market_question": m.question,
            "outcome": activeOutcomeIdx < m.outcomes.count ? m.outcomes[activeOutcomeIdx] : "Yes",
            "side": side,
            "order_type": isLimit ? "GTC" : "SWEEP",
            "post_only": false,
            "price": price,
            "size": (shares * 100).rounded() / 100,
            "fees_enabled": m.fees_enabled,
        ]
        call(["op": "place_order", "order": order], as: [String: AnyDecodable].self) { [weak self] res in
            switch res {
            case .success:
                self?.showToast("\(side) \(String(format: "%.1f", shares)) @ \(String(format: "%.1f¢", price * 100))")
            case .failure(let e):
                self?.showToast(e.localizedDescription)
            }
        }
    }

    func selectMarket(_ m: MarketItem, outcomeIdx: Int = 0) {
        activeMarket = m
        activeOutcomeIdx = outcomeIdx
        ladderStep = max(0.001, m.tick_size)
        book = nil
        history = []
        call(["op": "subscribe", "asset_ids": m.token_ids], as: [String: Int].self) { _ in }
        loadHistory(interval: chartInterval)
        call(["op": "whales", "condition_id": m.condition_id], as: WhaleSummary.self) { [weak self] res in
            if case .success(let w) = res { self?.whales = w }
        }
    }

    func loadHistory(interval: String) {
        chartInterval = interval
        guard let aid = activeAssetId else { return }
        call(["op": "history", "token_id": aid, "interval": interval], as: [PricePt].self) { [weak self] res in
            if case .success(let pts) = res { self?.history = pts }
        }
    }

    func placeOrder(side: String, price: Double) {
        guard let m = activeMarket, let aid = activeAssetId else { return }
        let shares = max(1.0, (stakeUsd / max(0.005, price) * 100).rounded() / 100)
        let order: [String: Any] = [
            "asset_id": aid,
            "condition_id": m.condition_id,
            "market_question": m.question,
            "outcome": activeOutcomeIdx < m.outcomes.count ? m.outcomes[activeOutcomeIdx] : "Yes",
            "side": side,
            "order_type": orderType,
            "post_only": postOnly,
            "price": price,
            "size": shares,
            "fees_enabled": m.fees_enabled,
        ]
        call(["op": "place_order", "order": order], as: [String: AnyDecodable].self) { [weak self] res in
            switch res {
            case .success:
                self?.showToast("\(side) \(String(format: "%.1f", shares)) @ \(String(format: "%.1f¢", price * 100))")
            case .failure(let e):
                self?.showToast(e.localizedDescription)
            }
        }
    }

    func cancelLevel(side: String, price: Double) {
        guard let aid = activeAssetId else { return }
        call(["op": "cancel_level", "asset_id": aid, "side": side, "price": price], as: [String: Int].self) { _ in }
    }

    func cancelAll() {
        call(["op": "cancel_all"], as: [String: Int].self) { [weak self] _ in
            self?.showToast(self?.t("Canceled all open orders", "已撤销全部挂单") ?? "")
        }
    }

    func executeGreenUp(strategy: String) {
        guard let aid = activeAssetId else { return }
        var req: [String: Any] = ["op": "green_up", "asset_id": aid, "strategy": strategy]
        if let opp = oppositeAssetId { req["opposite_asset_id"] = opp }
        call(req, as: [String: AnyDecodable].self) { [weak self] res in
            switch res {
            case .success: self?.showToast("Green-Up (\(strategy)) executed")
            case .failure(let e): self?.showToast(e.localizedDescription)
            }
        }
    }

    func armRule(kind: String, side: String, triggerCents: Double, trailCents: Double, shares: Double) {
        guard let m = activeMarket, let aid = activeAssetId else { return }
        var r: [String: Any] = [
            "kind": kind,
            "asset_id": aid,
            "condition_id": m.condition_id,
            "market_question": m.question,
            "outcome": activeOutcomeIdx < m.outcomes.count ? m.outcomes[activeOutcomeIdx] : "Yes",
            "side": side,
            "size": shares,
            "trigger_price": triggerCents / 100.0,
            "max_slippage_bps": 400.0,
        ]
        if kind == "trailing_stop" { r["trail_offset"] = trailCents / 100.0 }
        call(["op": "create_rule", "req": r], as: AutoRuleItem.self) { [weak self] res in
            if case .success = res { self?.showToast("Armed \(kind)") }
        }
    }

    func cancelRule(_ id: String) {
        call(["op": "cancel_rule", "id": id], as: [String: Bool].self) { _ in }
    }

    func addCopyTarget(wallet: String, label: String, mult: Double, maxUsd: Double) {
        let req: [String: Any] = [
            "wallet": wallet, "label": label, "multiplier": mult, "max_order_usd": maxUsd, "max_slippage_bps": 300.0
        ]
        call(["op": "add_copy", "req": req], as: CopyTargetItem.self) { [weak self] _ in
            self?.showToast("Tracking whale \(label)")
        }
    }

    func resetPaper(_ bal: Double) {
        call(["op": "reset_paper", "balance": bal], as: AccountSnap.self) { [weak self] res in
            if case .success(let acc) = res { self?.account = acc; self?.showToast("Paper balance reset") }
        }
    }

    func createNewWallet(password: String) {
        let req: [String: Any] = [
            "op": "wallet_create",
            "password": password,
            "persist_vault": true,
        ]
        call(req, as: WalletCreatePayload.self) { [weak self] res in
            switch res {
            case .success(let p):
                self?.revealedWallet = p.wallet
                self?.account = p.account
                if let dep = p.deposit { self?.depositInfo = dep }
                self?.showToast(self?.t("Created new local wallet!", "已生成本地新钱包！") ?? "")
            case .failure(let e):
                self?.showToast(e.localizedDescription)
            }
        }
    }

    func revealBackup(password: String) {
        call(["op": "wallet_reveal", "password": password], as: GeneratedWalletInfo.self) { [weak self] res in
            switch res {
            case .success(let w): self?.revealedWallet = w
            case .failure(let e): self?.showToast(e.localizedDescription)
            }
        }
    }

    func connectWallet(pk: String, funder: String, sigType: String, password: String) {
        var req: [String: Any] = [
            "op": "wallet_connect",
            "private_key": pk,
            "signature_type": sigType,
            "password": password,
            "persist_vault": true,
        ]
        if !funder.isEmpty { req["funder"] = funder }
        call(req, as: AccountSnap.self) { [weak self] res in
            switch res {
            case .success(let acc): self?.account = acc; self?.showToast("Live CLOB V2 wallet unlocked!")
            case .failure(let e): self?.showToast(e.localizedDescription)
            }
        }
    }

    func unlockVault(password: String) {
        call(["op": "wallet_unlock", "password": password], as: AccountSnap.self) { [weak self] res in
            switch res {
            case .success(let acc): self?.account = acc; self?.showToast("Vault unlocked!")
            case .failure(let e): self?.showToast(e.localizedDescription)
            }
        }
    }

    func lockWallet() {
        call(["op": "wallet_lock"], as: AccountSnap.self) { [weak self] res in
            switch res {
            case .success(let acc):
                self?.account = acc
                self?.showToast("Locked (PAPER mode)")
            case .failure(let e):
                self?.showToast(e.localizedDescription)
            }
        }
    }

    func syncAllowance() {
        call(["op": "sync_allowance"], as: AccountSnap.self) { [weak self] res in
            switch res {
            case .success(let acc): self?.account = acc; self?.showToast("Synced live pUSD balance & allowance")
            case .failure(let e): self?.showToast(e.localizedDescription)
            }
        }
    }

    func fetchDepositAddresses(wallet: String) {
        call(["op": "deposit_addresses", "wallet_addr": wallet], as: DepositInfo.self) { [weak self] res in
            switch res {
            case .success(let info): self?.depositInfo = info
            case .failure(let e): self?.showToast(e.localizedDescription)
            }
        }
    }

    // MARK: - v0.3.1 iOS parity: oracle strip, 5m grid, alerts, on-chain CTF

    /// Poll the live Chainlink spot / 60s TWAP / Price-to-Beat for the active 5m or 15m market.
    func pollShortFrame() {
        guard let m = activeMarket else {
            if shortFrame != nil { shortFrame = nil }
            return
        }
        let req: [String: Any] = [
            "op": "short_timeframe",
            "slug": m.slug,
            "event_slug": m.event_slug ?? "",
            "question": m.question,
            "condition_id": m.condition_id,
            "end_date": m.end_date ?? "",
        ]
        call(req, as: ShortFrame.self) { [weak self] res in
            guard let self else { return }
            switch res {
            case .success(let f):
                self.shortFrame = f.active ? f : nil
                // Keep the 4-up pool fresh so "Next Round" always has the live slot.
                if f.active, let secs = f.secs_left, secs <= 2,
                   Date().timeIntervalSince(self.lastGridLoad) > 15 {
                    self.loadGrid(interval: (f.interval_mins ?? 5) == 15 ? "15m" : "5m")
                }
            case .failure:
                self.shortFrame = nil
            }
        }
    }

    /// Load the 4-up 5m grid (BTC / ETH / SOL / XRP) with per-tile oracle data.
    func loadGrid(interval: String = "5m") {
        lastGridLoad = Date()
        call(["op": "short_timeframe_markets", "interval": interval, "limit": 4], as: GridPayload.self) { [weak self] res in
            guard let self else { return }
            switch res {
            case .success(let p): self.gridMarkets = p.markets
            case .failure(let e): self.showToast(e.localizedDescription)
            }
        }
    }

    /// Jump to the next slot for the same coin (used by the strip's Next Round button).
    func advanceNextRound() {
        guard let m = activeMarket else { return }
        let coin = String(m.slug.split(separator: "-").first ?? "")
        if let entry = gridMarkets.first(where: {
            $0.market.condition_id != m.condition_id
                && $0.market.slug.hasPrefix(coin)
                && ($0.oracle.secs_left ?? 0) > 0
        }) {
            selectMarket(entry.market)
            return
        }
        loadGrid(interval: (shortFrame?.interval_mins ?? 5) == 15 ? "15m" : "5m")
        showToast(t("Next round not live yet", "下一轮尚未上线"))
    }

    func loadAlerts() {
        call(["op": "alerts"], as: AlertsPayload.self) { [weak self] res in
            guard let self else { return }
            switch res {
            case .success(let p): self.alerts = p.alerts
            case .failure(let e): self.showToast(e.localizedDescription)
            }
        }
    }

    /// `kind` is `cross_above`, `cross_below` or `spread_above`; `thresholdCents` is in cents.
    func addAlert(kind: String, thresholdCents: Double) {
        guard let m = activeMarket, !m.token_ids.isEmpty else {
            showToast(t("Select a market first", "请先选择市场"))
            return
        }
        let idx = activeOutcomeIdx < m.token_ids.count ? activeOutcomeIdx : 0
        let req: [String: Any] = [
            "op": "add_alert",
            "asset_id": m.token_ids[idx],
            "market_question": m.question,
            "outcome": m.displayOutcome(idx, zh: false),
            "condition": ["kind": kind, "threshold": thresholdCents / 100.0],
            "sound_enabled": true,
        ]
        call(req, as: AlertsPayload.self) { [weak self] res in
            guard let self else { return }
            switch res {
            case .success(let p):
                self.alerts = p.alerts
                self.showToast(self.t("Alert armed", "预警已部署"))
            case .failure(let e): self.showToast(e.localizedDescription)
            }
        }
    }

    func removeAlert(_ id: String) {
        call(["op": "remove_alert", "id": id], as: AlertsPayload.self) { [weak self] res in
            guard let self else { return }
            if case .success(let p) = res { self.alerts = p.alerts }
        }
    }

    /// On-chain CTF `split` / `merge` / `redeem` for the active market (LIVE EOA wallets only).
    func executeCtf(action: String, amountUsd: Double) {
        guard let m = activeMarket, m.token_ids.count >= 2 else {
            showToast(t("Select a 2-outcome market", "请选择二元市场"))
            return
        }
        let tid0 = m.token_ids[0]
        let tid1 = m.token_ids[1]
        let positions = (account?.mode == "live" ? account?.live_positions : account?.paper.positions) ?? []
        let yesShares = positions.first(where: { $0.asset_id == tid0 })?.shares ?? 0
        let noShares = positions.first(where: { $0.asset_id == tid1 })?.shares ?? 0
        let autoAmount: Double = action == "merge" ? min(yesShares, noShares) : (yesShares + noShares)
        let amount = amountUsd > 0 ? amountUsd : autoAmount
        if amount <= 0 {
            showToast(t("Enter a positive amount", "请输入大于 0 的数量"))
            return
        }
        let req: [String: Any] = [
            "op": "ctf",
            "req": [
                "action": action,
                "condition_id": m.condition_id,
                "market_question": m.question,
                "yes_asset_id": tid0,
                "no_asset_id": tid1,
                "yes_outcome": m.outcomes.first ?? "Yes",
                "no_outcome": m.outcomes.count > 1 ? m.outcomes[1] : "No",
                "amount": amount,
                "yes_shares": yesShares,
                "no_shares": noShares,
                "neg_risk": m.neg_risk ?? false,
            ],
        ]
        ctfBusy = true
        call(req, as: CtfResult.self) { [weak self] res in
            guard let self else { return }
            self.ctfBusy = false
            switch res {
            case .success(let r):
                self.lastCtf = r
                self.showToast(r.summary)
                self.pollState()
            case .failure(let e):
                self.showToast(e.localizedDescription)
            }
        }
    }

}

struct AnyDecodable: Decodable {
    init(from decoder: Decoder) throws {
        let _ = try? decoder.singleValueContainer()
    }
}

// MARK: - v0.3.1 iOS parity models

struct ShortFrame: Codable {
    let active: Bool
    let coin: String?
    let symbol: String?
    let interval_mins: Int?
    let secs_left: Int?
    let beat: Double?
    let spot: Double?
    let twap_60s: Double?
    let binance_mid: Double?
    let stale: Bool?
}

struct GridEntry: Codable, Identifiable {
    let market: MarketItem
    let oracle: ShortFrame
    var id: String { market.condition_id }
}

struct GridPayload: Codable {
    let markets: [GridEntry]
}

struct AlertCond: Codable {
    let kind: String
    let threshold: Double
}

struct AlertItem: Codable, Identifiable {
    let id: String
    let asset_id: String
    let market_question: String
    let outcome: String
    let condition: AlertCond
    let active: Bool
    let sound_enabled: Bool
    let triggered_at_ms: Int64?
}

struct AlertsPayload: Codable {
    let alerts: [AlertItem]
}

struct CtfResult: Codable {
    let action: String
    let mode_label: String
    let tx_hash: String?
    let polygonscan_url: String?
    let summary: String
}
