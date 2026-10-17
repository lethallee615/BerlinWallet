use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use fedimint_client::db::ChronologicalOperationLogKey;
use fedimint_client::Client;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_eventlog::EventLogId;
use fedimint_walletv2_client::{
    FinalReceiveOperationState, ReceiveMeta, WalletClientModule, WalletOperationMeta,
};

use crate::bdk;
use crate::error::{classify_error, ErrorClass, PegError};
use crate::journal::{
    AdoptedClaim, ApplyResult, BindResult, DepositRow, DepositState, Journal,
};
use crate::ops::DepositId;

const SCAN_PAGE: usize = 256;
/// Re-scan this many operations older than the previous watermark to catch
/// outcomes that became visible late (#9338).
const SCAN_OVERLAP: usize = 32;
const SCAN_STATE_WAIT_PER_OP: Duration = Duration::from_millis(500);
const SCAN_TOTAL_STATE_WAIT: Duration = Duration::from_secs(2);
const SCAN_RETRY_ATTEMPTS: u32 = 5;

/// Create (or return) the single open deposit request for a federation.
///
/// The background address task is started by the caller; this function only
/// persists `AwaitingAddress` and returns immediately.
pub async fn request_deposit(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
    amount: bitcoin::Amount,
) -> anyhow::Result<DepositRow> {
    if let Some(existing) = journal.open_deposit(&federation_id).await? {
        return Ok(existing);
    }

    let position = client.get_next_event_log_id().await;
    let row = DepositRow::requested(DepositId::new(), federation_id, position, amount);
    journal.insert_deposit(&row).await
}

/// Background address task: bound `receive()` at 120 s, retry, then write
/// `AddressShown` **conditionally**. If `cancel_deposit` won the race, this
/// writes nothing and the caller must not spawn a watcher.
pub async fn run_address_task(
    journal: &Journal,
    module: &WalletClientModule,
    id: &DepositId,
) -> anyhow::Result<ApplyResult> {
    const MAX_ADDRESS_ATTEMPTS: u32 = 5;
    let mut attempts = 0u32;

    let address = loop {
        match tokio::time::timeout(Duration::from_secs(120), module.receive()).await {
            Ok(address) => break address,
            Err(_elapsed) => {
                attempts += 1;
                if attempts >= MAX_ADDRESS_ATTEMPTS {
                    let _ = journal
                        .fail_deposit_if(
                            id,
                            DepositState::AwaitingAddress,
                            "receive() timed out",
                        )
                        .await?;
                    return Ok(ApplyResult::Skipped);
                }
            }
        }
    };

    journal
        .set_deposit_address_if(
            id,
            address.into_unchecked(),
            DepositState::AwaitingAddress,
        )
        .await
}

/// Idempotently fund a deposit from the BDK wallet.
///
/// Quote -> show net -> build/sign/broadcast through the configured Esplora
/// URL. The journal's `funding_txid` is only written after a successful
/// broadcast, so a failed broadcast can be retried without a second journal
/// write.
pub async fn fund_deposit(
    journal: &Journal,
    module: &WalletClientModule,
    wallet: &mut bdk::BdkWallet,
    id: &DepositId,
) -> anyhow::Result<bitcoin::Txid> {
    if let Some(txid) = journal.deposit_funding_txid(id).await? {
        return Ok(txid);
    }

    let row = journal
        .deposit(id)
        .await?
        .ok_or_else(|| PegError::DepositNotFound(id.0.clone()))?;

    let quoted_fee = module.receive_fee().await?;
    journal.set_deposit_quoted_fee(id, quoted_fee).await?;

    let address = row.address.as_deref().ok_or(PegError::AddressNotReady)?;
    let destination: bitcoin::Address<bitcoin::address::NetworkUnchecked> = address.parse()?;
    let destination = destination.assume_checked();

    let tx = wallet.build_and_finalize(&destination, row.amount).await?;
    let txid = tx.compute_txid();
    journal.set_deposit_funding_txid(id, txid).await?;
    Ok(txid)
}

