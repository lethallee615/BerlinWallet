use std::collections::BTreeSet;
use std::time::Duration;

use async_trait::async_trait;
use bitcoin::hashes::Hash as _;
use fedimint_client::db::ChronologicalOperationLogKey;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::db::{
    Committable, Database, DatabaseError, DatabaseTransaction, IDatabaseTransactionOpsCore,
};
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

const TAG_OPEN_DEPOSIT: u8 = 0x05;
const TAG_OPEN_WITHDRAWAL: u8 = 0x06;
const RETRY_ON_CONFLICT_ATTEMPTS: u32 = 5;

fn open_deposit_key(federation_id: &FederationId) -> Vec<u8> {
    let mut key = vec![TAG_OPEN_DEPOSIT];
    key.extend_from_slice(&federation_bytes(federation_id));
    key
}

fn open_withdrawal_key(federation_id: &FederationId) -> Vec<u8> {
    let mut key = vec![TAG_OPEN_WITHDRAWAL];
    key.extend_from_slice(&federation_bytes(federation_id));
    key
}

async fn read_open_deposit(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
) -> anyhow::Result<Option<DepositId>> {
    match dbtx.raw_get_bytes(&open_deposit_key(federation_id)).await? {
        Some(bytes) => Ok(Some(decode::<DepositId>(&bytes)?)),
        None => Ok(None),
    }
}

async fn write_open_deposit(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
    id: &DepositId,
) -> anyhow::Result<()> {
    dbtx.raw_insert_bytes(&open_deposit_key(federation_id), &encode(id)?)
        .await?;
    Ok(())
}

async fn clear_open_deposit(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
    id: &DepositId,
) -> anyhow::Result<()> {
    if read_open_deposit(dbtx, federation_id).await?.as_ref() == Some(id) {
        dbtx.raw_remove_entry(&open_deposit_key(federation_id))
            .await?;
    }
    Ok(())
}

async fn read_open_withdrawal(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
) -> anyhow::Result<Option<WithdrawalId>> {
    match dbtx
        .raw_get_bytes(&open_withdrawal_key(federation_id))
        .await?
    {
        Some(bytes) => Ok(Some(decode::<WithdrawalId>(&bytes)?)),
        None => Ok(None),
    }
}

async fn write_open_withdrawal(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
    id: &WithdrawalId,
) -> anyhow::Result<()> {
    dbtx.raw_insert_bytes(&open_withdrawal_key(federation_id), &encode(id)?)
        .await?;
    Ok(())
}

async fn clear_open_withdrawal(
    dbtx: &mut DatabaseTransaction<'_, Committable>,
    federation_id: &FederationId,
    id: &WithdrawalId,
) -> anyhow::Result<()> {
    if read_open_withdrawal(dbtx, federation_id).await?.as_ref() == Some(id) {
        dbtx.raw_remove_entry(&open_withdrawal_key(federation_id))
            .await?;
    }
    Ok(())
}

fn is_retryable_conflict(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<DatabaseError>(),
        Some(DatabaseError::WriteConflict | DatabaseError::SnapshotTooOld(_))
    )
}

fn jittered_backoff(attempt: u32) -> Duration {
    let base_ms = 5u64.saturating_mul(1u64 << attempt.min(6));
    let jitter_ms = now_ms() % (base_ms / 2 + 1);
    Duration::from_millis(base_ms + jitter_ms)
}

