use std::time::Duration;

use fedimint_client::Client;
use fedimint_client::module::transaction::FeeQuote;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_walletv2_client::{
    FinalSendOperationState, SendMeta, WalletClientModule, WalletOperationMeta,
};

use crate::client::wallet_client_config;
use crate::error::{ErrorClass, PegError, classify_error};
use crate::journal::{Journal, WithdrawalRow, WithdrawalState};
use crate::ops::WithdrawalId;

/// Persist a `Quoting` withdrawal row for a caller-supplied destination.
///
/// The destination comes from the user/UI, not from the BDK wallet: the BDK
/// wallet is the peg-in funding source, while a withdrawal pays out to an
/// external on-chain address.
pub async fn request_withdrawal(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
    address: String,
    value: bitcoin::Amount,
) -> anyhow::Result<WithdrawalRow> {
    if let Some(existing) = journal.open_withdrawal(&federation_id).await? {
        anyhow::bail!("an in-flight withdrawal already exists: {}", existing.id.0);
    }

    let _: bitcoin::Address<bitcoin::address::NetworkUnchecked> = address.parse()?;
    let row = WithdrawalRow::new(WithdrawalId::new(), federation_id, address, value);
    journal.insert_withdrawal(&row).await?;

    // Quote while the row is still Quoting so the UI can show the on-chain fee
    // and net debit and require explicit confirmation before send_withdrawal.
    let module = client.get_first_module::<WalletClientModule>()?;
    let chain_fee = module.send_fee().await?;
    let quote = module.send_fee_quote(value).await?;
    journal
        .set_withdrawal_quote(&row.id, chain_fee, (&quote).into())
        .await?;
    journal
        .withdrawal(&row.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("withdrawal row disappeared after quote"))
}

/// Reject a withdrawal that the federation would reject as dust before any
/// fee quote is requested. `WalletClientModule::send` performs the same check
/// against its decoded `WalletClientConfig`, but only after the quote path.
pub fn validate_withdrawal_value(
    value: bitcoin::Amount,
    dust_limit: bitcoin::Amount,
) -> anyhow::Result<()> {
    if value < dust_limit {
        return Err(PegError::WithdrawalBelowDust { value, dust_limit }.into());
    }
    Ok(())
}

const RECONCILE_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for waiting on contiguous primary-module change outputs. The send is
/// already submitted; on timeout the row stays `Submitted` and reconciliation
/// retries instead of blocking forever.
const CHANGE_OUTPUT_TIMEOUT: Duration = Duration::from_secs(20);
const SEND_SCAN_PAGE: usize = 256;

/// One walletv2 Send operation discovered in the operation log.
#[derive(Debug, Clone)]
pub struct AdoptedSend {
    pub operation_id: OperationId,
    pub final_state: Option<FinalSendOperationState>,
    pub address: String,
    pub value: bitcoin::Amount,
    pub fee: bitcoin::Amount,
    pub custom_meta: serde_json::Value,
}

/// Scan the local operation log for every walletv2 Send operation.
///
/// This intentionally does not use the deposit scan watermark: adoption must
/// see the full log on a restored journal, and dedup is enforced by the
/// journal's `operation_id` index.
pub async fn scan_send_operations(client: &Client) -> anyhow::Result<Vec<AdoptedSend>> {
    let log = client.operation_log();
    let mut last_seen = None;
    let mut sends = Vec::new();

    loop {
        let page = log.paginate_operations_rev(SEND_SCAN_PAGE, last_seen).await;
        if page.is_empty() {
            break;
        }
        for (key, entry) in &page {
            if entry.operation_module_kind() != fedimint_walletv2_common::KIND.as_str() {
                continue;
            }
            let Ok(meta) = entry.try_meta::<WalletOperationMeta>() else {
                continue;
            };
            let WalletOperationMeta::Send(send) = meta else {
                continue;
            };
            let final_state = entry
                .try_outcome::<FinalSendOperationState>()
                .ok()
                .flatten();
            sends.push(AdoptedSend {
                operation_id: key.operation_id,
                final_state,
                address: send.address.assume_checked().to_string(),
                value: send.value,
                fee: send.fee,
                custom_meta: send.custom_meta,
            });
        }
        if page.len() < SEND_SCAN_PAGE {
            break;
        }
        last_seen = page.last().map(|(key, _)| *key);
    }

    Ok(sends)
}