/// Observe a deposit to completion. `Aborted` is handled **inside** the loop
/// and advances the cursor; Success owns `next_position` after the loop.
pub async fn watch_deposit(
    client: &Client,
    journal: &Journal,
    module: &WalletClientModule,
    id: &DepositId,
    federation_id: FederationId,
    mut position: EventLogId,
) -> anyhow::Result<()> {
    const DEPOSIT_WATCH_TIMEOUT: Duration = Duration::from_secs(600);
    let mut backoff = Duration::from_secs(1);
    let next_position;

    loop {
        match tokio::time::timeout(
            DEPOSIT_WATCH_TIMEOUT,
            module.await_receive(position.clone()),
        )
        .await
        {
            Ok(Ok((FinalReceiveOperationState::Success, next))) => {
                next_position = next;
                break;
            }
            Ok(Ok((FinalReceiveOperationState::Aborted, next))) => {
                journal.set_deposit_aborted(id, next.clone()).await?;
                position = next;
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
            Ok(Err(err)) => match classify_error(&err) {
                ErrorClass::Transient => {
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
                ErrorClass::Permanent => {
                    journal.fail_deposit(id, &err.to_string()).await?;
                    return Ok(());
                }
            },
            Err(_elapsed) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }

    // Success only: `set_deposit_aborted` already persisted the cursor on Aborted.
    journal.set_deposit_next_position(id, next_position).await?;

    // `await_receive` returns no operation id/meta: reuse the operation-log
    // scan and match the claim by scriptPubKey.
    let row = journal
        .deposit(id)
        .await?
        .ok_or_else(|| PegError::DepositNotFound(id.0.clone()))?;

    // The claim can lag `await_receive` returning Success. Retry the scan
    // with backoff; if it is still absent, leave the row AddressShown and let
    // the adoption pass bind it (R then takes the TakenBy path).
    let mut scan_backoff = Duration::from_secs(1);
    let mut selected = None;
    for _ in 0..SCAN_RETRY_ATTEMPTS {
        let claims = scan_receive_claims(client, journal, federation_id).await?;
        if let Some(claim) = select_claim_for_success(claims, row.address_script.as_deref()) {
            selected = Some(claim);
            break;
        }
        tokio::time::sleep(scan_backoff).await;
        scan_backoff = (scan_backoff * 2).min(Duration::from_secs(30));
    }

    let Some(claim) = selected else {
        // Not visible to the scan yet: do not fail the row. It stays
        // AddressShown; the adoption pass binds the claim later.
        return Ok(());
    };
    let outpoint = claim.outpoint.expect("checked above");

    match journal.claim_bind(&federation_id, outpoint, id).await? {
        BindResult::Bound => {
            journal
                .finish_deposit(id, outpoint, claim.value, claim.fee)
                .await?;
        }
        BindResult::TakenBy(_other) => {
            journal.close_deposit_done(id).await?;
        }
    }
    Ok(())
}

/// Boot/periodic reconciliation: adopt every successful claim that no row
/// owns, and close an open owner when adoption loses the bind.
pub async fn adopt_orphan_claims(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
) -> anyhow::Result<()> {
    for claim in scan_receive_claims(client, journal, federation_id).await? {
        if claim.final_state != FinalReceiveOperationState::Success {
            continue;
        }
        let Some(outpoint) = claim.outpoint else {
            continue;
        };

        match journal
            .claim_adopt_if_absent(
                &federation_id,
                outpoint,
                AdoptedClaim {
                    address: claim.address.clone(),
                    address_script: claim.address_script.clone(),
                    operation_id: claim.operation_id,
                    value: claim.value,
                    fee: claim.fee,
                },
            )
            .await?
        {
            BindResult::Bound => {}
            BindResult::TakenBy(row) => {
                journal
                    .close_deposit_done_if_open(&row, claim.value, claim.fee)
                    .await?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct OrphanClaim {
    pub final_state: FinalReceiveOperationState,
    pub operation_id: OperationId,
    pub outpoint: Option<bitcoin::OutPoint>,
    pub address: Option<String>,
    pub address_script: Option<Vec<u8>>,
    pub value: bitcoin::Amount,
    pub fee: bitcoin::Amount,
}

#[derive(Debug, Clone)]
struct RawClaim {
    operation_id: OperationId,
    final_state: Option<FinalReceiveOperationState>,
    outpoint: Option<bitcoin::OutPoint>,
    value: bitcoin::Amount,
    fee: bitcoin::Amount,
    address: Option<String>,
    address_script: Option<Vec<u8>>,
}

fn filter_orphan_claims(
    raw: Vec<RawClaim>,
    is_bound: impl Fn(&bitcoin::OutPoint) -> bool,
) -> Vec<OrphanClaim> {
    let mut seen = BTreeSet::new();
    raw.into_iter()
        .filter_map(|claim| {
            if claim.final_state != Some(FinalReceiveOperationState::Success) {
                return None;
            }
            let outpoint = claim.outpoint?;
            if !seen.insert(outpoint) || is_bound(&outpoint) {
                return None;
            }
            Some(OrphanClaim {
                final_state: FinalReceiveOperationState::Success,
                operation_id: claim.operation_id,
                outpoint: Some(outpoint),
                address: claim.address,
                address_script: claim.address_script,
                value: claim.value,
                fee: claim.fee,
            })
        })
        .collect()
}

fn is_older_than(
    key: &ChronologicalOperationLogKey,
    watermark: &ChronologicalOperationLogKey,
) -> bool {
    key.creation_time < watermark.creation_time
        || (key.creation_time == watermark.creation_time
            && key.operation_id < watermark.operation_id)
}

/// Advance the watermark only when the pass reached its terminal condition
/// (the watermark+overlap boundary or the end of the log) and did not exhaust
/// the state-wait budget. A truncated or errored pass leaves it unchanged so
/// the next pass re-covers the gap.
fn should_advance_watermark(reached_terminal: bool, state_wait_exhausted: bool) -> bool {
    reached_terminal && !state_wait_exhausted
}

fn select_claim_for_success(
    claims: Vec<OrphanClaim>,
    address_script: Option<&[u8]>,
) -> Option<OrphanClaim> {
    claims.into_iter().find(|claim| {
        claim.final_state == FinalReceiveOperationState::Success
            && claim.outpoint.is_some()
            && match (claim.address_script.as_deref(), address_script) {
                (Some(claim_script), Some(row_script)) => claim_script == row_script,
                // Unknown on either side: accept the unbound claim.
                _ => true,
            }
    })
}

/// Scan the operation log for walletv2 `Receive` claims whose final state is
/// `Success` and which no deposit row has bound yet.
///
/// - Operations are paged with `OperationLog::paginate_operations_rev`.
/// - The scan resumes from the persisted watermark with a bounded overlap.
/// - Non-wallet operations are skipped by `operation_module_kind()` before
///   `try_meta`; the expected walletv2 kind string is `"walletv2"`.
/// - Meta is `WalletOperationMeta::Receive(ReceiveMeta { outpoint, value, fee,
///   address, .. })`.
/// - The final state is `OperationLogEntry::try_outcome::<FinalReceiveOperationState>()`;
///   if the outcome is not cached yet, a bounded
///   `WalletClientModule::await_final_receive_operation_state` check is used,
///   with a total per-scan wait budget.
/// - `operation_id` is `ChronologicalOperationLogKey::operation_id` (the same
///   id carried by `ReceivePaymentEvent`).
pub async fn scan_receive_claims(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
) -> anyhow::Result<Vec<OrphanClaim>> {
    let log = client.operation_log();
    let module = client.get_first_module::<WalletClientModule>()?;
    let watermark = journal.scan_watermark(&federation_id).await?;

    let mut overlap_budget = watermark.map(|_| SCAN_OVERLAP);
    let mut newest_key = watermark;
    let mut last_seen = None;
    let mut state_wait = Duration::ZERO;
    let mut raw = Vec::new();
    let reached_terminal: bool;
    let mut state_wait_exhausted = false;

    'scan: loop {
        let page = log.paginate_operations_rev(SCAN_PAGE, last_seen).await;
        if page.is_empty() {
            reached_terminal = true;
            break;
        }

        for (key, entry) in &page {
            // Advance the watermark only with a key newer than the previous
            // watermark; an idle scan must not move it backwards.
            if newest_key == watermark
                && watermark.is_none_or(|watermark| !is_older_than(key, &watermark))
            {
                newest_key = Some(*key);
            }

            if let Some(watermark) = watermark {
                if is_older_than(key, &watermark) {
                    match overlap_budget {
                        Some(0) => {
                            reached_terminal = true;
                            break 'scan;
                        }
                        Some(n) => overlap_budget = Some(n - 1),
                        None => {}
                    }
                }
            }

            // Skip non-wallet operations BEFORE deserializing their meta.
            if entry.operation_module_kind() != fedimint_walletv2_common::KIND.as_str() {
                continue;
            }

            let Ok(meta) = entry.try_meta::<WalletOperationMeta>() else {
                continue;
            };
            let WalletOperationMeta::Receive(ReceiveMeta {
                outpoint,
                value,
                fee,
                address,
                ..
            }) = meta
            else {
                continue;
            };
            let Some(outpoint) = outpoint else {
                continue;
            };

            let final_state = match entry.try_outcome::<FinalReceiveOperationState>() {
                Ok(Some(state)) => Some(state),
                _ => {
                    if state_wait >= SCAN_TOTAL_STATE_WAIT {
                        state_wait_exhausted = true;
                        None
                    } else {
                        let budget =
                            (SCAN_TOTAL_STATE_WAIT - state_wait).min(SCAN_STATE_WAIT_PER_OP);
                        let started = Instant::now();
                        let state = match tokio::time::timeout(
                            budget,
                            module.await_final_receive_operation_state(key.operation_id),
                        )
                        .await
                        {
                            Ok(Ok(state)) => Some(state),
                            _ => None,
                        };
                        state_wait += started.elapsed();
                        state
                    }
                }
            };

            let (address, address_script) = match address {
                Some(address) => {
                    let checked = address.assume_checked();
                    (
                        Some(checked.to_string()),
                        Some(checked.script_pubkey().as_bytes().to_vec()),
                    )
                }
                None => (None, None),
            };

            raw.push(RawClaim {
                operation_id: key.operation_id,
                final_state,
                outpoint: Some(outpoint),
                value,
                fee,
                address,
                address_script,
            });
        }

        last_seen = page.last().map(|(key, _)| *key);
        if page.len() < SCAN_PAGE {
            reached_terminal = true;
            break;
        }
    }

    if should_advance_watermark(reached_terminal, state_wait_exhausted) {
        if let Some(newest) = newest_key {
            journal.set_scan_watermark(&federation_id, newest).await?;
        }
    }

    let bound = journal.bound_outpoints(&federation_id).await?;
    Ok(filter_orphan_claims(raw, |outpoint| bound.contains(outpoint)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::hashes::Hash as _;

    fn outpoint(vout: u32) -> bitcoin::OutPoint {
        bitcoin::OutPoint {
            txid: bitcoin::Txid::from_byte_array([0u8; 32]),
            vout,
        }
    }

    fn success(operation_id: OperationId, outpoint: bitcoin::OutPoint) -> RawClaim {
        RawClaim {
            operation_id,
            final_state: Some(FinalReceiveOperationState::Success),
            outpoint: Some(outpoint),
            value: bitcoin::Amount::from_sat(10_000),
            fee: bitcoin::Amount::from_sat(100),
            address: None,
            address_script: Some(vec![1, 2, 3]),
        }
    }

    #[test]
    fn filters_successful_unbound_unique_claims() {
        let a = outpoint(0);
        let b = outpoint(1);
        let c = outpoint(2);
        let d = outpoint(3);

        let op_a = OperationId::new_random();
        let op_aborted = OperationId::new_random();
        let op_bound = OperationId::new_random();
        let op_pending = OperationId::new_random();

        let mut aborted = success(op_aborted, b);
        aborted.final_state = Some(FinalReceiveOperationState::Aborted);

        let mut pending = success(op_pending, d);
        pending.final_state = None;

        let raw = vec![
            success(op_a, a),
            aborted,
            success(op_bound, c),
            pending,
            success(op_a, a), // duplicate outpoint; must be de-duplicated
        ];

        let bound: BTreeSet<bitcoin::OutPoint> = [c].into_iter().collect();
        let orphans = filter_orphan_claims(raw, |outpoint| bound.contains(outpoint));

        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].operation_id, op_a);
        assert_eq!(orphans[0].outpoint, Some(a));
        assert_eq!(orphans[0].value, bitcoin::Amount::from_sat(10_000));
        assert_eq!(orphans[0].fee, bitcoin::Amount::from_sat(100));
        assert_eq!(orphans[0].address_script, Some(vec![1, 2, 3]));
    }

    fn orphan_with_script(script: Vec<u8>) -> OrphanClaim {
        OrphanClaim {
            final_state: FinalReceiveOperationState::Success,
            operation_id: OperationId::new_random(),
            outpoint: Some(outpoint(0)),
            address: None,
            address_script: Some(script),
            value: bitcoin::Amount::from_sat(1),
            fee: bitcoin::Amount::ZERO,
        }
    }

    #[test]
    fn success_without_a_visible_matching_claim_defers_to_adoption() {
        // Nothing visible yet: the watcher must leave R AddressShown and let
        // the adoption pass bind the claim.
        assert!(select_claim_for_success(Vec::new(), Some(&[1, 2, 3])).is_none());

        // A claim with a different script does not match.
        assert!(
            select_claim_for_success(vec![orphan_with_script(vec![9, 9])], Some(&[1, 2, 3]))
                .is_none()
        );

        // The matching claim is selected.
        assert!(
            select_claim_for_success(vec![orphan_with_script(vec![1, 2, 3])], Some(&[1, 2, 3]))
                .is_some()
        );
    }

    #[test]
    fn truncated_pass_does_not_advance_watermark_and_next_pass_finds_claim() {
        // A pass that stopped before its boundary (or exhausted the state-wait
        // budget) must not advance the watermark.
        assert!(!should_advance_watermark(false, false));
        assert!(!should_advance_watermark(false, true));
        assert!(!should_advance_watermark(true, true));
        assert!(should_advance_watermark(true, false));

        // First (truncated) pass: the claim is beyond the truncation point,
        // so it is not seen. The watermark is unchanged; the next pass
        // re-covers the gap and finds it.
        let op = OperationId::new_random();
        let o = outpoint(0);
        assert!(filter_orphan_claims(Vec::new(), |_| false).is_empty());

        let second_pass = filter_orphan_claims(vec![success(op, o)], |_| false);
        assert_eq!(second_pass.len(), 1);
        assert_eq!(second_pass[0].operation_id, op);
    }
}