/// Re-run the whole transition on an optimistic commit conflict. The closure
/// must open a fresh transaction (and re-read/re-check) on every call.
async fn retry_on_conflict<T, F, Fut>(mut op: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut attempt: u32 = 1;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt < RETRY_ON_CONFLICT_ATTEMPTS && is_retryable_conflict(&error) => {
                let backoff = jittered_backoff(attempt);
                tracing::debug!(
                    attempt,
                    backoff_ms = backoff.as_millis(),
                    %error,
                    "journal transaction conflict; retrying"
                );
                tokio::time::sleep(backoff).await;
                attempt += 1;
            }
            Err(error) => return Err(error),
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
        let row = match read_open_deposit(&mut dbtx, federation_id).await? {
            Some(id) => Some(get_deposit(&mut dbtx, &id).await?.ok_or_else(|| {
                anyhow::anyhow!("open deposit index points at missing row: {}", id.0)
            })?),
            None => None,
        };
        dbtx.commit_tx_result().await?;
        Ok(row)
    }

    async fn insert_deposit(&self, row: &DepositRow) -> anyhow::Result<DepositRow> {
        let this = self;
        let row = row.clone();
        retry_on_conflict(move || {
            let row = row.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if is_open_deposit(row.state) {
                    if let Some(existing_id) =
                        read_open_deposit(&mut dbtx, &row.federation_id).await?
                    {
                        let existing =
                            get_deposit(&mut dbtx, &existing_id).await?.ok_or_else(|| {
                                anyhow::anyhow!(
                                    "open deposit index points at missing row: {}",
                                    existing_id.0
                                )
                            })?;
                        dbtx.commit_tx_result().await?;
                        return Ok(existing);
                    }
                    write_deposit(&mut dbtx, &row).await?;
                    write_open_deposit(&mut dbtx, &row.federation_id, &row.id).await?;
                } else {
                    write_deposit(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(row)
            }
        })
        .await
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
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    row.quoted_fee = Some(fee);
                    row.updated_at = now_ms();
                    write_deposit(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_deposit_funding_txid(
        &self,
        id: &DepositId,
        txid: bitcoin::Txid,
    ) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    row.funding_txid = Some(txid);
                    row.updated_at = now_ms();
                    write_deposit(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_deposit_address_if(
        &self,
        id: &DepositId,
        address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
        expected: DepositState,
    ) -> anyhow::Result<ApplyResult> {
        let this = self;
        let id = id.clone();
        let address = address.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            let address = address.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                let result = match get_deposit(&mut dbtx, &id).await? {
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
        })
        .await
    }

    async fn fail_deposit_if(
        &self,
        id: &DepositId,
        expected: DepositState,
        err: &str,
    ) -> anyhow::Result<ApplyResult> {
        let this = self;
        let id = id.clone();
        let err = err.to_owned();
        retry_on_conflict(move || {
            let id = id.clone();
            let err = err.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                let result = match get_deposit(&mut dbtx, &id).await? {
                    Some(mut row) if row.state == expected => {
                        row.state = DepositState::Failed;
                        row.last_error = Some(err);
                        row.updated_at = now_ms();
                        write_deposit(&mut dbtx, &row).await?;
                        clear_open_deposit(&mut dbtx, &row.federation_id, &row.id).await?;
                        ApplyResult::Applied
                    }
                    _ => ApplyResult::Skipped,
                };
                dbtx.commit_tx_result().await?;
                Ok(result)
            }
        })
        .await
    }

    async fn fail_deposit(&self, id: &DepositId, err: &str) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        let err = err.to_owned();
        retry_on_conflict(move || {
            let id = id.clone();
            let err = err.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    row.state = DepositState::Failed;
                    row.last_error = Some(err);
                    row.updated_at = now_ms();
                    write_deposit(&mut dbtx, &row).await?;
                    clear_open_deposit(&mut dbtx, &row.federation_id, &row.id).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_deposit_aborted(&self, id: &DepositId, next: EventLogId) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    row.next_position = Some(next);
                    row.state = DepositState::AddressShown;
                    row.last_error = Some("claim aborted".to_owned());
                    row.updated_at = now_ms();
                    write_deposit(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_deposit_next_position(
        &self,
        id: &DepositId,
        next: EventLogId,
    ) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    row.next_position = Some(next);
                    row.updated_at = now_ms();
                    write_deposit(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn claim_bind(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        id: &DepositId,
    ) -> anyhow::Result<BindResult> {
        let this = self;
        let federation_id = *federation_id;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(existing) =
                    read_claim_owner(&mut dbtx, &federation_id, outpoint).await?
                {
                    dbtx.commit_tx_result().await?;
                    return Ok(if existing == id {
                        BindResult::Bound
                    } else {
                        BindResult::TakenBy(existing)
                    });
                }
                let Some(mut row) = get_deposit(&mut dbtx, &id).await? else {
                    dbtx.commit_tx_result().await?;
                    anyhow::bail!("deposit not found: {}", id.0);
                };
                write_claim(&mut dbtx, &federation_id, outpoint, &id).await?;
                row.funding_outpoint = Some(outpoint);
                row.updated_at = now_ms();
                write_deposit(&mut dbtx, &row).await?;
                dbtx.commit_tx_result().await?;
                Ok(BindResult::Bound)
            }
        })
        .await
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
        let this = self;
        let federation_id = *federation_id;
        retry_on_conflict(move || async move {
            let mut dbtx = this.db.begin_transaction().await;
            dbtx.raw_insert_bytes(&watermark_key(&federation_id), &encode(&key)?)
                .await?;
            dbtx.commit_tx_result().await?;
            Ok(())
        })
        .await
    }

    async fn finish_deposit(
        &self,
        id: &DepositId,
        outpoint: bitcoin::OutPoint,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    row.funding_outpoint = Some(outpoint);
                    row.gross_deposited = Some(gross);
                    row.claim_fee = Some(fee);
                    row.net_credit = Some(net_credit(gross, fee));
                    row.state = DepositState::Done;
                    row.updated_at = now_ms();
                    write_deposit(&mut dbtx, &row).await?;
                    clear_open_deposit(&mut dbtx, &row.federation_id, &row.id).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn close_deposit_done(&self, id: &DepositId) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    row.state = DepositState::Done;
                    row.updated_at = now_ms();
                    write_deposit(&mut dbtx, &row).await?;
                    clear_open_deposit(&mut dbtx, &row.federation_id, &row.id).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn claim_adopt_if_absent(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        claim: AdoptedClaim,
    ) -> anyhow::Result<BindResult> {
        let this = self;
        let federation_id = *federation_id;
        let claim = claim.clone();
        retry_on_conflict(move || {
            let claim = claim.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(existing) =
                    read_claim_owner(&mut dbtx, &federation_id, outpoint).await?
                {
                    dbtx.commit_tx_result().await?;
                    return Ok(BindResult::TakenBy(existing));
                }
                let id = DepositId::new();
                let now = now_ms();
                let row = DepositRow {
                    id: id.clone(),
                    federation_id,
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
                write_claim(&mut dbtx, &federation_id, outpoint, &id).await?;
                dbtx.commit_tx_result().await?;
                Ok(BindResult::Bound)
            }
        })
        .await
    }

    async fn close_deposit_done_if_open(
        &self,
        id: &DepositId,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_deposit(&mut dbtx, &id).await? {
                    if is_open_deposit(row.state) {
                        if row.net_credit.is_none() {
                            row.gross_deposited = Some(gross);
                            row.claim_fee = Some(fee);
                            row.net_credit = Some(net_credit(gross, fee));
                        }
                        row.state = DepositState::Done;
                        row.updated_at = now_ms();
                        write_deposit(&mut dbtx, &row).await?;
                        clear_open_deposit(&mut dbtx, &row.federation_id, &row.id).await?;
                    }
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn cancel_deposit(&self, id: &DepositId) -> anyhow::Result<ApplyResult> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                let result = match get_deposit(&mut dbtx, &id).await? {
                    Some(mut row) if is_open_deposit(row.state) && row.funding_txid.is_none() => {
                        row.state = DepositState::Cancelled;
                        row.updated_at = now_ms();
                        write_deposit(&mut dbtx, &row).await?;
                        clear_open_deposit(&mut dbtx, &row.federation_id, &row.id).await?;
                        ApplyResult::Applied
                    }
                    _ => ApplyResult::Skipped,
                };
                dbtx.commit_tx_result().await?;
                Ok(result)
            }
        })
        .await
    }

    async fn insert_withdrawal(&self, row: &WithdrawalRow) -> anyhow::Result<()> {
        let this = self;
        let row = row.clone();
        retry_on_conflict(move || {
            let row = row.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if is_open_withdrawal(&row.state) {
                    if let Some(existing_id) =
                        read_open_withdrawal(&mut dbtx, &row.federation_id).await?
                    {
                        dbtx.commit_tx_result().await?;
                        anyhow::bail!(
                            "an in-flight withdrawal already exists for this federation: {}",
                            existing_id.0
                        );
                    }
                    write_withdrawal(&mut dbtx, &row).await?;
                    write_open_withdrawal(&mut dbtx, &row.federation_id, &row.id).await?;
                } else {
                    write_withdrawal(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<Option<WithdrawalRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let row = get_withdrawal(&mut dbtx, id).await?;
        dbtx.commit_tx_result().await?;
        Ok(row)
    }

    async fn withdrawals_in(
        &self,
        states: &[WithdrawalState],
    ) -> anyhow::Result<Vec<WithdrawalRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let rows = scan_withdrawals(&mut dbtx).await?;
        dbtx.commit_tx_result().await?;
        Ok(rows
            .into_iter()
            .filter(|row| states.contains(&row.state))
            .collect())
    }

    async fn open_withdrawal(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<WithdrawalRow>> {
        let mut dbtx = self.db.begin_transaction().await;
        let row = match read_open_withdrawal(&mut dbtx, federation_id).await? {
            Some(id) => Some(get_withdrawal(&mut dbtx, &id).await?.ok_or_else(|| {
                anyhow::anyhow!("open withdrawal index points at missing row: {}", id.0)
            })?),
            None => None,
        };
        dbtx.commit_tx_result().await?;
        Ok(row)
    }

    async fn set_withdrawal_operation_id(
        &self,
        id: &WithdrawalId,
        operation_id: OperationId,
    ) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_withdrawal(&mut dbtx, &id).await? {
                    row.operation_id = Some(operation_id);
                    row.state = WithdrawalState::Submitted;
                    row.updated_at = now_ms();
                    write_withdrawal(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_withdrawal_quote(
        &self,
        id: &WithdrawalId,
        quote: FeeQuoteWire,
    ) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        let quote = quote.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            let quote = quote.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_withdrawal(&mut dbtx, &id).await? {
                    row.quote = Some(quote);
                    row.updated_at = now_ms();
                    write_withdrawal(&mut dbtx, &row).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_withdrawal_failed(&self, id: &WithdrawalId, err: &str) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        let err = err.to_owned();
        retry_on_conflict(move || {
            let id = id.clone();
            let err = err.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_withdrawal(&mut dbtx, &id).await? {
                    row.state = WithdrawalState::Failed;
                    row.last_error = Some(err);
                    row.updated_at = now_ms();
                    write_withdrawal(&mut dbtx, &row).await?;
                    clear_open_withdrawal(&mut dbtx, &row.federation_id, &row.id).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_broadcast(&self, id: &WithdrawalId, txid: bitcoin::Txid) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_withdrawal(&mut dbtx, &id).await? {
                    row.txid = Some(txid);
                    row.state = WithdrawalState::Broadcast { txid };
                    row.updated_at = now_ms();
                    write_withdrawal(&mut dbtx, &row).await?;
                    clear_open_withdrawal(&mut dbtx, &row.federation_id, &row.id).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_refunded(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_withdrawal(&mut dbtx, &id).await? {
                    row.state = WithdrawalState::Refunded;
                    row.updated_at = now_ms();
                    write_withdrawal(&mut dbtx, &row).await?;
                    clear_open_withdrawal(&mut dbtx, &row.federation_id, &row.id).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn set_failed(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                if let Some(mut row) = get_withdrawal(&mut dbtx, &id).await? {
                    row.state = WithdrawalState::Failed;
                    row.updated_at = now_ms();
                    write_withdrawal(&mut dbtx, &row).await?;
                    clear_open_withdrawal(&mut dbtx, &row.federation_id, &row.id).await?;
                }
                dbtx.commit_tx_result().await?;
                Ok(())
            }
        })
        .await
    }

    async fn cancel_withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<ApplyResult> {
        let this = self;
        let id = id.clone();
        retry_on_conflict(move || {
            let id = id.clone();
            async move {
                let mut dbtx = this.db.begin_transaction().await;
                let result = match get_withdrawal(&mut dbtx, &id).await? {
                    Some(mut row) if row.state == WithdrawalState::Quoting => {
                        row.state = WithdrawalState::Cancelled;
                        row.updated_at = now_ms();
                        write_withdrawal(&mut dbtx, &row).await?;
                        clear_open_withdrawal(&mut dbtx, &row.federation_id, &row.id).await?;
                        ApplyResult::Applied
                    }
                    _ => ApplyResult::Skipped,
                };
                dbtx.commit_tx_result().await?;
                Ok(result)
            }
        })
        .await
    }

    async fn scan_for_send(&self, id: &WithdrawalId) -> anyhow::Result<Option<OperationId>> {
        let mut dbtx = self.db.begin_transaction().await;
        let row = get_withdrawal(&mut dbtx, id).await?;
        dbtx.commit_tx_result().await?;
        Ok(row.and_then(|row| row.operation_id))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use bitcoin::hashes::Hash as _;
    use fedimint_core::db::mem_impl::MemDatabase;
    use fedimint_core::module::registry::ModuleDecoderRegistry;

    use super::*;
    use crate::journal::Journal;

    fn mem_journal(prefix: u8) -> Journal {
        let db = Database::new(MemDatabase::new(), ModuleDecoderRegistry::default());
        Journal::new(db, vec![prefix])
    }

    #[tokio::test]
    async fn open_index_tracks_single_open_deposit() {
        let journal = mem_journal(0x00);
        let federation_id = FederationId::dummy();

        let first = DepositRow::requested(
            DepositId::new(),
            federation_id,
            EventLogId::LOG_START,
            bitcoin::Amount::from_sat(10_000),
        );
        let inserted = journal.insert_deposit(&first).await.expect("insert first");
        assert_eq!(inserted.id, first.id);
        assert_eq!(
            journal
                .open_deposit(&federation_id)
                .await
                .expect("open")
                .expect("row")
                .id,
            first.id
        );

        let second = DepositRow::requested(
            DepositId::new(),
            federation_id,
            EventLogId::LOG_START.next(),
            bitcoin::Amount::from_sat(20_000),
        );
        let existing = journal
            .insert_deposit(&second)
            .await
            .expect("second open insert returns the existing row");
        assert_eq!(existing.id, first.id);

        let open_rows = journal
            .deposits_in(&[DepositState::AwaitingAddress, DepositState::AddressShown])
            .await
            .expect("list open deposits");
        assert_eq!(open_rows.len(), 1, "exactly one open row must exist");

        let outpoint = bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([0x22; 32]), 1);
        journal
            .finish_deposit(
                &first.id,
                outpoint,
                bitcoin::Amount::from_sat(10_000),
                bitcoin::Amount::from_sat(100),
            )
            .await
            .expect("finish");
        assert!(
            journal
                .open_deposit(&federation_id)
                .await
                .expect("open")
                .is_none(),
            "terminal transition must clear the open index"
        );

        let third = DepositRow::requested(
            DepositId::new(),
            federation_id,
            EventLogId::LOG_START.next().next(),
            bitcoin::Amount::from_sat(30_000),
        );
        journal
            .insert_deposit(&third)
            .await
            .expect("a new open row is allowed after the prior row is terminal");
    }

    #[tokio::test]
    async fn open_index_tracks_single_open_withdrawal() {
        let journal = mem_journal(0x01);
        let federation_id = FederationId::dummy();

        let first = WithdrawalRow::new(
            WithdrawalId::new(),
            federation_id,
            "bcrt1qtest".to_owned(),
            bitcoin::Amount::from_sat(10_000),
        );
        journal
            .insert_withdrawal(&first)
            .await
            .expect("insert first");
        assert_eq!(
            journal
                .open_withdrawal(&federation_id)
                .await
                .expect("open")
                .expect("row")
                .id,
            first.id
        );

        let second = WithdrawalRow::new(
            WithdrawalId::new(),
            federation_id,
            "bcrt1qtest".to_owned(),
            bitcoin::Amount::from_sat(20_000),
        );
        assert!(
            journal.insert_withdrawal(&second).await.is_err(),
            "a second in-flight withdrawal must be rejected"
        );

        journal
            .set_withdrawal_failed(&first.id, "test failure")
            .await
            .expect("fail");
        assert!(
            journal
                .open_withdrawal(&federation_id)
                .await
                .expect("open")
                .is_none(),
            "terminal transition must clear the open index"
        );
        journal
            .insert_withdrawal(&second)
            .await
            .expect("a new in-flight withdrawal is allowed after the prior row is terminal");
    }

    #[tokio::test]
    async fn retry_on_conflict_retries_write_conflict() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_for_op = attempts.clone();
        let result: anyhow::Result<u32> = retry_on_conflict(move || {
            let attempts = attempts_for_op.clone();
            async move {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    Err(DatabaseError::WriteConflict.into())
                } else {
                    Ok(attempt)
                }
            }
        })
        .await;

        assert_eq!(result.expect("eventually succeeds"), 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }
}
