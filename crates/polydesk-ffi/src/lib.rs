use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use polydesk::CoreEngine;
use polydesk::automation::{CreateCopyTargetRequest, CreateRuleRequest};
use polydesk::execution::{OrderRequest, ScaleEntryRequest, TradingMode};
use polydesk::vault::WalletSignatureType;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Runtime;

struct FfiHost {
    rt: Runtime,
    core: Arc<CoreEngine>,
}

static ENGINE: OnceLock<FfiHost> = OnceLock::new();

#[unsafe(no_mangle)]
pub unsafe extern "C" fn polydesk_start(data_dir_utf8: *const c_char) -> i32 {
    if ENGINE.get().is_some() {
        return 0;
    }
    let config_path = if !data_dir_utf8.is_null() {
        let s = unsafe { CStr::from_ptr(data_dir_utf8) }.to_string_lossy();
        if !s.trim().is_empty() {
            Some(PathBuf::from(s.trim()).join("config.toml"))
        } else {
            None
        }
    } else {
        None
    };

    let Ok(rt) = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    else {
        return -1;
    };

    let core = {
        let _guard = rt.enter();
        match CoreEngine::start(config_path) {
            Ok(c) => c,
            Err(_) => return -2,
        }
    };

    let _ = ENGINE.set(FfiHost { rt, core });
    0
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum FfiRequest {
    State { active_asset_id: Option<String>, opposite_asset_id: Option<String> },
    Markets { query: Option<String>, category: Option<String>, limit: Option<usize>, lang: Option<String> },
    Subscribe { asset_ids: Vec<String> },
    History { token_id: String, interval: Option<String> },
    Whales { condition_id: String },
    PlaceOrder { order: OrderRequest },
    ScaleEntry { req: ScaleEntryRequest },
    CancelOrder { order_id: String },
    CancelLevel { asset_id: String, side: Option<String>, price: f64 },
    CancelMarket { asset_id: String },
    CancelAll,
    GreenUp { asset_id: String, opposite_asset_id: Option<String>, strategy: String },
    CreateRule { req: CreateRuleRequest },
    CancelRule { id: String },
    AddCopy { req: CreateCopyTargetRequest },
    RemoveCopy { id: String },
    ResetPaper { balance: f64 },
    SetMode { mode: TradingMode },
    WalletConnect {
        private_key: String,
        funder: Option<String>,
        signature_type: WalletSignatureType,
        password: String,
        persist_vault: bool,
    },
    WalletUnlock { password: String },
    WalletLock,
    SyncAllowance,
    DepositAddresses { wallet_addr: String },
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn polydesk_call(request_json_utf8: *const c_char) -> *mut c_char {
    let out = match handle_call(request_json_utf8) {
        Ok(v) => json!({ "ok": true, "data": v }).to_string(),
        Err(e) => json!({ "ok": false, "error": e }).to_string(),
    };
    CString::new(out)
        .unwrap_or_else(|_| CString::new("{\"ok\":false,\"error\":\"null byte\"}").unwrap())
        .into_raw()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn polydesk_free_str(ptr: *mut c_char) {
    if !ptr.is_null() {
        unsafe {
            let _ = CString::from_raw(ptr);
        }
    }
}

fn handle_call(raw: *const c_char) -> Result<Value, String> {
    let host = ENGINE
        .get()
        .ok_or_else(|| "Engine not started; call polydesk_start first".to_string())?;
    if raw.is_null() {
        return Err("Null request".into());
    }
    let text = unsafe { CStr::from_ptr(raw) }
        .to_str()
        .map_err(|e| e.to_string())?;
    let req: FfiRequest = serde_json::from_str(text).map_err(|e| e.to_string())?;

    host.rt.block_on(async move {
        let c = &host.core;
        match req {
            FfiRequest::State {
                active_asset_id,
                opposite_asset_id,
            } => {
                let acc = c.execution.account_state().await;
                let book = active_asset_id
                    .as_deref()
                    .and_then(|id| c.feed.get_book_view(id, 60));
                let green_up = active_asset_id
                    .as_deref()
                    .and_then(|id| c.execution.preview_green_up(id, opposite_asset_id.as_deref()));
                let tape = c.feed.recent_tape(active_asset_id.as_deref(), 40);
                Ok(json!({
                    "account": acc,
                    "book": book,
                    "green_up": green_up,
                    "tape": tape,
                    "rules": c.automation.list_rules(),
                    "copy_targets": c.automation.list_copy_targets(),
                    "telemetry": c.telemetry.snapshot(),
                }))
            }
            FfiRequest::Markets {
                query,
                category,
                limit,
                lang,
            } => {
                let want_zh = lang.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("zh"));
                let ms = c
                    .market_client
                    .fetch_markets_bilingual(
                        query.as_deref(),
                        category.as_deref(),
                        limit.unwrap_or(40),
                        &c.translator,
                        want_zh,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(ms))
            }
            FfiRequest::Subscribe { asset_ids } => {
                c.feed.subscribe(asset_ids.clone());
                c.execution.prewarm_market(&asset_ids).await;
                if let Some(first) = asset_ids.first()
                    && c.feed.get_book_view(first, 1).is_none()
                    && let Ok((bids, asks, tick, lt, ts, hash)) =
                        c.market_client.fetch_book_rest(first).await
                {
                    c.feed.seed_book(first, "", tick, &bids, &asks, lt, ts, hash);
                }
                Ok(json!({"subscribed": asset_ids.len()}))
            }
            FfiRequest::History { token_id, interval } => {
                let pts = c
                    .market_client
                    .fetch_price_history(&token_id, interval.as_deref().unwrap_or("1d"), 10)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(pts))
            }
            FfiRequest::Whales { condition_id } => {
                let thresh = c.config.read().whale_threshold_usd;
                let w = c
                    .market_client
                    .fetch_whale_flow(&condition_id, thresh)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(w))
            }
            FfiRequest::PlaceOrder { order } => {
                let ack = c.execution.place_order(order).await.map_err(|e| e.to_string())?;
                Ok(json!(ack))
            }
            FfiRequest::ScaleEntry { req } => {
                let acks = c.execution.place_scale_entry(req).await.map_err(|e| e.to_string())?;
                Ok(json!(acks))
            }
            FfiRequest::CancelOrder { order_id } => {
                let n = c.execution.cancel_order(&order_id).await.map_err(|e| e.to_string())?;
                Ok(json!({"canceled": n}))
            }
            FfiRequest::CancelLevel { asset_id, side, price } => {
                let n = c
                    .execution
                    .cancel_at_price(&asset_id, side.as_deref(), price)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"canceled": n}))
            }
            FfiRequest::CancelMarket { asset_id } => {
                let n = c.execution.cancel_market(&asset_id).await.map_err(|e| e.to_string())?;
                Ok(json!({"canceled": n}))
            }
            FfiRequest::CancelAll => {
                let n = c.execution.cancel_all().await.map_err(|e| e.to_string())?;
                Ok(json!({"canceled": n}))
            }
            FfiRequest::GreenUp {
                asset_id,
                opposite_asset_id,
                strategy,
            } => {
                let ack = c
                    .execution
                    .execute_green_up(&asset_id, opposite_asset_id.as_deref(), &strategy)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(ack))
            }
            FfiRequest::CreateRule { req } => Ok(json!(c.automation.create_rule(req))),
            FfiRequest::CancelRule { id } => Ok(json!({"canceled": c.automation.cancel_rule(&id)})),
            FfiRequest::AddCopy { req } => Ok(json!(c.automation.add_copy_target(req))),
            FfiRequest::RemoveCopy { id } => Ok(json!({"removed": c.automation.remove_copy_target(&id)})),
            FfiRequest::ResetPaper { balance } => {
                c.execution.reset_paper(balance);
                Ok(json!(c.execution.account_state().await))
            }
            FfiRequest::SetMode { mode } => {
                c.execution.set_mode(mode).map_err(|e| e.to_string())?;
                Ok(json!(c.execution.account_state().await))
            }
            FfiRequest::WalletConnect {
                private_key,
                funder,
                signature_type,
                password,
                persist_vault,
            } => {
                let acc = c
                    .execution
                    .save_and_unlock_wallet(private_key, funder, signature_type, password, persist_vault)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(acc))
            }
            FfiRequest::WalletUnlock { password } => {
                let acc = c
                    .execution
                    .unlock_existing_vault(&password)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(acc))
            }
            FfiRequest::WalletLock => Ok(json!(c.execution.lock_wallet().await)),
            FfiRequest::SyncAllowance => {
                let acc = c
                    .execution
                    .sync_balance_allowance()
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(acc))
            }
            FfiRequest::DepositAddresses { wallet_addr } => {
                let dep = c
                    .execution
                    .fetch_deposit_addresses(&wallet_addr)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!(dep))
            }
        }
    })
}
