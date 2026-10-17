use std::collections::BTreeSet;
use std::time::Duration;

use bdk_wallet::signer::SignersContainer;
use bdk_wallet::Wallet;
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
    journal.insert_deposit(&row).await?;
    Ok(row)
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
/// Quote -> show net -> fund: `receive_fee()` is read before `build_tx`.
/// No network I/O is performed by this scaffold; it returns the finalized
/// transaction id for the caller's chain source.
pub async fn fund_deposit(
    journal: &Journal,
    module: &WalletClientModule,
    wallet: &mut Wallet,
    id: &DepositId,
    signers: &[&SignersContainer],
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

    let tx = bdk::build_and_finalize(wallet, &destination, row.amount, signers)?;
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
    let claim = scan_receive_claims(client, journal, federation_id)
        .await?
        .into_iter()
        .find(|claim| {
            claim.final_state == FinalReceiveOperationState::Success
                && claim.outpoint.is_some()
                && (claim.address_script.is_none()
                    || row.address_script.is_none()
                    || claim.address_script.as_deref() == row.address_script.as_deref())
        })
        .ok_or_else(|| anyhow::anyhow!("no unbound claim found after Success"))?;
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

/// Scan the operation log for walletv2 `Receive` claims whose final state is
/// `Success` and which no deposit row has bound yet.
///
/// - Operations are paged with `OperationLog::paginate_operations_rev`.
/// - Meta is `WalletOperationMeta::Receive(ReceiveMeta { outpoint, value, fee,
///   address, .. })`.
/// - The final state is `OperationLogEntry::try_outcome::<FinalReceiveOperationState>()`;
///   if the outcome is not cached yet, a bounded 500 ms
///   `WalletClientModule::await_final_receive_operation_state` check is used.
/// - `operation_id` is `ChronologicalOperationLogKey::operation_id` (the same
///   id carried by `ReceivePaymentEvent`).
pub async fn scan_receive_claims(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
) -> anyhow::Result<Vec<OrphanClaim>> {
    const PAGE: usize = 256;
    let log = client.operation_log();
    let module = client.get_first_module::<WalletClientModule>()?;

    let mut last_seen = None;
    let mut raw = Vec::new();

    loop {
        let page = log.paginate_operations_rev(PAGE, last_seen).await;
        if page.is_empty() {
            break;
        }

        for (key, entry) in &page {
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
                _ => match tokio::time::timeout(
                    Duration::from_millis(500),
                    module.await_final_receive_operation_state(key.operation_id),
                )
                .await
                {
                    Ok(Ok(state)) => Some(state),
                    _ => None,
                },
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
        if page.len() < PAGE {
            break;
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
}
