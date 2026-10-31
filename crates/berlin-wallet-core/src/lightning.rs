//! Lightning (`lnv2`) send/receive orchestration.
//!
//! The API is source-verified in `docs/API-VERIFICATION.md` §48. This module
//! keeps the same discipline as deposits and withdrawals: a journal row with an
//! idempotency key before the operation, the operation id persisted after
//! submission, final-state updates from the lnv2 state machine, and adoption
//! from the operation log on restart. No invoice is ever re-sent.

use std::str::FromStr;
use std::time::Duration;

use fedimint_client::Client;
use fedimint_client::module::transaction::FeeQuote;
use fedimint_core::Amount;
use fedimint_core::config::FederationId;
use fedimint_core::module::AmountUnit;
use fedimint_core::util::SafeUrl;
use fedimint_lnv2_client::common::Bolt11InvoiceDescription;
use fedimint_lnv2_client::common::gateway_api::PaymentFee;
use fedimint_lnv2_client::{
    FinalReceiveOperationState, FinalSendOperationState, InvoiceSendStatus, LightningClientModule,
    LightningOperationMeta, SendPaymentError,
};
use lightning_invoice::Bolt11Invoice;
use serde_json::{Value, json};

use crate::journal::{Journal, LightningKind, LightningOrigin, LightningRow, LightningState};
use crate::ops::LightningId;
use crate::wire::FeeQuoteWire;

/// Bound for the CLI's inline wait on a send. The background adoption tick
/// resumes a row that outlives it.
const SEND_FINAL_TIMEOUT: Duration = Duration::from_secs(300);
/// Bound for one row update in the periodic adoption pass.
const TICK_FINAL_TIMEOUT: Duration = Duration::from_secs(2);
const OPERATION_SCAN_PAGE: usize = 256;
pub const MAX_INVOICE_EXPIRY_SECS: u32 = 60 * 60 * 24;

#[derive(Debug, Clone)]
pub struct LightningSendQuote {
    pub invoice: Bolt11Invoice,
    pub amount_msat: u64,
    pub gateway: SafeUrl,
    pub gateway_fee_msat: u64,
    pub federation_fee_msat: u64,
    pub federation_quote: FeeQuote,
    pub total_debit_msat: u64,
}

#[derive(Debug, Clone)]
pub struct LightningReceiveQuote {
    pub amount_msat: u64,
    pub gateway: SafeUrl,
    pub gateway_fee_msat: u64,
    pub federation_fee_msat: u64,
    pub federation_quote: FeeQuote,
    pub net_msat: u64,
    pub expiry_secs: u32,
}

/// Parse a caller-supplied gateway URL.
pub fn parse_gateway(value: &str) -> anyhow::Result<SafeUrl> {
    SafeUrl::from_str(value).map_err(|err| anyhow::anyhow!("invalid gateway URL: {err}"))
}

/// List the federation's vetted lnv2 gateways.
pub async fn list_gateways(client: &Client) -> anyhow::Result<Vec<SafeUrl>> {
    let module = client.get_first_module::<LightningClientModule>()?;
    Ok(module.list_gateways(None).await?)
}

/// Total BTC-denominated fee in a [`FeeQuote`].
pub fn fee_quote_total_msat(quote: &FeeQuote) -> u64 {
    fn btc(amounts: &fedimint_core::module::Amounts) -> u64 {
        amounts
            .get(&AmountUnit::BITCOIN)
            .copied()
            .map(|amount| amount.msats)
            .unwrap_or(0)
    }
    btc(&quote.input)
        .saturating_add(btc(&quote.output))
        .saturating_add(btc(&quote.dust))
}

