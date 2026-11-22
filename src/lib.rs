pub mod automation;
pub mod book;
pub mod config;
pub mod execution;
pub mod i18n;
pub mod market;
pub mod odds;
pub mod paper;
pub mod telemetry;
pub mod translate;
pub mod updater;
pub mod vault;
pub mod ws_feed;

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::automation::AutomationEngine;
use crate::config::AppConfig;
use crate::execution::ExecutionEngine;
use crate::market::MarketClient;
use crate::telemetry::Telemetry;
use crate::translate::Translator;
use crate::ws_feed::MarketFeed;

/// Complete in-process low-latency PolyDesk engine shared identically by
/// Linux/Windows/macOS native GPU apps AND the native iOS/iPadOS SwiftUI app (via FFI).
pub struct CoreEngine {
    pub config: Arc<RwLock<AppConfig>>,
    pub market_client: Arc<MarketClient>,
    pub translator: Arc<Translator>,
    pub feed: Arc<MarketFeed>,
    pub execution: Arc<ExecutionEngine>,
    pub automation: Arc<AutomationEngine>,
    pub telemetry: Arc<Telemetry>,
}

impl CoreEngine {
    pub fn start(custom_config: Option<PathBuf>) -> anyhow::Result<Arc<Self>> {
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

        Ok(Arc::new(Self {
            config,
            market_client,
            translator,
            feed,
            execution,
            automation,
            telemetry,
        }))
    }
}