/// Adopt Send operations that have no local withdrawal row.
///
/// Invariant: every walletv2 Send operation in the log is represented by
/// exactly one `withdrawal_requests` row, enforced by the journal's unique
/// operation-id index. Runs at boot and on the adoption tick.
pub async fn adopt_orphan_sends(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
) -> anyhow::Result<()> {
    let sends = scan_send_operations(client).await?;
    adopt_send_claims(journal, federation_id, sends).await
}

/// Pure adoption step, separated from the operation-log scan for testing.
pub(crate) async fn adopt_send_claims(
    journal: &Journal,
    federation_id: FederationId,
    sends: Vec<AdoptedSend>,
) -> anyhow::Result<()> {
    for send in sends {
        let (state, txid) = match send.final_state {
            Some(FinalSendOperationState::Success(txid)) => {
                (WithdrawalState::Broadcast { txid }, Some(txid))
            }
            Some(FinalSendOperationState::Aborted) => (WithdrawalState::Refunded, None),
            Some(FinalSendOperationState::Failure) => (WithdrawalState::Failed, None),
            None => (WithdrawalState::Submitted, None),
        };
        let mut id = send
            .custom_meta
            .get("id")
            .and_then(|value| value.as_str())
            .map(|id| WithdrawalId(id.to_owned()))
            .unwrap_or_else(WithdrawalId::new);
        // A custom_meta id must not clobber a different local withdrawal.
        if let Some(existing) = journal.withdrawal(&id).await? {
            if existing.operation_id.is_some() && existing.operation_id != Some(send.operation_id) {
                id = WithdrawalId::new();
            }
        }
        let row = WithdrawalRow::adopted(
            id,
            federation_id,
            send.address,
            send.value,
            send.fee,
            send.operation_id,
            state,
            txid,
            send.custom_meta.to_string(),
        );
        if let Some(existing) = journal.insert_adopted_withdrawal(&row).await? {
            tracing::debug!(
                operation_id = %send.operation_id.fmt_full(),
                withdrawal = %existing.id.0,
                "Send operation already represented by a withdrawal row"
            );
        }
    }
    Ok(())
}

