# PolyDesk — 100% Native Low-Latency Polymarket CLOB V2 Terminal

**Zero browser. Zero Electron. Zero WebView. Zero localhost web server.**
Pure native local apps sharing one ultra-low-latency Rust CLOB V2 engine across **Linux**, **Windows 10/11**, **macOS**, and **iOS / iPadOS** — with built-in **English / 简体中文**, **Polymarket Bridge Real-Money Deposit**, **Polymarket CLOB V2 Builder Fee (Take a Cut)**, and **SHA-256 verified GitHub Auto-Update**.

---

## Native Platforms

| Platform | Tech Stack | Output Binary |
| :--- | :--- | :--- |
| **Linux** | Pure Rust + `eframe`/`egui` GPU (`glow` Wayland/X11) | `polydesk-linux-x86_64` |
| **Windows 10 / 11** | Pure Rust + `eframe`/`egui` GPU (`windows_subsystem = "windows"`, no console, no WebView2) | `polydesk-windows-x86_64.exe` |
| **iOS / iPadOS (iPad & iPhone)** | Native **SwiftUI + Apple Metal Canvas** statically linking Rust `libpolydesk_ffi.a` (`aarch64-apple-ios`) | `PolyDesk-iOS.ipa` |
| **macOS** | Pure Rust + `eframe`/`egui` GPU | `polydesk-macos-arm64` |

---

## How to Build & Distribute to Friends (With Auto-Update)

1. Push this folder to your GitHub repository.
2. Your **Dronepilot** Builder Code (`0x8b258cc9a239e2ebf724bd4097bcf3dbecd2d53ca28a7a74959fe48087135e5d`) is permanently compiled into every native binary (Linux, Windows 10/11, macOS, and iOS/iPadOS) — no GitHub Action variables or local config editing required.
3. Tag a release:
   ```bash
   git tag v0.2.0 && git push origin v0.2.0
   ```
4. GitHub Actions (`.github/workflows/release.yml`) automatically builds and publishes:
   - `polydesk-windows-x86_64.exe` (Windows 10/11 — double-click to run!)
   - `polydesk-linux-x86_64` (Linux — `chmod +x` and run!)
   - `polydesk-macos-arm64` (macOS Apple Silicon)
   - `PolyDesk-iOS.ipa` (iPad / iPhone — install via **SideStore**, **AltStore**, or **Sideloadly** using a free Apple ID, or sign for TestFlight!)
   - `SHA256SUMS.txt` (used by the built-in GitHub Auto-Updater in Linux/Windows/macOS).

---

## How to Add Real Money (充值真实资金)

Inside the app, click **Wallet / Deposit (钱包 / 充值)**:
1. Connect a dedicated trading wallet (EOA private key, Polymarket Proxy, Gnosis Safe, or Poly1271 deposit wallet) into the **100% local on-device Argon2id + AES-256-GCM encrypted vault** (private keys never leave the device; orders are signed locally via EIP-712).
2. Click **Get Deposit Addresses** — PolyDesk queries Polymarket's official Bridge API and displays your personal **EVM (Polygon/Arbitrum/Base/Ethereum)**, **Solana**, and **Bitcoin** deposit addresses.
3. Send USDC (or SOL/BTC) — Polymarket automatically converts it 1:1 into Polygon `pUSD` collateral (`0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB`).
4. Click **Sync Balance & Allowance** and switch from **PAPER** to **LIVE**.

> **Note:** Never put cold hardware-wallet seed phrases into any hot trading app; use a dedicated hot trading wallet. Polymarket live trading requires age 18+ and non-restricted jurisdictions.

---

## How Builder Fees Work (Dronepilot · Hardcoded)

PolyDesk uses **Polymarket CLOB V2's native on-chain Builder Fees** (zero custody, zero extra gas, zero latency hit):
1. The **Dronepilot** 32-byte `builderCode` is hardcoded in Rust (`src/config.rs`) across Linux, Windows 10/11, macOS, and iOS/iPadOS and cannot be modified by end users via config, UI, or environment variables.
2. Live Maker/Taker fee rates are synced read-only from Polymarket (`https://clob.polymarket.com/fees/builder-fees/...`) and controlled solely from `https://polymarket.com/settings?tab=builder`.
3. Every matched order routed through PolyDesk carries your `builderCode` on-chain. Polymarket automatically settles your fee cut **daily in `pUSD`** straight to your Builder Profile wallet (`$0.10`/day minimum floor).
4. Current rates are transparently displayed to users above the price ladder before placing orders.
