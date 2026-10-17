use std::collections::BTreeSet;

use async_trait::async_trait;
use bitcoin::hashes::Hash as _;
use fedimint_client::db::ChronologicalOperationLogKey;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::db::{Committable, Database, DatabaseTransaction, IDatabaseTransactionOpsCore};
use fedimint_eventlog::EventLogId;
use futures::StreamExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::journal::{
    AdoptedClaim, ApplyResult, BindResult, DepositOrigin, DepositRow, DepositState, JournalBackend,
    WithdrawalRow, WithdrawalState, is_open_deposit, is_open_withdrawal, net_credit, now_ms,
};
use crate::ops::{DepositId, WithdrawalId};
use crate::wire::FeeQuoteWire;

const TAG_DEPOSIT: u8 = 0x01;
const TAG_WITHDRAWAL: u8 = 0x02;
const TAG_CLAIM: u8 = 0x03;
const TAG_WATERMARK: u8 = 0x04;
const ROW_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct Envelope<T> {
    version: u8,
    value: T,
}

fn encode<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Envelope {
        version: ROW_VERSION,
        value,
    })?)
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    let envelope: Envelope<T> = serde_json::from_slice(bytes)?;
    anyhow::ensure!(envelope.version == ROW_VERSION, "unsupported row version");
    Ok(envelope.value)
}

fn deposit_key(id: &DepositId) -> Vec<u8> {
    let mut key = vec![TAG_DEPOSIT];
    key.extend_from_slice(id.0.as_bytes());
    key
}

fn withdrawal_key(id: &WithdrawalId) -> Vec<u8> {
    let mut key = vec![TAG_WITHDRAWAL];
    key.extend_from_slice(id.0.as_bytes());
    key
}

fn federation_bytes(federation_id: &FederationId) -> [u8; 32] {
    federation_id.0.to_byte_array()
}

fn claim_prefix(federation_id: &FederationId) -> Vec<u8> {
    let mut key = vec![TAG_CLAIM];
    key.extend_from_slice(&federation_bytes(federation_id));
    key
}

fn claim_key(federation_id: &FederationId, outpoint: bitcoin::OutPoint) -> Vec<u8> {
    let mut key = claim_prefix(federation_id);
    key.extend_from_slice(&outpoint.txid.to_byte_array());
    key.extend_from_slice(&outpoint.vout.to_le_bytes());
    key
}

fn outpoint_from_claim_key(key: &[u8]) -> anyhow::Result<bitcoin::OutPoint> {
    anyhow::ensure!(key.len() == 1 + 32 + 36, "malformed claim key");
    let txid = bitcoin::Txid::from_byte_array(key[33..65].try_into().expect("length checked"));
    let vout = u32::from_le_bytes(key[65..69].try_into().expect("length checked"));
    Ok(bitcoin::OutPoint { txid, vout })
}

fn watermark_key(federation_id: &FederationId) -> Vec<u8> {
    let mut key = vec![TAG_WATERMARK];
    key.extend_from_slice(&federation_bytes(federation_id));
    key
}

async fn get_deposit(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    id: &DepositId,
) -> anyhow::Result<Option<DepositRow>> {
    match dbtx.raw_get_bytes(&deposit_key(id)).await? {
        Some(bytes) => Ok(Some(decode::<DepositRow>(&bytes)?)),
        None => Ok(None),
    }
}

async fn write_deposit(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    row: &DepositRow,
) -> anyhow::Result<()> {
    dbtx.raw_insert_bytes(&deposit_key(&row.id), &encode(row)?)
        .await?;
    Ok(())
}

async fn scan_deposits(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
) -> anyhow::Result<Vec<DepositRow>> {
    let mut rows = Vec::new();
    let mut stream = dbtx.raw_find_by_prefix(&[TAG_DEPOSIT]).await?;
    while let Some((_key, value)) = stream.next().await {
        rows.push(decode::<DepositRow>(&value)?);
    }
    Ok(rows)
}

async fn get_withdrawal(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    id: &WithdrawalId,
) -> anyhow::Result<Option<WithdrawalRow>> {
    match dbtx.raw_get_bytes(&withdrawal_key(id)).await? {
        Some(bytes) => Ok(Some(decode::<WithdrawalRow>(&bytes)?)),
        None => Ok(None),
    }
}

async fn write_withdrawal(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    row: &WithdrawalRow,
) -> anyhow::Result<()> {
    dbtx.raw_insert_bytes(&withdrawal_key(&row.id), &encode(row)?)
        .await?;
    Ok(())
}

