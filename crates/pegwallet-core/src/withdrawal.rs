use std::time::Duration;

use bdk_wallet::Wallet;
use fedimint_client::module::transaction::FeeQuote;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_walletv2_client::{FinalSendOperationState, WalletClientModule};

use crate::bdk;
use crate::error::{classify_error, ErrorClass, PegError};
use crate::journal::{Journal, WithdrawalRow};
use crate::ops::WithdrawalId;

/// Reveal a BDK destination and persist a `Quoting` withdrawal row.
pub async fn request_withdrawal(
    journal: &Journal,
    wallet: &mut Wallet,
    federation_id: FederationId,
    value: bitcoin::Amount,
) -> anyhow::Result<WithdrawalRow> {
    if let Some(existing) = journal.open_withdrawal(&federation_id).await? {
        anyhow::bail!(
            "an in-flight withdrawal already exists: {}",
            existing.id.0
        );
    }

    let address = bdk::reveal_destination(wallet);
    let row = WithdrawalRow::new(
        WithdrawalId::new(),
        federation_id,
        address.to_string(),
        value,
    );
    journal.insert_withdrawal(&row).await?;
    Ok(row)
}

/// Quote, re-quote and drift-check, submit once, then await the final state.
/// Both `SendError` and the final-await error are classified before any row
/// is left behind.
pub async fn send_withdrawal(
    journal: &Journal,
    module: &WalletClientModule,
    id: &WithdrawalId,
) -> anyhow::Result<()> {
    let row = journal
        .withdrawal(id)
        .await?
        .ok_or_else(|| PegError::WithdrawalNotFound(id.0.clone()))?;
    let value = row.value;

    let chain_fee = module.send_fee().await?;
    let quote: FeeQuote = module.send_fee_quote(value).await?;
    journal.set_withdrawal_quote(id, (&quote).into()).await?;

    // Re-quote immediately before send(); abort on drift (#9098).
    let fresh_fee = module.send_fee().await?;
    let fresh_quote: FeeQuote = module.send_fee_quote(value).await?;
    if fresh_fee != chain_fee || fresh_quote != quote {
        return Err(PegError::QuoteChanged {
            quoted: quote,
            current: fresh_quote,
        }
        .into());
    }

    let destination: bitcoin::Address<bitcoin::address::NetworkUnchecked> =
        row.address.parse()?;
    let custom_meta = serde_json::json!({ "id": id.0 });

    let operation_id = match module
        .send(destination, value, Some(fresh_fee), custom_meta)
        .await
    {
        Ok(operation_id) => {
            journal
                .set_withdrawal_operation_id(id, operation_id)
                .await?;
            operation_id
        }
        Err(err) => {
            if classify_error(&err) == ErrorClass::Permanent {
                journal
                    .set_withdrawal_failed(id, &err.to_string())
                    .await?;
            }
            return Err(err.into());
        }
    };

    match module
        .await_final_send_operation_state(operation_id)
        .await
    {
        Ok(FinalSendOperationState::Success(txid)) => {
            journal.set_broadcast(id, txid).await?;
        }
        Ok(FinalSendOperationState::Aborted) => {
            journal.set_refunded(id).await?;
        }
        Ok(FinalSendOperationState::Failure) => {
            journal.set_failed(id).await?;
        }
        Err(err) => {
            if classify_error(&err) == ErrorClass::Permanent {
                journal
                    .set_withdrawal_failed(id, &err.to_string())
                    .await?;
            }
            return Err(err);
        }
    }

    Ok(())
}

/// Bounded reconciliation of the `send()`-returned / `operation_id`-not-yet-
/// persisted crash window. Only `Ok(None)` from a clean scan permits a
/// re-send; an inconclusive scan never does.
pub async fn reconcile_before_resend(
    journal: &Journal,
    id: &WithdrawalId,
) -> anyhow::Result<Option<OperationId>> {
    const MAX_SCAN_ATTEMPTS: u32 = 5;
    let mut backoff = Duration::from_secs(1);

    for _ in 0..MAX_SCAN_ATTEMPTS {
        match journal.scan_for_send(id).await {
            Ok(Some(operation_id)) => return Ok(Some(operation_id)),
            Ok(None) => return Ok(None),
            Err(_err) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
        }
    }

    Err(PegError::InconclusiveScan.into())
}
