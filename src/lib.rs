pub mod alerts;
pub mod automation;
pub mod book;
pub mod config;
pub mod ctf;
pub mod execution;
pub mod i18n;
pub mod market;
pub mod news;
pub mod odds;
pub mod oracle_feed;
pub mod paper;
pub mod telemetry;
pub mod translate;
pub mod updater;
pub mod vault;
pub mod ws_feed;

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use parking_lot::RwLock;

use crate::alerts::AlertEngine;
use crate::automation::AutomationEngine;
use crate::config::{APP_VERSION, AppConfig, default_polygon_rpc_url};
use crate::execution::ExecutionEngine;
use crate::market::MarketClient;
use crate::oracle_feed::OracleFeed;
use crate::telemetry::Telemetry;
use crate::translate::Translator;
use crate::updater::{
    HotPatchApplySummary, HotPatchCompatibility, HotPatchManifest, clear_cached_hot_patch,
    load_cached_hot_patch, save_cached_hot_patch, validate_hot_patch_manifest,
};
use crate::ws_feed::{FeedEvent, MarketFeed};

#[derive(Clone)]
struct HotEndpointDefaults {
    gamma_host: String,
    clob_host: String,
    data_host: String,
    ws_market_url: String,
    polygon_rpc_url: String,
    binance_futures_ws_base: String,
    binance_spot_ws_base: String,
}

/// Complete in-process low-latency PolyDirect engine shared identically by
/// Linux/Windows/macOS native GPU apps AND the native iOS/iPadOS SwiftUI app (via FFI).
pub struct CoreEngine {
    pub config: Arc<RwLock<AppConfig>>,
    pub config_path: Option<PathBuf>,
    pub applied_hot_patch: Arc<RwLock<Option<HotPatchManifest>>>,
    default_endpoints: HotEndpointDefaults,
    pub market_client: Arc<MarketClient>,
    pub news: Arc<crate::news::NewsClient>,
    pub translator: Arc<Translator>,
    pub feed: Arc<MarketFeed>,
    pub oracle: Arc<OracleFeed>,
    pub alerts: Arc<AlertEngine>,
    pub execution: Arc<ExecutionEngine>,
    pub automation: Arc<AutomationEngine>,
    pub telemetry: Arc<Telemetry>,
}

impl CoreEngine {
    pub fn start(custom_config: Option<PathBuf>) -> anyhow::Result<Arc<Self>> {
        // Ensure a process-level rustls CryptoProvider is installed before any TLS
        // client (reqwest / tokio-tungstenite / alloy) initializes when both `ring`
        // and `aws-lc-rs` features are unified in the dependency graph.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let cfg = AppConfig::load(custom_config.as_ref())?;
        let default_endpoints = HotEndpointDefaults {
            gamma_host: cfg.gamma_host.clone(),
            clob_host: cfg.clob_host.clone(),
            data_host: cfg.data_host.clone(),
            ws_market_url: cfg.ws_market_url.clone(),
            polygon_rpc_url: cfg.polygon_rpc_url.clone(),
            binance_futures_ws_base: crate::oracle_feed::DEFAULT_FUTURES_WS_BASE.to_string(),
            binance_spot_ws_base: crate::oracle_feed::DEFAULT_SPOT_WS_BASE.to_string(),
        };
        let market_client = Arc::new(MarketClient::new(
            cfg.gamma_host.clone(),
            cfg.clob_host.clone(),
            cfg.data_host.clone(),
        )?);
        let news = Arc::new(crate::news::NewsClient::new()?);
        let translator = Arc::new(Translator::new(market_client.http().clone()));
        let config = Arc::new(RwLock::new(cfg.clone()));
        let telemetry = Arc::new(Telemetry::new());
        let feed = MarketFeed::spawn(
            cfg.ws_market_url.clone(),
            telemetry.clone(),
            cfg.whale_threshold_usd,
        );
        let oracle = OracleFeed::spawn();
        let alerts = AlertEngine::new(cfg.price_alerts.clone());
        let execution = ExecutionEngine::new(
            config.clone(),
            custom_config.clone(),
            market_client.clone(),
            feed.clone(),
            telemetry.clone(),
        );
        let automation = AutomationEngine::spawn(
            execution.clone(),
            feed.clone(),
            market_client.clone(),
        );

        let mut feed_rx = feed.subscribe_events();
        let alerts_ev = alerts.clone();
        let exec_ev = execution.clone();
        tokio::spawn(async move {
            loop {
                match feed_rx.recv().await {
                    Ok(FeedEvent::Book { view }) => {
                        let now_ms = chrono::Utc::now().timestamp_millis();
                        let fired = alerts_ev.evaluate_book(
                            &view.asset_id,
                            view.best_bid,
                            view.best_ask,
                            view.mid,
                            now_ms,
                        );
                        if !fired.is_empty() {
                            let snapshot = alerts_ev.list();
                            let _ = exec_ev.update_config_and_save(|c| c.price_alerts = snapshot);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    _ => {}
                }
            }
        });

        let engine = Arc::new(Self {
            config,
            config_path: custom_config.clone(),
            applied_hot_patch: Arc::new(RwLock::new(None)),
            default_endpoints,
            market_client,
            news,
            translator,
            feed,
            oracle,
            alerts,
            execution,
            automation,
            telemetry,
        });

        if let Some(cached_patch) = load_cached_hot_patch(engine.config_path.as_deref()) {
            let _ = engine.apply_hot_patch_in_memory(&cached_patch);
        }

        Ok(engine)
    }

    pub fn current_hot_patch(&self) -> Option<HotPatchManifest> {
        self.applied_hot_patch.read().clone()
    }

    pub fn apply_hot_patch(&self, patch: HotPatchManifest) -> anyhow::Result<HotPatchApplySummary> {
        match validate_hot_patch_manifest(&patch, APP_VERSION)? {
            HotPatchCompatibility::Compatible => {}
            HotPatchCompatibility::RequiresColdUpdate { min_version } => {
                return Err(anyhow!(
                    "Hot patch requires binary v{min_version} or newer (Cold Update required)"
                ));
            }
            HotPatchCompatibility::SupersededByNewerApp => {
                return Err(anyhow!(
                    "Hot patch was superseded by this newer binary version (v{APP_VERSION})"
                ));
            }
        }
        save_cached_hot_patch(self.config_path.as_deref(), &patch)?;
        self.apply_hot_patch_in_memory(&patch)
    }

    fn apply_hot_patch_in_memory(
        &self,
        patch: &HotPatchManifest,
    ) -> anyhow::Result<HotPatchApplySummary> {
        let translations_applied = self
            .translator
            .apply_hot_overrides(&patch.translations_en_to_zh, &patch.search_aliases_zh_to_en);

        let mut endpoints_updated = false;
        if let Some(ep) = &patch.endpoints {
            let g = ep.gamma_host.as_deref().map(str::trim).filter(|s| !s.is_empty());
            let d = ep.data_host.as_deref().map(str::trim).filter(|s| !s.is_empty());
            // NOTE: `clob_host` is intentionally NOT applied here. The executing CLOB
            // client is constructed once and authenticated sessions are host-scoped, so
            // letting a remote manifest move order submission created a three-way
            // desync (public_clob / MarketClient / authenticate_live) and a credential
            // exfiltration path. `validate_hot_patch_manifest` rejects any non-canonical
            // clob_host outright.
            if g.is_some() || d.is_some() {
                self.market_client.update_hosts(g, None, d);
                endpoints_updated = true;
            }
            if let Some(ws_url) = ep
                .ws_market_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                self.feed.update_ws_url(ws_url.to_string());
                endpoints_updated = true;
            }
            let bf = ep
                .binance_futures_ws_base
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let bs = ep
                .binance_spot_ws_base
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            if bf.is_some() || bs.is_some() {
                self.oracle.update_ws_endpoints(bf, bs);
                endpoints_updated = true;
            }
            if let Some(rpc) = ep
                .polygon_rpc_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let default_rpc = default_polygon_rpc_url();
                let mut cfg = self.config.write();
                if cfg.polygon_rpc_url == default_rpc {
                    cfg.polygon_rpc_url = rpc.to_string();
                    endpoints_updated = true;
                }
            }
        }

        *self.applied_hot_patch.write() = Some(patch.clone());

        Ok(HotPatchApplySummary {
            patch_id: patch.patch_id.clone(),
            revision: patch.revision,
            translations_applied,
            endpoints_updated,
        })
    }

