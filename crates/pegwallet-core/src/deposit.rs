use std::time::Duration;

use bdk_wallet::signer::SignersContainer;
use bdk_wallet::Wallet;
use fedimint_client::Client;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_eventlog::EventLogId;
use fedimint_walletv2_client::{FinalReceiveOperationState, WalletClientModule};

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

    let figures = claim_figures(client, position).await?;
    match journal
        .claim_bind(&federation_id, figures.outpoint, id)
        .await?
    {
        BindResult::Bound => {
            journal
                .finish_deposit(id, figures.outpoint, figures.gross, figures.fee)
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
    for claim in scan_receive_claims(client).await? {
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

struct ClaimFigures {
    outpoint: bitcoin::OutPoint,
    gross: bitcoin::Amount,
    fee: bitcoin::Amount,
    #[allow(dead_code)]
    operation_id: OperationId,
}

/// `await_receive` returns only `(state, cursor)`, not the claim's operation
/// id or figures. The real implementation reads `ReceivePaymentEvent` from
/// the event log to get the operation id, then `WalletOperationMeta::Receive`
/// for `ReceiveMeta { outpoint, value, fee, .. }`.
async fn claim_figures(
    _client: &Client,
    _position: EventLogId,
) -> anyhow::Result<ClaimFigures> {
    anyhow::bail!(
        "claim_figures: event-log/ReceiveMeta lookup is not implemented in the scaffold"
    )
}

struct OrphanClaim {
    final_state: FinalReceiveOperationState,
    operation_id: OperationId,
    outpoint: Option<bitcoin::OutPoint>,
    address: Option<String>,
    value: bitcoin::Amount,
    fee: bitcoin::Amount,
}

/// Scan the operation log for walletv2 `Receive` claims. Scaffold stub; the
/// real implementation pages the operation log and reads each `ReceiveMeta`.
async fn scan_receive_claims(_client: &Client) -> anyhow::Result<Vec<OrphanClaim>> {
    anyhow::bail!("scan_receive_claims: operation-log scan is not implemented in the scaffold")
}