fn invoice_amount_msat(invoice: &Bolt11Invoice) -> anyhow::Result<u64> {
    invoice
        .amount_milli_satoshis()
        .ok_or_else(|| anyhow::anyhow!("invoice has no amount"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn description(value: String) -> Bolt11InvoiceDescription {
    Bolt11InvoiceDescription::Direct(value)
}

async fn quote_send(
    module: &LightningClientModule,
    invoice: &Bolt11Invoice,
    gateway: Option<SafeUrl>,
) -> anyhow::Result<(LightningSendQuote, SafeUrl)> {
    let amount_msat = invoice_amount_msat(invoice)?;
    let (gateway, routing) = match gateway {
        Some(gateway) => {
            let routing = module
                .routing_info(&gateway)
                .await?
                .ok_or_else(|| anyhow::anyhow!("gateway does not support this federation"))?;
            (gateway, routing)
        }
        None => module.select_gateway(Some(invoice.clone())).await?,
    };
    let (send_fee, _expiration_delta) = routing.send_parameters(invoice);
    anyhow::ensure!(
        send_fee.is_within(&PaymentFee::SEND_FEE_LIMIT),
        "gateway send fee exceeds the client limit"
    );
    let gateway_fee_msat = send_fee.fee(amount_msat).msats;
    let contract_amount = send_fee.add_to(amount_msat);
    let federation_quote = module.send_fee_quote(contract_amount).await?;
    let federation_fee_msat = fee_quote_total_msat(&federation_quote);
    let quote = LightningSendQuote {
        invoice: invoice.clone(),
        amount_msat,
        gateway: gateway.clone(),
        gateway_fee_msat,
        federation_fee_msat,
        federation_quote,
        total_debit_msat: amount_msat
            .saturating_add(gateway_fee_msat)
            .saturating_add(federation_fee_msat),
    };
    Ok((quote, gateway))
}

/// Persist a Pending send row and return the exact pre-send quote.
///
/// The row is written before `send`; its id is the `custom_meta` idempotency
/// key. If the invoice already has an operation, this refuses to re-send.
pub async fn prepare_send(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
    invoice: &str,
    gateway: Option<SafeUrl>,
) -> anyhow::Result<(LightningRow, LightningSendQuote)> {
    let invoice = Bolt11Invoice::from_str(invoice)
        .map_err(|err| anyhow::anyhow!("invalid Bolt11 invoice: {err}"))?;
    let module = client.get_first_module::<LightningClientModule>()?;
    let status = module.get_invoice_send_status(&invoice).await?;
    let payment_hash = invoice.payment_hash().to_string();

    if !matches!(status, InvoiceSendStatus::NotAttempted) {
        if let Some(row) = journal.lightning_all().await?.into_iter().find(|row| {
            row.kind == LightningKind::Send
                && row.federation_id == federation_id
                && row.payment_hash.as_deref() == Some(payment_hash.as_str())
        }) {
            anyhow::bail!(
                "invoice already has row {} in state {:?} ({status:?}); not re-sending",
                row.id.0,
                row.state
            );
        }
        anyhow::bail!("invoice already attempted ({status:?}); not re-sending");
    }

    // A crash between the row insert and send() leaves a Pending row with the
    // same invoice. Reuse it instead of creating a duplicate.
    if let Some(mut row) = journal.lightning_all().await?.into_iter().find(|row| {
        row.kind == LightningKind::Send
            && row.federation_id == federation_id
            && row.payment_hash.as_deref() == Some(payment_hash.as_str())
            && row.state == LightningState::Pending
    }) {
        let (quote, gateway) = quote_send(&module, &invoice, gateway).await?;
        row.gateway = Some(gateway.to_string());
        row.gateway_fee_msat = Some(quote.gateway_fee_msat);
        row.federation_quote = Some(FeeQuoteWire::from(&quote.federation_quote));
        row.touch();
        journal.update_lightning(&row).await?;
        return Ok((row, quote));
    }

    let (quote, gateway) = quote_send(&module, &invoice, gateway).await?;
    let mut row = LightningRow::requested(
        LightningId::new(),
        federation_id,
        LightningKind::Send,
        quote.amount_msat,
    );
    row.invoice = Some(invoice.to_string());
    row.payment_hash = Some(payment_hash);
    row.gateway = Some(gateway.to_string());
    row.gateway_fee_msat = Some(quote.gateway_fee_msat);
    row.federation_quote = Some(FeeQuoteWire::from(&quote.federation_quote));
    journal.insert_lightning(&row).await?;
    Ok((row, quote))
}

/// Submit the persisted send and wait (bounded) for its final state.
pub async fn execute_send(
    client: &Client,
    journal: &Journal,
    id: &LightningId,
) -> anyhow::Result<LightningRow> {
    let module = client.get_first_module::<LightningClientModule>()?;
    let mut row = journal
        .lightning(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("lightning row not found: {}", id.0))?;
    if row.state != LightningState::Pending {
        return Ok(row);
    }

    if row.operation_id.is_none() {
        let invoice: Bolt11Invoice = row
            .invoice
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("send row has no invoice"))?
            .parse()
            .map_err(|err| anyhow::anyhow!("invalid stored invoice: {err}"))?;
        let gateway: SafeUrl = row
            .gateway
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("send row has no gateway"))?
            .parse()
            .map_err(|err| anyhow::anyhow!("invalid stored gateway: {err}"))?;
        let custom_meta = json!({ "id": row.id.0 });

        match module.send(invoice, Some(gateway), custom_meta).await {
            Ok(operation_id) => {
                row.operation_id = Some(operation_id);
                row.touch();
                journal.update_lightning(&row).await?;
            }
            Err(SendPaymentError::DuplicatePaymentAttempt(operation_id)) => {
                // Another process created the operation between the status
                // check and this call; adopt it instead of failing.
                row.operation_id = Some(operation_id);
                row.touch();
                journal.update_lightning(&row).await?;
            }
            Err(err) => {
                row.state = LightningState::Failed;
                row.last_error = Some(err.to_string());
                row.touch();
                journal.update_lightning(&row).await?;
                return Err(err.into());
            }
        }
    }

    update_send_state(client, journal, id, SEND_FINAL_TIMEOUT).await
}

async fn update_send_state(
    client: &Client,
    journal: &Journal,
    id: &LightningId,
    timeout: Duration,
) -> anyhow::Result<LightningRow> {
    let mut row = journal
        .lightning(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("lightning row not found: {}", id.0))?;
    let Some(operation_id) = row.operation_id else {
        return Ok(row);
    };
    let module = client.get_first_module::<LightningClientModule>()?;
    let state = match tokio::time::timeout(
        timeout,
        module.await_final_send_operation_state(operation_id),
    )
    .await
    {
        Ok(Ok(state)) => state,
        Ok(Err(err)) => {
            row.last_error = Some(err.to_string());
            row.touch();
            journal.update_lightning(&row).await?;
            return Ok(row);
        }
        Err(_) => return Ok(row),
    };
    match state {
        FinalSendOperationState::Success(preimage) => {
            row.state = LightningState::Succeeded;
            row.preimage = Some(hex(&preimage));
        }
        FinalSendOperationState::Refunded => row.state = LightningState::Refunded,
        FinalSendOperationState::Failure => row.state = LightningState::Failed,
    }
    row.touch();
    journal.update_lightning(&row).await?;
    Ok(row)
}

async fn update_receive_state(
    client: &Client,
    journal: &Journal,
    id: &LightningId,
    timeout: Duration,
) -> anyhow::Result<LightningRow> {
    let mut row = journal
        .lightning(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("lightning row not found: {}", id.0))?;
    let Some(operation_id) = row.operation_id else {
        return Ok(row);
    };
    let module = client.get_first_module::<LightningClientModule>()?;
    let state = match tokio::time::timeout(
        timeout,
        module.await_final_receive_operation_state(operation_id),
    )
    .await
    {
        Ok(Ok(state)) => Some(state),
        Ok(Err(err)) => {
            row.last_error = Some(err.to_string());
            None
        }
        Err(_) => None,
    };
    if let Some(state) = state {
        row.state = match state {
            FinalReceiveOperationState::Claimed => LightningState::Claimed,
            FinalReceiveOperationState::Expired => LightningState::Expired,
            FinalReceiveOperationState::Failure => LightningState::Failed,
        };
    } else if invoice_is_expired(&row) {
        // The upstream receive stream can lag the invoice deadline. The
        // invoice itself is the authority for "no payment can arrive now".
        row.state = LightningState::Expired;
    }
    row.touch();
    journal.update_lightning(&row).await?;
    Ok(row)
}

fn invoice_is_expired(row: &LightningRow) -> bool {
    row.invoice
        .as_deref()
        .and_then(|invoice| Bolt11Invoice::from_str(invoice).ok())
        .is_some_and(|invoice| invoice.is_expired())
}

/// Create an lnv2 invoice, persist a Pending receive row, and return the
/// invoice plus its pre-creation quote. Payment is watched by the adoption
/// task.
pub async fn request_receive(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
    amount: Amount,
    expiry_secs: u32,
    description_text: String,
    gateway: Option<SafeUrl>,
) -> anyhow::Result<(LightningRow, LightningReceiveQuote)> {
    anyhow::ensure!(
        expiry_secs > 0 && expiry_secs <= MAX_INVOICE_EXPIRY_SECS,
        "invoice expiry must be between 1 and {MAX_INVOICE_EXPIRY_SECS} seconds"
    );
    let module = client.get_first_module::<LightningClientModule>()?;
    let (gateway, routing) = match gateway {
        Some(gateway) => {
            let routing = module
                .routing_info(&gateway)
                .await?
                .ok_or_else(|| anyhow::anyhow!("gateway does not support this federation"))?;
            (gateway, routing)
        }
        None => module.select_gateway(None).await?,
    };
    anyhow::ensure!(
        routing
            .receive_fee
            .is_within(&PaymentFee::RECEIVE_FEE_LIMIT),
        "gateway receive fee exceeds the client limit"
    );
    let gateway_fee_msat = routing.receive_fee.fee(amount.msats).msats;
    let federation_quote = module.receive_fee_quote(amount).await?;
    let federation_fee_msat = fee_quote_total_msat(&federation_quote);
    let quote = LightningReceiveQuote {
        amount_msat: amount.msats,
        gateway: gateway.clone(),
        gateway_fee_msat,
        federation_fee_msat,
        federation_quote,
        net_msat: amount
            .msats
            .saturating_sub(gateway_fee_msat)
            .saturating_sub(federation_fee_msat),
        expiry_secs,
    };

    let mut row = LightningRow::requested(
        LightningId::new(),
        federation_id,
        LightningKind::Receive,
        amount.msats,
    );
    row.gateway = Some(gateway.to_string());
    row.gateway_fee_msat = Some(gateway_fee_msat);
    row.federation_quote = Some(FeeQuoteWire::from(&quote.federation_quote));
    row.expiry_secs = Some(expiry_secs);
    journal.insert_lightning(&row).await?;

    let custom_meta = json!({ "id": row.id.0 });
    match module
        .receive(
            amount,
            expiry_secs,
            description(description_text),
            Some(gateway),
            custom_meta,
        )
        .await
    {
        Ok((invoice, operation_id)) => {
            row.invoice = Some(invoice.to_string());
            row.payment_hash = Some(invoice.payment_hash().to_string());
            row.operation_id = Some(operation_id);
            row.touch();
            journal.update_lightning(&row).await?;
            Ok((row, quote))
        }
        Err(err) => {
            row.state = LightningState::Failed;
            row.last_error = Some(err.to_string());
            row.touch();
            journal.update_lightning(&row).await?;
            Err(err.into())
        }
    }
}

/// Refresh one Pending row if it already has an operation id.
pub async fn refresh_row(
    client: &Client,
    journal: &Journal,
    id: &LightningId,
) -> anyhow::Result<LightningRow> {
    let row = journal
        .lightning(id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("lightning row not found: {}", id.0))?;
    if row.state != LightningState::Pending || row.operation_id.is_none() {
        return Ok(row);
    }
    match row.kind {
        LightningKind::Send => update_send_state(client, journal, id, TICK_FINAL_TIMEOUT).await,
        LightningKind::Receive => {
            update_receive_state(client, journal, id, TICK_FINAL_TIMEOUT).await
        }
    }
}

/// Update every Pending row that already has an operation id. Each wait is
/// bounded so the adoption task cannot stall on an unpaid invoice.
pub async fn refresh_pending(client: &Client, journal: &Journal) -> anyhow::Result<()> {
    for row in journal.lightning_pending().await? {
        if row.operation_id.is_none() {
            continue;
        }
        let result = match row.kind {
            LightningKind::Send => {
                update_send_state(client, journal, &row.id, TICK_FINAL_TIMEOUT).await
            }
            LightningKind::Receive => {
                update_receive_state(client, journal, &row.id, TICK_FINAL_TIMEOUT).await
            }
        };
        if let Err(err) = result {
            tracing::warn!(lightning = %row.id.0, ?err, "lightning refresh failed");
        }
    }
    Ok(())
}

/// Adopt lnv2 operations from the local operation log that have no row, and
/// refresh rows whose operation id was not persisted before a crash.
///
/// The row id travels in `custom_meta.id`; the operation id is the fallback
/// key. This never calls `send` or `receive`.
pub async fn adopt_lightning_operations(
    client: &Client,
    journal: &Journal,
    federation_id: FederationId,
) -> anyhow::Result<()> {
    let log = client.operation_log();
    let mut last_seen = None;
    loop {
        let page = log
            .paginate_operations_rev(OPERATION_SCAN_PAGE, last_seen)
            .await;
        if page.is_empty() {
            break;
        }
        for (key, entry) in &page {
            if entry.operation_module_kind() != fedimint_lnv2_common::KIND.as_str() {
                continue;
            }
            let Ok(meta) = entry.try_meta::<LightningOperationMeta>() else {
                continue;
            };
            let (kind, invoice, gateway, gateway_fee, custom_meta) = match &meta {
                LightningOperationMeta::Send(send) => (
                    LightningKind::Send,
                    &send.invoice,
                    &send.gateway,
                    send.gateway_fee(),
                    &send.custom_meta,
                ),
                LightningOperationMeta::Receive(receive) => (
                    LightningKind::Receive,
                    &receive.invoice,
                    &receive.gateway,
                    receive.gateway_fee(),
                    &receive.custom_meta,
                ),
                LightningOperationMeta::LnurlReceive(_) => continue,
            };
            let fedimint_lnv2_common::LightningInvoice::Bolt11(invoice) = invoice;
            let amount_msat = invoice.amount_milli_satoshis().unwrap_or(0);
            let row_id = custom_meta
                .get("id")
                .and_then(Value::as_str)
                .map(|id| LightningId(id.to_owned()))
                .unwrap_or_else(LightningId::new);
            let mut row = match journal.lightning(&row_id).await? {
                Some(row) if row.federation_id == federation_id => row,
                Some(_) => continue,
                None => {
                    let mut row = LightningRow::requested(row_id, federation_id, kind, amount_msat);
                    row.origin = LightningOrigin::Adopted;
                    row
                }
            };
            row.operation_id = Some(key.operation_id);
            row.invoice = Some(invoice.to_string());
            row.payment_hash = Some(invoice.payment_hash().to_string());
            row.amount_msat = amount_msat;
            row.gateway = Some(gateway.to_string());
            row.gateway_fee_msat = Some(gateway_fee.msats);
            match kind {
                LightningKind::Send => {
                    if let Ok(Some(state)) = entry.try_outcome::<FinalSendOperationState>() {
                        match state {
                            FinalSendOperationState::Success(preimage) => {
                                row.state = LightningState::Succeeded;
                                row.preimage = Some(hex(&preimage));
                            }
                            FinalSendOperationState::Refunded => {
                                row.state = LightningState::Refunded;
                            }
                            FinalSendOperationState::Failure => {
                                row.state = LightningState::Failed;
                            }
                        }
                    }
                }
                LightningKind::Receive => {
                    if let Ok(Some(state)) = entry.try_outcome::<FinalReceiveOperationState>() {
                        row.state = match state {
                            FinalReceiveOperationState::Claimed => LightningState::Claimed,
                            FinalReceiveOperationState::Expired => LightningState::Expired,
                            FinalReceiveOperationState::Failure => LightningState::Failed,
                        };
                    }
                }
            }
            row.touch();
            journal.update_lightning(&row).await?;
        }
        last_seen = page.last().map(|(key, _)| *key);
        if page.len() < OPERATION_SCAN_PAGE {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fedimint_core::module::Amounts;

    #[test]
    fn gateway_urls_are_parsed_and_rejected() {
        assert!(parse_gateway("http://127.0.0.1:1234/").is_ok());
        assert!(parse_gateway("not a url").is_err());
    }

    #[test]
    fn fee_quote_total_adds_input_output_and_dust() {
        let quote = FeeQuote {
            input: Amounts::new_bitcoin_msats(1_000),
            output: Amounts::new_bitcoin_msats(200),
            dust: Amounts::new_bitcoin_msats(30),
        };
        assert_eq!(fee_quote_total_msat(&quote), 1_230);
    }
}
