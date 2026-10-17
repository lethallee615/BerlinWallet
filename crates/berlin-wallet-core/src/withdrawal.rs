use std::time::Duration;

use fedimint_client::Client;
use fedimint_client::module::transaction::FeeQuote;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_walletv2_client::{FinalSendOperationState, WalletClientModule};

use crate::client::wallet_client_config;
use crate::error::{ErrorClass, PegError, classify_error};
use crate::journal::{Journal, WithdrawalRow};
use crate::ops::WithdrawalId;

/// Persist a `Quoting` withdrawal row for a caller-supplied destination.
///
/// The destination comes from the user/UI, not from the BDK wallet: the BDK
/// wallet is the peg-in funding source, while a withdrawal pays out to an
/// external on-chain address.
pub async fn request_withdrawal(
    journal: &Journal,
    federation_id: FederationId,
    address: String,
    value: bitcoin::Amount,
) -> anyhow::Result<WithdrawalRow> {
    if let Some(existing) = journal.open_withdrawal(&federation_id).await? {
        anyhow::bail!(
            "an in-flight withdrawal already exists: {}",
            existing.id.0
        );
    }

    let _: bitcoin::Address<bitcoin::address::NetworkUnchecked> = address.parse()?;
    let row = WithdrawalRow::new(WithdrawalId::new(), federation_id, address, value);
    journal.insert_withdrawal(&row).await?;
    Ok(row)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(config.fee_consensus.base, fedimint_core::Amount::from_sats(100));
        assert_eq!(config.fee_consensus.parts_per_million, 10_000);
        assert_eq!(config.send_tx_vbytes, 228);
        assert_eq!(config.receive_tx_vbytes, 316);
        assert_eq!(config.feerate_base, 250);
    }
}