async fn scan_withdrawals(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
) -> anyhow::Result<Vec<WithdrawalRow>> {
    let mut rows = Vec::new();
    let mut stream = dbtx.raw_find_by_prefix(&[TAG_WITHDRAWAL]).await?;
    while let Some((_key, value)) = stream.next().await {
        rows.push(decode::<WithdrawalRow>(&value)?);
    }
    Ok(rows)
}

async fn read_claim_owner(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
    outpoint: bitcoin::OutPoint,
) -> anyhow::Result<Option<DepositId>> {
    match dbtx
        .raw_get_bytes(&claim_key(federation_id, outpoint))
        .await?
    {
        Some(bytes) => Ok(Some(decode::<DepositId>(&bytes)?)),
        None => Ok(None),
    }
}

async fn write_claim(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
    outpoint: bitcoin::OutPoint,
    id: &DepositId,
) -> anyhow::Result<()> {
    dbtx.raw_insert_bytes(&claim_key(federation_id, outpoint), &encode(id)?)
        .await?;
    Ok(())
}

/// DB-backed journal. Every transition is a read-check-write inside one
/// `DatabaseTransaction`.
#[derive(Clone, Debug)]
pub struct DbJournal {
    db: Database,
}

impl DbJournal {
    pub fn new(db: Database, client_prefix: Vec<u8>) -> Self {
        Self {
            db: db.with_prefix(client_prefix),
        }
    }
}

#[async_trait]
impl JournalBackend for DbJournal {
    async fn open_deposit(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<DepositRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let rows = scan_deposits(&mut dbtx).await?;
        dbtx.commit_tx_result().await?;
        Ok(rows
            .into_iter()
            .find(|row| row.federation_id == *federation_id && is_open_deposit(row.state)))
    }