/// Reconcile a submitted withdrawal from the federation's final state.
///
/// The authoritative txid comes from
/// `await_final_send_operation_state(operation_id)`, not from a BDK script
/// scan: the caller-supplied withdrawal address is normally external, so it is
/// not in our BDK wallet's index.
pub async fn reconcile_withdrawal(
    client: &Client,
    journal: &Journal,
    id: &WithdrawalId,
) -> anyhow::Result<()> {
    let row = journal
        .withdrawal(id)
        .await?
        .ok_or_else(|| PegError::WithdrawalNotFound(id.0.clone()))?;
    match row.state {
        WithdrawalState::Submitted => {
            let Some(operation_id) = row.operation_id else {
                return Ok(());
            };
            tokio::time::timeout(
                RECONCILE_TIMEOUT,
                await_final_state(client, journal, id, operation_id),
            )
            .await
            .map_err(|_| anyhow::anyhow!("withdrawal final-state await timed out"))??;
        }
        WithdrawalState::Quoting => {
            if let Some(operation_id) = find_sent_operation(client, journal, id).await? {
                journal
                    .set_withdrawal_operation_id(id, operation_id)
                    .await?;
                tokio::time::timeout(
                    RECONCILE_TIMEOUT,
                    await_final_state(client, journal, id, operation_id),
                )
                .await
                .map_err(|_| anyhow::anyhow!("withdrawal final-state await timed out"))??;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Find the operation id of a send that returned before it was persisted.
///
/// Checks the journal first, then the walletv2 operation log by our
/// `custom_meta.id`. This closes the `send()` -> `set_withdrawal_operation_id`
/// crash window without re-sending.
pub async fn find_sent_operation(
    client: &Client,
    journal: &Journal,
    id: &WithdrawalId,
) -> anyhow::Result<Option<OperationId>> {
    if let Some(operation_id) = journal.scan_for_send(id).await? {
        return Ok(Some(operation_id));
    }

    let log = client.operation_log();
    let mut last_seen = None;
    loop {
        let page = log.paginate_operations_rev(256, last_seen).await;
        if page.is_empty() {
            return Ok(None);
        }
        for (key, entry) in &page {
            if entry.operation_module_kind() != fedimint_walletv2_common::KIND.as_str() {
                continue;
            }
            let Ok(meta) = entry.try_meta::<WalletOperationMeta>() else {
                continue;
            };
            if let WalletOperationMeta::Send(SendMeta { custom_meta, .. }) = meta {
                if custom_meta.get("id").and_then(|value| value.as_str()) == Some(id.0.as_str()) {
                    return Ok(Some(key.operation_id));
                }
            }
        }
        last_seen = page.last().map(|(key, _)| *key);
    }
}

/// Wait for the primary module change outputs of a successful send before
/// declaring Broadcast. Without this, a crash immediately after the send final
/// state can leave the change notes uncredited.
///
/// Returns `false` if the bounded wait expires; the caller must leave the
/// withdrawal `Submitted` so reconciliation can retry.
pub async fn await_change_outputs(
    client: &Client,
    operation_id: OperationId,
) -> anyhow::Result<bool> {
    let Some(entry) = client.operation_log().get_operation(operation_id).await else {
        return Ok(true);
    };
    let Ok(WalletOperationMeta::Send(SendMeta {
        change_outpoint_range,
        ..
    })) = entry.try_meta::<WalletOperationMeta>()
    else {
        return Ok(true);
    };

    let wait = async {
        for outpoint in change_outpoint_range {
            client
                .await_primary_bitcoin_module_output(operation_id, outpoint)
                .await?;
        }
        Ok::<(), anyhow::Error>(())
    };

    match tokio::time::timeout(CHANGE_OUTPUT_TIMEOUT, wait).await {
        Ok(result) => {
            result?;
            Ok(true)
        }
        Err(_) => {
            tracing::warn!(
                ?operation_id,
                "timed out waiting for change outputs; leaving withdrawal Submitted"
            );
            Ok(false)
        }
    }
}

async fn await_final_state(
    client: &Client,
    journal: &Journal,
    id: &WithdrawalId,
    operation_id: OperationId,
) -> anyhow::Result<()> {
    let module = client.get_first_module::<WalletClientModule>()?;
    match module.await_final_send_operation_state(operation_id).await {
        Ok(FinalSendOperationState::Success(txid)) => {
            if await_change_outputs(client, operation_id).await? {
                crate::fault::crash_point("after_broadcast");
                journal.set_broadcast(id, txid).await?;
            }
            // On timeout the row remains `Submitted`; `reconcile_submitted`
            // retries the bounded change-output wait.
        }
        Ok(FinalSendOperationState::Aborted) => {
            journal.set_refunded(id).await?;
        }
        Ok(FinalSendOperationState::Failure) => {
            journal.set_failed(id).await?;
        }
        Err(err) => {
            if classify_error(&err) == ErrorClass::Permanent {
                journal.set_withdrawal_failed(id, &err.to_string()).await?;
            }
            return Err(err);
        }
    }
    Ok(())
}

/// Boot/periodic pass over `Submitted` withdrawal rows. Uses the federation
/// txid; no BDK scan is attempted for external destinations.
pub async fn reconcile_submitted(client: &Client, journal: &Journal) -> anyhow::Result<()> {
    let rows = journal
        .withdrawals_in(&[WithdrawalState::Quoting, WithdrawalState::Submitted])
        .await?;
    for row in rows {
        if let Err(err) = reconcile_withdrawal(client, journal, &row.id).await {
            tracing::warn!(
                ?err,
                withdrawal = %row.id.0,
                "withdrawal reconciliation failed; will retry"
            );
        }
    }
    Ok(())
}

/// Quote, re-quote and drift-check, submit once, then await the final state.
/// Both `SendError` and the final-await error are classified before any row
/// is left behind.
pub async fn send_withdrawal(
    client: &Client,
    journal: &Journal,
    id: &WithdrawalId,
) -> anyhow::Result<()> {
    let module = client.get_first_module::<WalletClientModule>()?;
    let row = journal
        .withdrawal(id)
        .await?
        .ok_or_else(|| PegError::WithdrawalNotFound(id.0.clone()))?;
    let value = row.value;

    // Fail before the quote/send path: a dust value is permanent, so release
    // the single-flight slot and surface a typed error.
    let dust_limit = wallet_client_config(client).await?.dust_limit;
    if let Err(err) = validate_withdrawal_value(value, dust_limit) {
        if let Err(journal_err) = journal.set_withdrawal_failed(id, &err.to_string()).await {
            tracing::warn!(?journal_err, "failed to persist dust rejection");
        }
        return Err(err);
    }

    // A crash may have happened after send() but before the operation id was
    // persisted, or after the final state arrived but before Broadcast was
    // written. Recover from the operation log/journal instead of re-sending.
    if row.state == WithdrawalState::Submitted {
        if let Some(operation_id) = row.operation_id {
            journal
                .set_withdrawal_operation_id(id, operation_id)
                .await?;
            return await_final_state(client, journal, id, operation_id).await;
        }
        return Ok(());
    }
    if row.state != WithdrawalState::Quoting {
        return Ok(());
    }
    if let Some(operation_id) = find_sent_operation(client, journal, id).await? {
        journal
            .set_withdrawal_operation_id(id, operation_id)
            .await?;
        return await_final_state(client, journal, id, operation_id).await;
    }

    // Use the quote persisted by request_withdrawal (the UI showed it), or
    // establish one for a row created directly. Re-quote immediately before
    // send() and abort on drift. On drift persist the new quote so the UI can
    // display it and ask for confirmation again.
    let (expected_fee, expected_quote) = match (row.fee, row.quote.clone()) {
        (Some(fee), Some(quote)) => (fee, FeeQuote::from(quote)),
        _ => {
            let fee = module.send_fee().await?;
            let quote = module.send_fee_quote(value).await?;
            journal
                .set_withdrawal_quote(id, fee, (&quote).into())
                .await?;
            (fee, quote)
        }
    };
    let fresh_fee = module.send_fee().await?;
    let fresh_quote = module.send_fee_quote(value).await?;
    if fresh_fee != expected_fee || fresh_quote != expected_quote {
        journal
            .set_withdrawal_quote(id, fresh_fee, (&fresh_quote).into())
            .await?;
        return Err(PegError::QuoteChanged {
            quoted: expected_quote,
            current: fresh_quote,
        }
        .into());
    }

    let destination: bitcoin::Address<bitcoin::address::NetworkUnchecked> = row.address.parse()?;
    let custom_meta = serde_json::json!({ "id": id.0 });

    let operation_id = match module
        .send(destination, value, Some(fresh_fee), custom_meta)
        .await
    {
        Ok(operation_id) => {
            crate::fault::crash_point("after_send");
            journal
                .set_withdrawal_operation_id(id, operation_id)
                .await?;
            operation_id
        }
        Err(err) => {
            if classify_error(&err) == ErrorClass::Permanent {
                journal.set_withdrawal_failed(id, &err.to_string()).await?;
            }
            return Err(err.into());
        }
    };

    await_final_state(client, journal, id, operation_id).await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completed_send_is_adopted_exactly_once() {
        use bitcoin::hashes::Hash as _;

        let journal = Journal::in_memory();
        let federation_id = FederationId::dummy();
        let operation_id = OperationId::new_random();
        let txid = bitcoin::Txid::from_byte_array([0x77; 32]);
        let id = WithdrawalId("adopted-send".to_owned());
        let send = AdoptedSend {
            operation_id,
            final_state: Some(FinalSendOperationState::Success(txid)),
            address: "tb1qadopted".to_owned(),
            value: bitcoin::Amount::from_sat(10_000),
            fee: bitcoin::Amount::from_sat(234),
            custom_meta: serde_json::json!({ "id": id.0 }),
        };

        adopt_send_claims(&journal, federation_id, vec![send.clone()])
            .await
            .expect("adopt");
        let rows = journal
            .withdrawals_in(&[WithdrawalState::Broadcast { txid }])
            .await
            .expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].origin, crate::journal::WithdrawalOrigin::Adopted);
        assert_eq!(rows[0].operation_id, Some(operation_id));
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].fee, Some(bitcoin::Amount::from_sat(234)));
        assert_eq!(rows[0].txid, Some(txid));

        // Re-scan: same operation, deliberately different custom_meta id. The
        // operation-id index must still deduplicate it.
        let mut duplicate = send.clone();
        duplicate.custom_meta = serde_json::json!({ "id": "different-id" });
        adopt_send_claims(&journal, federation_id, vec![duplicate])
            .await
            .expect("re-adopt");
        let rows = journal
            .withdrawals_in(&[WithdrawalState::Broadcast { txid }])
            .await
            .expect("rows");
        assert_eq!(rows.len(), 1, "Send op must be represented exactly once");
        assert_eq!(rows[0].id, id);
    }

    #[tokio::test]
    async fn adopted_send_binds_an_existing_quoting_row() {
        use bitcoin::hashes::Hash as _;

        let journal = Journal::in_memory();
        let federation_id = FederationId::dummy();
        let id = WithdrawalId("requested-before-crash".to_owned());
        let op = OperationId::new_random();
        let txid = bitcoin::Txid::from_byte_array([0x88; 32]);
        journal
            .insert_withdrawal(&WithdrawalRow::new(
                id.clone(),
                federation_id,
                "tb1qrequested".to_owned(),
                bitcoin::Amount::from_sat(10_000),
            ))
            .await
            .expect("insert requested row");

        adopt_send_claims(
            &journal,
            federation_id,
            vec![AdoptedSend {
                operation_id: op,
                final_state: Some(FinalSendOperationState::Success(txid)),
                address: "tb1qrequested".to_owned(),
                value: bitcoin::Amount::from_sat(10_000),
                fee: bitcoin::Amount::from_sat(234),
                custom_meta: serde_json::json!({ "id": id.0 }),
            }],
        )
        .await
        .expect("bind");

        let row = journal.withdrawal(&id).await.unwrap().expect("row");
        assert_eq!(row.origin, crate::journal::WithdrawalOrigin::Requested);
        assert_eq!(row.operation_id, Some(op));
        assert_eq!(row.state, WithdrawalState::Broadcast { txid });
        assert_eq!(
            journal.withdrawals_all().await.expect("all").len(),
            1,
            "binding must not create a duplicate row"
        );
    }

    #[test]
    fn dust_value_is_rejected_before_send() {
        let dust_limit = bitcoin::Amount::from_sat(10_000);
        let value = bitcoin::Amount::from_sat(9_999);

        let err = validate_withdrawal_value(value, dust_limit).expect_err("below dust");
        let typed = err
            .downcast_ref::<PegError>()
            .expect("must be a typed PegError");
        assert!(matches!(
            typed,
            PegError::WithdrawalBelowDust {
                value: rejected,
                dust_limit: limit
            } if *rejected == value && *limit == dust_limit
        ));

        assert!(validate_withdrawal_value(dust_limit, dust_limit).is_ok());
    }

    #[test]
    fn change_wait_timeout_leaves_reconciliation_headroom() {
        assert!(
            CHANGE_OUTPUT_TIMEOUT < RECONCILE_TIMEOUT,
            "the inner change wait must expire before the outer reconcile timeout"
        );
    }

    #[test]
    fn wallet_config_defaults_match_source() {
        use std::collections::BTreeMap;

        use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey};
        use fedimint_core::PeerId;
        use fedimint_walletv2_common::config::{FeeConsensus, WalletConfigConsensus};

        let secp = Secp256k1::new();
        let mut bitcoin_pks = BTreeMap::new();
        for peer in 0..4u16 {
            let mut secret = [0u8; 32];
            secret[0] = peer as u8 + 1;
            let secret_key = SecretKey::from_slice(&secret).expect("valid secret key");
            bitcoin_pks.insert(
                PeerId::from(peer),
                PublicKey::from_secret_key(&secp, &secret_key),
            );
        }

        let fee_consensus = FeeConsensus::new(10_000).expect("1% is the maximum");
        let config =
            WalletConfigConsensus::new(bitcoin_pks, fee_consensus, bitcoin::Network::Bitcoin);

        assert_eq!(config.dust_limit, bitcoin::Amount::from_sat(10_000));
        assert_eq!(
            config.fee_consensus.base,
            fedimint_core::Amount::from_sats(100)
        );
        assert_eq!(config.fee_consensus.parts_per_million, 10_000);
        assert_eq!(config.send_tx_vbytes, 228);
        assert_eq!(config.receive_tx_vbytes, 316);
        assert_eq!(config.feerate_base, 250);
    }
}