    pub fn clear_hot_patch(&self) -> anyhow::Result<()> {
        clear_cached_hot_patch(self.config_path.as_deref())?;
        let had_endpoints = self
            .applied_hot_patch
            .read()
            .as_ref()
            .and_then(|p| p.endpoints.clone());
        *self.applied_hot_patch.write() = None;
        // Hot-patch translations live in the same map as the ambient translation
        // cache, so they must be explicitly unwound — otherwise "clear" leaves
        // attacker-supplied labels in place for the rest of the session.
        self.translator.clear_hot_overrides();
        self.market_client.update_hosts(
            Some(&self.default_endpoints.gamma_host),
            Some(&self.default_endpoints.clob_host),
            Some(&self.default_endpoints.data_host),
        );
        self.feed
            .update_ws_url(self.default_endpoints.ws_market_url.clone());
        self.oracle.update_ws_endpoints(
            Some(&self.default_endpoints.binance_futures_ws_base),
            Some(&self.default_endpoints.binance_spot_ws_base),
        );
        // Only unwind the RPC the patch actually changed: a user who picked a custom
        // RPC in Settings after launch must keep it.
        // Bind the read first: holding a read guard while taking the write guard on
        // the same `parking_lot::RwLock` would deadlock.
        let current_rpc = self.config.read().polygon_rpc_url.clone();
        if should_restore_rpc(
            had_endpoints.as_ref().is_some_and(|e| e.polygon_rpc_url.is_some()),
            &current_rpc,
            &default_polygon_rpc_url(),
        ) {
            let mut cfg = self.config.write();
            cfg.polygon_rpc_url = self.default_endpoints.polygon_rpc_url.clone();
        }
        Ok(())
    }
}

/// Decide whether `clear_hot_patch` may reset `polygon_rpc_url`.
///
/// Only when (a) the applied patch actually carried an RPC override and (b) the live
/// value is still something the patch could have installed. A custom RPC the operator
/// selected afterwards must never be silently reverted.
fn should_restore_rpc(patched_rpc: bool, current: &str, default: &str) -> bool {
    patched_rpc && current != default
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- §4.9: clearing a hot patch must not clobber a user-chosen RPC ---------------

    #[test]
    fn clear_hot_patch_rpc_restore_is_scoped_to_what_the_patch_changed() {
        let default = default_polygon_rpc_url();

        // The patch never carried an RPC -> never touch whatever the user has.
        assert!(!should_restore_rpc(false, "https://custom.example/rpc", &default));
        assert!(!should_restore_rpc(false, &default, &default));

        // The patch carried an RPC and the live value still differs from the default
        // (i.e. the patch's value is installed) -> restore.
        assert!(should_restore_rpc(true, "https://polygon-rpc.com", &default));

        // The patch carried an RPC but the live value is the default already
        // (user re-selected it) -> nothing to do.
        assert!(!should_restore_rpc(true, &default, &default));
    }
}
