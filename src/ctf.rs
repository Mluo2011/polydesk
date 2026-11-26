use std::str::FromStr;

use alloy::primitives::{B256, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::signers::Signer as _;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use anyhow::{Context, Result, anyhow};
use polymarket_client_sdk_v2::ctf::Client as CtfClient;
use polymarket_client_sdk_v2::ctf::types::{
    MergePositionsRequest, RedeemNegRiskRequest, RedeemPositionsRequest, SplitPositionRequest,
};
use polymarket_client_sdk_v2::{POLYGON, contract_config};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::{Deserialize, Serialize};

use crate::config::{redact_rpc_url, validate_polygon_rpc_url, verify_polygon_rpc_dns};
use crate::vault::WalletSignatureType;

sol! {
    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IERC1155 {
        function balanceOf(address account, uint256 id) external view returns (uint256);
        function isApprovedForAll(address account, address operator) external view returns (bool);
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    interface INegRiskSplitMerge {
        function splitPosition(bytes32 conditionId, uint256 amount) external;
        function mergePositions(bytes32 conditionId, uint256 amount) external;
        function redeemPositions(bytes32 conditionId, uint256[] calldata amounts) external;
    }

    #[allow(missing_docs)]
    #[sol(rpc)]
    interface IConditionalTokensGas {
        function splitPosition(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata partition,
            uint256 amount
        ) external;
        function mergePositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata partition,
            uint256 amount
        ) external;
        function redeemPositions(
            address collateralToken,
            bytes32 parentCollectionId,
            bytes32 conditionId,
            uint256[] calldata indexSets
        ) external;
    }
}

pub const MIN_EOA_POL_GAS_WEI: u128 = 10_000_000_000_000_000; // 0.01 POL
pub const MAX_CTF_AMOUNT_USD: u64 = 10_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CtfAction {
    Split,
    Merge,
    Redeem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtfRequest {
    pub action: CtfAction,
    pub condition_id: String,
    pub market_question: String,
    pub yes_asset_id: String,
    pub no_asset_id: String,
    pub yes_outcome: String,
    pub no_outcome: String,
    pub amount: f64,
    #[serde(default)]
    pub yes_shares: f64,
    #[serde(default)]
    pub no_shares: f64,
    pub neg_risk: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtfTxResult {
    pub action: CtfAction,
    pub mode_label: String,
    pub tx_hash: Option<String>,
    pub polygonscan_url: Option<String>,
    pub summary: String,
}

pub fn ensure_eoa_for_direct_ctf(sig_type: WalletSignatureType) -> Result<()> {
    match sig_type {
        WalletSignatureType::Eoa => Ok(()),
        WalletSignatureType::Proxy | WalletSignatureType::GnosisSafe | WalletSignatureType::Poly1271 => {
            Err(anyhow!(
                "Direct on-chain CTF Split/Merge/Redeem is only supported for EOA wallets. Proxy/Safe wallets hold tokens in your smart contract — use https://polymarket.com/portfolio instead."
            ))
        }
    }
}

pub fn usd_to_usdc_raw(amount_usd: f64) -> Result<U256> {
    if !amount_usd.is_finite() || amount_usd <= 0.0 {
        return Err(anyhow!("CTF amount must be a positive finite number"));
    }
    let dec = Decimal::from_f64(amount_usd)
        .ok_or_else(|| anyhow!("CTF amount exceeds supported decimal range"))?;
    if dec <= Decimal::ZERO || dec > Decimal::from(MAX_CTF_AMOUNT_USD) {
        return Err(anyhow!(
            "CTF amount must be between 0.000001 and ${MAX_CTF_AMOUNT_USD}"
        ));
    }
    let scaled = dec
        .checked_mul(Decimal::from(1_000_000_u64))
        .ok_or_else(|| anyhow!("CTF amount overflow during 6-decimal scaling"))?
        .trunc();
    let raw = scaled
        .to_u128()
        .ok_or_else(|| anyhow!("CTF amount out of range"))?;
    if raw == 0 {
        return Err(anyhow!(
            "CTF amount is smaller than 0.000001 pUSD minimum unit"
        ));
    }
    Ok(U256::from(raw))
}

fn shares_to_raw_decimal(shares: f64) -> Result<U256> {
    if !shares.is_finite() || shares < 0.0 {
        return Err(anyhow!("Share balance must be a non-negative finite number"));
    }
    if shares == 0.0 {
        return Ok(U256::ZERO);
    }
    let dec = Decimal::from_f64(shares)
        .ok_or_else(|| anyhow!("Share amount exceeds supported decimal range"))?;
    if dec > Decimal::from(MAX_CTF_AMOUNT_USD) {
        return Err(anyhow!(
            "Share amount exceeds max {MAX_CTF_AMOUNT_USD} units"
        ));
    }
    let scaled = dec
        .checked_mul(Decimal::from(1_000_000_u64))
        .ok_or_else(|| anyhow!("Share scaling overflow"))?
        .trunc();
    let raw = scaled
        .to_u128()
        .ok_or_else(|| anyhow!("Share amount out of range"))?;
    Ok(U256::from(raw))
}

fn format_usdc_raw(raw: U256) -> String {
    let denom = U256::from(1_000_000_u64);
    let whole = raw / denom;
    let frac = raw % denom;
    format!("{whole}.{frac:06}")
}

fn format_pol_wei(wei: U256) -> String {
    let whole = wei / U256::from(1_000_000_000_000_000_000_u128);
    let rem_milli = (wei / U256::from(1_000_000_000_000_000_u128)) % U256::from(1000_u64);
    format!("{whole}.{rem_milli:03}")
}

async fn verify_tx_receipt<P: Provider>(provider: &P, action: CtfAction, tx_hash: B256) -> Result<String> {
    let receipt = provider
        .get_transaction_receipt(tx_hash)
        .await
        .with_context(|| format!("Failed fetching Polygon receipt for CTF {action:?} (tx {tx_hash:#x})"))?
        .ok_or_else(|| anyhow!("Missing Polygon receipt for CTF {action:?} (tx {tx_hash:#x})"))?;
    if !receipt.status() {
        return Err(anyhow!("CTF {action:?} reverted on-chain (tx {tx_hash:#x})"));
    }
    Ok(format!("{tx_hash:#x}"))
}

pub async fn execute_ctf_live(
    rpc_url: &str,
    signer: PrivateKeySigner,
    sig_type: WalletSignatureType,
    req: &CtfRequest,
) -> Result<CtfTxResult> {
    ensure_eoa_for_direct_ctf(sig_type)?;
    let clean_rpc = validate_polygon_rpc_url(rpc_url)?;
    let redacted_rpc = redact_rpc_url(&clean_rpc);
    verify_polygon_rpc_dns(&clean_rpc).await?;

    let cond_id = B256::from_str(req.condition_id.trim())
        .context("Invalid market condition_id (expected 32-byte 0x hex)")?;
    let cfg = contract_config(POLYGON, req.neg_risk)
        .ok_or_else(|| anyhow!("Missing official Polymarket Polygon CTF contract configuration"))?;

    let signer = signer.with_chain_id(Some(POLYGON));
    let eoa_addr = signer.address();

    let provider = ProviderBuilder::new()
        .wallet(signer)
        .connect(&clean_rpc)
        .await
        .with_context(|| format!("Failed connecting to Polygon RPC {redacted_rpc}"))?;

    let pol_wei = provider
        .get_balance(eoa_addr)
        .await
        .with_context(|| format!("Failed querying POL gas balance on {redacted_rpc}"))?;
    if pol_wei < U256::from(MIN_EOA_POL_GAS_WEI) {
        return Err(anyhow!(
            "Insufficient POL on EOA for gas: have {} POL, need at least 0.010 POL on Polygon",
            format_pol_wei(pol_wei)
        ));
    }

    // Preflight collateral/token balances & spender approvals before broadcasting any tx
    let erc20 = IERC20::new(cfg.collateral, provider.clone());
    let erc1155 = IERC1155::new(cfg.conditional_tokens, provider.clone());
    let mut neg_risk_redeem_amounts: Option<(U256, U256)> = None;

    match (req.neg_risk, req.action) {
        (neg_risk, CtfAction::Split) => {
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let spender = if neg_risk {
                cfg.neg_risk_adapter.ok_or_else(|| {
                    anyhow!("Missing official Polymarket NegRisk adapter contract address")
                })?
            } else {
                cfg.conditional_tokens
            };
            let bal_call = erc20.balanceOf(eoa_addr);
            let allow_call = erc20.allowance(eoa_addr, spender);
            let (bal, allowance) = tokio::try_join!(bal_call.call(), allow_call.call())
                .map_err(|e| anyhow!("Preflight collateral check failed on {redacted_rpc}: {e}"))?;
            if bal < raw_amt {
                return Err(anyhow!(
                    "Insufficient pUSD/USDC.e balance for Split: have ${}, need ${}",
                    format_usdc_raw(bal),
                    format_usdc_raw(raw_amt)
                ));
            }
            if allowance < raw_amt {
                return Err(anyhow!(
                    "Insufficient pUSD/USDC.e allowance for spender {spender:#x}: approved ${}, need ${}. Approve this contract first before Splitting.",
                    format_usdc_raw(allowance),
                    format_usdc_raw(raw_amt)
                ));
            }
        }
        (neg_risk, CtfAction::Merge) => {
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let yes_id = U256::from_str(req.yes_asset_id.trim())
                .context("Invalid YES outcome ERC-1155 token ID")?;
            let no_id = U256::from_str(req.no_asset_id.trim())
                .context("Invalid NO outcome ERC-1155 token ID")?;
            let yes_call = erc1155.balanceOf(eoa_addr, yes_id);
            let no_call = erc1155.balanceOf(eoa_addr, no_id);
            let (yes_bal, no_bal) = tokio::try_join!(yes_call.call(), no_call.call())
                .map_err(|e| anyhow!("Preflight ERC-1155 balance check failed on {redacted_rpc}: {e}"))?;
            if yes_bal < raw_amt || no_bal < raw_amt {
                return Err(anyhow!(
                    "Insufficient outcome tokens for Merge: have {} YES / {} NO, need {} pairs",
                    format_usdc_raw(yes_bal),
                    format_usdc_raw(no_bal),
                    format_usdc_raw(raw_amt)
                ));
            }
            if neg_risk {
                let adapter_addr = cfg.neg_risk_adapter.ok_or_else(|| {
                    anyhow!("Missing official Polymarket NegRisk adapter contract address")
                })?;
                let approved = erc1155
                    .isApprovedForAll(eoa_addr, adapter_addr)
                    .call()
                    .await
                    .map_err(|e| {
                        anyhow!("Preflight NegRisk approval check failed on {redacted_rpc}: {e}")
                    })?;
                if !approved {
                    return Err(anyhow!(
                        "Conditional Tokens ERC-1155 setApprovalForAll not granted to NegRisk adapter {adapter_addr:#x}. Approve before merging."
                    ));
                }
            }
        }
        (true, CtfAction::Redeem) => {
            let adapter_addr = cfg.neg_risk_adapter.ok_or_else(|| {
                anyhow!("Missing official Polymarket NegRisk adapter contract address")
            })?;
            let yes_raw = shares_to_raw_decimal(req.yes_shares)?;
            let no_raw = shares_to_raw_decimal(req.no_shares)?;
            if yes_raw.is_zero() && no_raw.is_zero() {
                return Err(anyhow!(
                    "No redeemable shares recorded for this NegRisk condition"
                ));
            }
            if !yes_raw.is_zero() {
                let yes_id = U256::from_str(req.yes_asset_id.trim())
                    .context("Invalid YES outcome ERC-1155 token ID")?;
                let bal = erc1155
                    .balanceOf(eoa_addr, yes_id)
                    .call()
                    .await
                    .map_err(|e| anyhow!("Preflight ERC-1155 check failed on {redacted_rpc}: {e}"))?;
                if bal < yes_raw {
                    return Err(anyhow!(
                        "Insufficient on-chain YES shares for NegRisk Redeem: have {}, requested {}",
                        format_usdc_raw(bal),
                        format_usdc_raw(yes_raw)
                    ));
                }
            }
            if !no_raw.is_zero() {
                let no_id = U256::from_str(req.no_asset_id.trim())
                    .context("Invalid NO outcome ERC-1155 token ID")?;
                let bal = erc1155
                    .balanceOf(eoa_addr, no_id)
                    .call()
                    .await
                    .map_err(|e| anyhow!("Preflight ERC-1155 check failed on {redacted_rpc}: {e}"))?;
                if bal < no_raw {
                    return Err(anyhow!(
                        "Insufficient on-chain NO shares for NegRisk Redeem: have {}, requested {}",
                        format_usdc_raw(bal),
                        format_usdc_raw(no_raw)
                    ));
                }
            }
            let approved = erc1155
                .isApprovedForAll(eoa_addr, adapter_addr)
                .call()
                .await
                .map_err(|e| {
                    anyhow!("Preflight NegRisk approval check failed on {redacted_rpc}: {e}")
                })?;
            if !approved {
                return Err(anyhow!(
                    "Conditional Tokens ERC-1155 setApprovalForAll not granted to NegRisk adapter {adapter_addr:#x}. Approve before redeeming."
                ));
            }
            neg_risk_redeem_amounts = Some((yes_raw, no_raw));
        }
        (false, CtfAction::Redeem) => {
            if let (Ok(yes_id), Ok(no_id)) = (
                U256::from_str(req.yes_asset_id.trim()),
                U256::from_str(req.no_asset_id.trim()),
            ) {
                let yes_call = erc1155.balanceOf(eoa_addr, yes_id);
                let no_call = erc1155.balanceOf(eoa_addr, no_id);
                let (yes_bal, no_bal) = tokio::try_join!(yes_call.call(), no_call.call()).map_err(|e| {
                    anyhow!("Preflight ERC-1155 balance check failed on {redacted_rpc}: {e}")
                })?;
                if yes_bal.is_zero() && no_bal.is_zero() {
                    return Err(anyhow!(
                        "No on-chain ERC-1155 shares held to redeem for this condition"
                    ));
                }
            }
        }
    }

    // Real gas estimation (+20% buffer) before broadcasting
    let ctf_gas = IConditionalTokensGas::new(cfg.conditional_tokens, provider.clone());
    let binary_partition = vec![U256::from(1_u64), U256::from(2_u64)];
    let gas_est_res = match (req.neg_risk, req.action) {
        (true, CtfAction::Split) => {
            let adapter_addr = cfg.neg_risk_adapter.ok_or_else(|| {
                anyhow!("Missing official Polymarket NegRisk adapter contract address")
            })?;
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let adapter = INegRiskSplitMerge::new(adapter_addr, provider.clone());
            adapter.splitPosition(cond_id, raw_amt).from(eoa_addr).estimate_gas().await
        }
        (true, CtfAction::Merge) => {
            let adapter_addr = cfg.neg_risk_adapter.ok_or_else(|| {
                anyhow!("Missing official Polymarket NegRisk adapter contract address")
            })?;
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let adapter = INegRiskSplitMerge::new(adapter_addr, provider.clone());
            adapter.mergePositions(cond_id, raw_amt).from(eoa_addr).estimate_gas().await
        }
        (true, CtfAction::Redeem) => {
            let adapter_addr = cfg.neg_risk_adapter.ok_or_else(|| {
                anyhow!("Missing official Polymarket NegRisk adapter contract address")
            })?;
            let (yes_raw, no_raw) = neg_risk_redeem_amounts.unwrap_or((U256::ZERO, U256::ZERO));
            let adapter = INegRiskSplitMerge::new(adapter_addr, provider.clone());
            adapter
                .redeemPositions(cond_id, vec![yes_raw, no_raw])
                .from(eoa_addr)
                .estimate_gas()
                .await
        }
        (false, CtfAction::Split) => {
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            ctf_gas
                .splitPosition(cfg.collateral, B256::ZERO, cond_id, binary_partition, raw_amt)
                .from(eoa_addr)
                .estimate_gas()
                .await
        }
        (false, CtfAction::Merge) => {
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            ctf_gas
                .mergePositions(cfg.collateral, B256::ZERO, cond_id, binary_partition, raw_amt)
                .from(eoa_addr)
                .estimate_gas()
                .await
        }
        (false, CtfAction::Redeem) => {
            ctf_gas
                .redeemPositions(cfg.collateral, B256::ZERO, cond_id, binary_partition)
                .from(eoa_addr)
                .estimate_gas()
                .await
        }
    };

    match gas_est_res {
        Ok(est_gas) => {
            if let Ok(gas_price) = provider.get_gas_price().await {
                let buffered_wei = U256::from(est_gas)
                    .saturating_mul(U256::from(gas_price))
                    .saturating_mul(U256::from(120_u64))
                    / U256::from(100_u64);
                let required = buffered_wei.max(U256::from(MIN_EOA_POL_GAS_WEI));
                if pol_wei < required {
                    return Err(anyhow!(
                        "Insufficient POL for estimated CTF gas: have {} POL, estimated need >= {} POL",
                        format_pol_wei(pol_wei),
                        format_pol_wei(required)
                    ));
                }
            }
        }
        Err(e) => {
            let lower = e.to_string().to_ascii_lowercase();
            if lower.contains("revert") {
                return Err(anyhow!(
                    "CTF preflight gas estimation reverted (market unresolved, insufficient tokens/approval, or invalid condition): {e}"
                ));
            }
        }
    }

    let client = if req.neg_risk {
        CtfClient::with_neg_risk(provider.clone(), POLYGON)?
    } else {
        CtfClient::new(provider.clone(), POLYGON)?
    };

    let tx_hash_hex = match (req.neg_risk, req.action) {
        (true, CtfAction::Split) => {
            let adapter_addr = cfg.neg_risk_adapter.ok_or_else(|| {
                anyhow!("Missing official Polymarket NegRisk adapter contract address")
            })?;
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let adapter = INegRiskSplitMerge::new(adapter_addr, provider.clone());
            let pending_tx = adapter
                .splitPosition(cond_id, raw_amt)
                .send()
                .await
                .map_err(|e| anyhow!("Failed sending NegRisk split transaction: {e}"))?;
            let tx_hash = *pending_tx.tx_hash();
            let receipt = pending_tx
                .get_receipt()
                .await
                .map_err(|e| anyhow!("Failed confirming NegRisk split transaction: {e}"))?;
            if !receipt.status() {
                return Err(anyhow!(
                    "CTF {:?} reverted on-chain (tx {tx_hash:#x})",
                    req.action
                ));
            }
            format!("{tx_hash:#x}")
        }
        (true, CtfAction::Merge) => {
            let adapter_addr = cfg.neg_risk_adapter.ok_or_else(|| {
                anyhow!("Missing official Polymarket NegRisk adapter contract address")
            })?;
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let adapter = INegRiskSplitMerge::new(adapter_addr, provider.clone());
            let pending_tx = adapter
                .mergePositions(cond_id, raw_amt)
                .send()
                .await
                .map_err(|e| anyhow!("Failed sending NegRisk merge transaction: {e}"))?;
            let tx_hash = *pending_tx.tx_hash();
            let receipt = pending_tx
                .get_receipt()
                .await
                .map_err(|e| anyhow!("Failed confirming NegRisk merge transaction: {e}"))?;
            if !receipt.status() {
                return Err(anyhow!(
                    "CTF {:?} reverted on-chain (tx {tx_hash:#x})",
                    req.action
                ));
            }
            format!("{tx_hash:#x}")
        }
        (true, CtfAction::Redeem) => {
            let (yes_raw, no_raw) = neg_risk_redeem_amounts.unwrap_or((U256::ZERO, U256::ZERO));
            let redeem_req = RedeemNegRiskRequest::builder()
                .condition_id(cond_id)
                .amounts(vec![yes_raw, no_raw])
                .build();
            let resp = client.redeem_neg_risk(&redeem_req).await?;
            verify_tx_receipt(&provider, req.action, resp.transaction_hash).await?
        }
        (false, CtfAction::Split) => {
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let split_req =
                SplitPositionRequest::for_binary_market(cfg.collateral, cond_id, raw_amt);
            let resp = client.split_position(&split_req).await?;
            verify_tx_receipt(&provider, req.action, resp.transaction_hash).await?
        }
        (false, CtfAction::Merge) => {
            let raw_amt = usd_to_usdc_raw(req.amount)?;
            let merge_req =
                MergePositionsRequest::for_binary_market(cfg.collateral, cond_id, raw_amt);
            let resp = client.merge_positions(&merge_req).await?;
            verify_tx_receipt(&provider, req.action, resp.transaction_hash).await?
        }
        (false, CtfAction::Redeem) => {
            let redeem_req = RedeemPositionsRequest::for_binary_market(cfg.collateral, cond_id);
            let resp = client.redeem_positions(&redeem_req).await?;
            verify_tx_receipt(&provider, req.action, resp.transaction_hash).await?
        }
    };

    let scan_url = format!("https://polygonscan.com/tx/{tx_hash_hex}");
    Ok(CtfTxResult {
        action: req.action,
        mode_label: "LIVE".into(),
        tx_hash: Some(tx_hash_hex.clone()),
        polygonscan_url: Some(scan_url),
        summary: format!("CTF {:?} confirmed on Polygon: {tx_hash_hex}", req.action),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_ctf_permits_only_eoa_and_rejects_proxy_or_safe() {
        assert!(ensure_eoa_for_direct_ctf(WalletSignatureType::Eoa).is_ok());
        for non_eoa in [
            WalletSignatureType::Proxy,
            WalletSignatureType::GnosisSafe,
            WalletSignatureType::Poly1271,
        ] {
            let err = ensure_eoa_for_direct_ctf(non_eoa).unwrap_err().to_string();
            assert!(err.contains("EOA") && err.contains("polymarket.com/portfolio"));
        }
    }

    #[test]
    fn usd_to_usdc_raw_scales_six_decimals_and_rejects_invalid() {
        assert_eq!(usd_to_usdc_raw(12.5).unwrap(), U256::from(12_500_000_u128));
        assert_eq!(
            usd_to_usdc_raw(12.345678).unwrap(),
            U256::from(12_345_678_u128)
        );
        assert_eq!(usd_to_usdc_raw(0.1 + 0.2).unwrap(), U256::from(300_000_u128));
        assert_eq!(usd_to_usdc_raw(1.0).unwrap(), U256::from(1_000_000_u128));
        assert_eq!(
            usd_to_usdc_raw(10_000_000.0).unwrap(),
            U256::from(10_000_000_000_000_u128)
        );

        for bad in [
            0.0,
            -5.0,
            0.000_000_1,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX,
            1e30,
            10_000_000.01,
        ] {
            assert!(
                usd_to_usdc_raw(bad).is_err(),
                "expected rejection for CTF amount {bad:?}"
            );
        }
    }

    #[test]
    fn official_polygon_contract_configs_resolve_ctf_and_negrisk() {
        let std_cfg = contract_config(POLYGON, false).expect("standard polygon config");
        assert_ne!(std_cfg.conditional_tokens, alloy::primitives::Address::ZERO);
        assert_ne!(std_cfg.collateral, alloy::primitives::Address::ZERO);

        let nr_cfg = contract_config(POLYGON, true).expect("negrisk polygon config");
        assert!(nr_cfg.neg_risk_adapter.is_some());
        assert_ne!(
            nr_cfg.neg_risk_adapter.unwrap(),
            alloy::primitives::Address::ZERO
        );
    }
}
