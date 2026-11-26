pub mod alerts;
pub mod automation;
pub mod book;
pub mod config;
pub mod ctf;
pub mod execution;
pub mod i18n;
pub mod market;
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

use parking_lot::RwLock;

use crate::alerts::AlertEngine;
use crate::automation::AutomationEngine;
use crate::config::AppConfig;
use crate::execution::ExecutionEngine;
use crate::market::MarketClient;
use crate::oracle_feed::OracleFeed;
use crate::telemetry::Telemetry;
use crate::translate::Translator;
use crate::ws_feed::{FeedEvent, MarketFeed};

/// Complete in-process low-latency PolyDirect engine shared identically by
/// Linux/Windows/macOS native GPU apps AND the native iOS/iPadOS SwiftUI app (via FFI).
pub struct CoreEngine {
    pub config: Arc<RwLock<AppConfig>>,
    pub market_client: Arc<MarketClient>,
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
        let market_client = Arc::new(MarketClient::new(
            cfg.gamma_host.clone(),
            cfg.clob_host.clone(),
            cfg.data_host.clone(),
        )?);
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
            custom_config,
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

        Ok(Arc::new(Self {
            config,
            market_client,
            translator,
            feed,
            oracle,
            alerts,
            execution,
            automation,
            telemetry,
        }))
    }
}