    async fn insert_deposit(&self, row: &DepositRow) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        let rows = scan_deposits(&mut dbtx).await?;
        anyhow::ensure!(
            !rows.iter().any(|existing| {
                existing.federation_id == row.federation_id && is_open_deposit(existing.state)
            }),
            "an open deposit already exists for this federation"
        );
        write_deposit(&mut dbtx, row).await?;
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn deposit(&self, id: &DepositId) -> anyhow::Result<Option<DepositRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let row = get_deposit(&mut dbtx, id).await?;
        dbtx.commit_tx_result().await?;
        Ok(row)
    }

    async fn deposits_in(&self, states: &[DepositState]) -> anyhow::Result<Vec<DepositRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let rows = scan_deposits(&mut dbtx).await?;
        dbtx.commit_tx_result().await?;
        Ok(rows
            .into_iter()
            .filter(|row| states.contains(&row.state))
            .collect())
    }

    async fn deposit_funding_txid(&self, id: &DepositId) -> anyhow::Result<Option<bitcoin::Txid>> {
        let mut dbtx = self.db.begin_transaction().await;
        let row = get_deposit(&mut dbtx, id).await?;
        dbtx.commit_tx_result().await?;
        Ok(row.and_then(|row| row.funding_txid))
    }

    async fn set_deposit_quoted_fee(
        &self,
        id: &DepositId,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            row.quoted_fee = Some(fee);
            row.updated_at = now_ms();
            write_deposit(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_deposit_funding_txid(
        &self,
        id: &DepositId,
        txid: bitcoin::Txid,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            row.funding_txid = Some(txid);
            row.updated_at = now_ms();
            write_deposit(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_deposit_address_if(
        &self,
        id: &DepositId,
        address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
        expected: DepositState,
    ) -> anyhow::Result<ApplyResult> {
        let mut dbtx = self.db.begin_transaction().await;
        let result = match get_deposit(&mut dbtx, id).await? {
            Some(mut row) if row.state == expected => {
                let checked = address.assume_checked();
                row.address = Some(checked.to_string());
                row.address_script = Some(checked.script_pubkey().as_bytes().to_vec());
                row.state = DepositState::AddressShown;
                row.updated_at = now_ms();
                write_deposit(&mut dbtx, &row).await?;
                ApplyResult::Applied
            }
            _ => ApplyResult::Skipped,
        };
        dbtx.commit_tx_result().await?;
        Ok(result)
    }

    async fn fail_deposit_if(
        &self,
        id: &DepositId,
        expected: DepositState,
        err: &str,
    ) -> anyhow::Result<ApplyResult> {
        let mut dbtx = self.db.begin_transaction().await;
        let result = match get_deposit(&mut dbtx, id).await? {
            Some(mut row) if row.state == expected => {
                row.state = DepositState::Failed;
                row.last_error = Some(err.to_owned());
                row.updated_at = now_ms();
                write_deposit(&mut dbtx, &row).await?;
                ApplyResult::Applied
            }
            _ => ApplyResult::Skipped,
        };
        dbtx.commit_tx_result().await?;
        Ok(result)
    }

    async fn fail_deposit(&self, id: &DepositId, err: &str) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            row.state = DepositState::Failed;
            row.last_error = Some(err.to_owned());
            row.updated_at = now_ms();
            write_deposit(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_deposit_aborted(&self, id: &DepositId, next: EventLogId) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            row.next_position = Some(next);
            row.state = DepositState::AddressShown;
            row.last_error = Some("claim aborted".to_owned());
            row.updated_at = now_ms();
            write_deposit(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_deposit_next_position(
        &self,
        id: &DepositId,
        next: EventLogId,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            row.next_position = Some(next);
            row.updated_at = now_ms();
            write_deposit(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn claim_bind(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        id: &DepositId,
    ) -> anyhow::Result<BindResult> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(existing) = read_claim_owner(&mut dbtx, federation_id, outpoint).await? {
            dbtx.commit_tx_result().await?;
            return Ok(if existing == *id {
                BindResult::Bound
            } else {
                BindResult::TakenBy(existing)
            });
        }
        let Some(mut row) = get_deposit(&mut dbtx, id).await? else {
            dbtx.commit_tx_result().await?;
            anyhow::bail!("deposit not found: {}", id.0);
        };
        write_claim(&mut dbtx, federation_id, outpoint, id).await?;
        row.funding_outpoint = Some(outpoint);
        row.updated_at = now_ms();
        write_deposit(&mut dbtx, &row).await?;
        dbtx.commit_tx_result().await?;
        Ok(BindResult::Bound)
    }

    async fn bound_outpoints(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<BTreeSet<bitcoin::OutPoint>> {
        let mut dbtx = self.db.begin_transaction().await;
        let mut out = BTreeSet::new();
        let mut stream = dbtx
            .raw_find_by_prefix(&claim_prefix(federation_id))
            .await?;
        while let Some((key, _value)) = stream.next().await {
            out.insert(outpoint_from_claim_key(&key)?);
        }
        drop(stream);
        dbtx.commit_tx_result().await?;
        Ok(out)
    }

    async fn scan_watermark(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<ChronologicalOperationLogKey>> {
        let mut dbtx = self.db.begin_transaction().await;
        let watermark = match dbtx.raw_get_bytes(&watermark_key(federation_id)).await? {
            Some(bytes) => Some(decode::<ChronologicalOperationLogKey>(&bytes)?),
            None => None,
        };
        dbtx.commit_tx_result().await?;
        Ok(watermark)
    }

    async fn set_scan_watermark(
        &self,
        federation_id: &FederationId,
        key: ChronologicalOperationLogKey,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        dbtx.raw_insert_bytes(&watermark_key(federation_id), &encode(&key)?)
            .await?;
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn finish_deposit(
        &self,
        id: &DepositId,
        outpoint: bitcoin::OutPoint,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            row.funding_outpoint = Some(outpoint);
            row.gross_deposited = Some(gross);
            row.claim_fee = Some(fee);
            row.net_credit = Some(net_credit(gross, fee));
            row.state = DepositState::Done;
            row.updated_at = now_ms();
            write_deposit(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn close_deposit_done(&self, id: &DepositId) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            row.state = DepositState::Done;
            row.updated_at = now_ms();
            write_deposit(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn claim_adopt_if_absent(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        claim: AdoptedClaim,
    ) -> anyhow::Result<BindResult> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(existing) = read_claim_owner(&mut dbtx, federation_id, outpoint).await? {
            dbtx.commit_tx_result().await?;
            return Ok(BindResult::TakenBy(existing));
        }
        let id = DepositId::new();
        let now = now_ms();
        let row = DepositRow {
            id: id.clone(),
            federation_id: *federation_id,
            origin: DepositOrigin::Adopted,
            position: None,
            amount: claim.value,
            address: claim.address,
            address_script: claim.address_script,
            state: DepositState::Done,
            next_position: None,
            operation_id: Some(claim.operation_id),
            funding_outpoint: Some(outpoint),
            funding_txid: Some(outpoint.txid),
            quoted_fee: None,
            claim_fee: Some(claim.fee),
            gross_deposited: Some(claim.value),
            net_credit: Some(net_credit(claim.value, claim.fee)),
            last_error: None,
            created_at: now,
            updated_at: now,
        };
        write_deposit(&mut dbtx, &row).await?;
        write_claim(&mut dbtx, federation_id, outpoint, &id).await?;
        dbtx.commit_tx_result().await?;
        Ok(BindResult::Bound)
    }

    async fn close_deposit_done_if_open(
        &self,
        id: &DepositId,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_deposit(&mut dbtx, id).await? {
            if is_open_deposit(row.state) {
                if row.net_credit.is_none() {
                    row.gross_deposited = Some(gross);
                    row.claim_fee = Some(fee);
                    row.net_credit = Some(net_credit(gross, fee));
                }
                row.state = DepositState::Done;
                row.updated_at = now_ms();
                write_deposit(&mut dbtx, &row).await?;
            }
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn cancel_deposit(&self, id: &DepositId) -> anyhow::Result<ApplyResult> {
        let mut dbtx = self.db.begin_transaction().await;
        let result = match get_deposit(&mut dbtx, id).await? {
            Some(mut row) if is_open_deposit(row.state) && row.funding_txid.is_none() => {
                row.state = DepositState::Cancelled;
                row.updated_at = now_ms();
                write_deposit(&mut dbtx, &row).await?;
                ApplyResult::Applied
            }
            _ => ApplyResult::Skipped,
        };
        dbtx.commit_tx_result().await?;
        Ok(result)
    }

    async fn insert_withdrawal(&self, row: &WithdrawalRow) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        let rows = scan_withdrawals(&mut dbtx).await?;
        anyhow::ensure!(
            !rows.iter().any(|existing| {
                existing.federation_id == row.federation_id && is_open_withdrawal(&existing.state)
            }),
            "an in-flight withdrawal already exists for this federation"
        );
        write_withdrawal(&mut dbtx, row).await?;
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<Option<WithdrawalRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let row = get_withdrawal(&mut dbtx, id).await?;
        dbtx.commit_tx_result().await?;
        Ok(row)
    }

    async fn open_withdrawal(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<WithdrawalRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let rows = scan_withdrawals(&mut dbtx).await?;
        dbtx.commit_tx_result().await?;
        Ok(rows
            .into_iter()
            .find(|row| row.federation_id == *federation_id && is_open_withdrawal(&row.state)))
    }

    async fn set_withdrawal_operation_id(
        &self,
        id: &WithdrawalId,
        operation_id: OperationId,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_withdrawal(&mut dbtx, id).await? {
            row.operation_id = Some(operation_id);
            row.state = WithdrawalState::Submitted;
            row.updated_at = now_ms();
            write_withdrawal(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_withdrawal_quote(
        &self,
        id: &WithdrawalId,
        quote: FeeQuoteWire,
    ) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_withdrawal(&mut dbtx, id).await? {
            row.quote = Some(quote);
            row.updated_at = now_ms();
            write_withdrawal(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_withdrawal_failed(&self, id: &WithdrawalId, err: &str) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_withdrawal(&mut dbtx, id).await? {
            row.state = WithdrawalState::Failed;
            row.last_error = Some(err.to_owned());
            row.updated_at = now_ms();
            write_withdrawal(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_broadcast(&self, id: &WithdrawalId, txid: bitcoin::Txid) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_withdrawal(&mut dbtx, id).await? {
            row.txid = Some(txid);
            row.state = WithdrawalState::Broadcast { txid };
            row.updated_at = now_ms();
            write_withdrawal(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_refunded(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_withdrawal(&mut dbtx, id).await? {
            row.state = WithdrawalState::Refunded;
            row.updated_at = now_ms();
            write_withdrawal(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn set_failed(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let mut dbtx = self.db.begin_transaction().await;
        if let Some(mut row) = get_withdrawal(&mut dbtx, id).await? {
            row.state = WithdrawalState::Failed;
            row.updated_at = now_ms();
            write_withdrawal(&mut dbtx, &row).await?;
        }
        dbtx.commit_tx_result().await?;
        Ok(())
    }

    async fn cancel_withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<ApplyResult> {
        let mut dbtx = self.db.begin_transaction().await;
        let result = match get_withdrawal(&mut dbtx, id).await? {
            Some(mut row) if row.state == WithdrawalState::Quoting => {
                row.state = WithdrawalState::Cancelled;
                row.updated_at = now_ms();
                write_withdrawal(&mut dbtx, &row).await?;
                ApplyResult::Applied
            }
            _ => ApplyResult::Skipped,
        };
        dbtx.commit_tx_result().await?;
        Ok(result)
    }

    async fn scan_for_send(&self, id: &WithdrawalId) -> anyhow::Result<Option<OperationId>> {
        let mut dbtx = self.db.begin_transaction().await;
        let row = get_withdrawal(&mut dbtx, id).await?;
        dbtx.commit_tx_result().await?;
        Ok(row.and_then(|row| row.operation_id))
    }
}
