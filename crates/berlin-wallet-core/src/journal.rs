use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use fedimint_client::db::ChronologicalOperationLogKey;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::db::Database;
use fedimint_eventlog::EventLogId;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::ops::{DepositId, WithdrawalId};
use crate::wire::FeeQuoteWire;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepositOrigin {
    Requested,
    Adopted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DepositState {
    AwaitingAddress,
    AddressShown,
    Done,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WithdrawalState {
    Quoting,
    Submitted,
    Broadcast { txid: bitcoin::Txid },
    Refunded,
    Failed,
    Cancelled,
}

/// `claim_bind` result: either this call owns the credit, or another row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindResult {
    Bound,
    TakenBy(DepositId),
}

/// Result of a conditional state write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyResult {
    Applied,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepositRow {
    pub id: DepositId,
    pub federation_id: FederationId,
    pub origin: DepositOrigin,
    pub position: Option<EventLogId>,
    pub amount: bitcoin::Amount,
    pub address: Option<String>,
    pub address_script: Option<Vec<u8>>,
    pub state: DepositState,
    pub next_position: Option<EventLogId>,
    pub operation_id: Option<OperationId>,
    pub funding_outpoint: Option<bitcoin::OutPoint>,
    pub funding_txid: Option<bitcoin::Txid>,
    pub quoted_fee: Option<bitcoin::Amount>,
    pub claim_fee: Option<bitcoin::Amount>,
    pub gross_deposited: Option<bitcoin::Amount>,
    pub net_credit: Option<fedimint_core::Amount>,
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl DepositRow {
    pub fn requested(
        id: DepositId,
        federation_id: FederationId,
        position: EventLogId,
        amount: bitcoin::Amount,
    ) -> Self {
        let now = now_ms();
        Self {
            id,
            federation_id,
            origin: DepositOrigin::Requested,
            position: Some(position),
            amount,
            address: None,
            address_script: None,
            state: DepositState::AwaitingAddress,
            next_position: None,
            operation_id: None,
            funding_outpoint: None,
            funding_txid: None,
            quoted_fee: None,
            claim_fee: None,
            gross_deposited: None,
            net_credit: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WithdrawalRow {
    pub id: WithdrawalId,
    pub federation_id: FederationId,
    pub address: String,
    pub value: bitcoin::Amount,
    pub fee: Option<bitcoin::Amount>,
    /// Typed wire form of `FeeQuote` (which is not `Serialize`).
    pub quote: Option<FeeQuoteWire>,
    pub operation_id: Option<OperationId>,
    pub state: WithdrawalState,
    pub txid: Option<bitcoin::Txid>,
    pub custom_meta: String,
    pub last_error: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl WithdrawalRow {
    pub fn new(
        id: WithdrawalId,
        federation_id: FederationId,
        address: String,
        value: bitcoin::Amount,
    ) -> Self {
        let now = now_ms();
        Self {
            custom_meta: serde_json::json!({ "id": id.0 }).to_string(),
            id,
            federation_id,
            address,
            value,
            fee: None,
            quote: None,
            operation_id: None,
            state: WithdrawalState::Quoting,
            txid: None,
            last_error: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// The claim figures an orphan-adoption pass carries into a new row.
#[derive(Debug, Clone)]
pub struct AdoptedClaim {
    pub address: Option<String>,
    pub address_script: Option<Vec<u8>>,
    pub operation_id: OperationId,
    pub value: bitcoin::Amount,
    pub fee: bitcoin::Amount,
}

#[derive(Debug, Default)]
struct JournalInner {
    deposits: BTreeMap<DepositId, DepositRow>,
    withdrawals: BTreeMap<WithdrawalId, WithdrawalRow>,
    claims: BTreeMap<(FederationId, bitcoin::OutPoint), DepositId>,
    /// Last operation-log key scanned per federation (scan watermark).
    watermarks: BTreeMap<FederationId, ChronologicalOperationLogKey>,
}

#[async_trait]
pub trait JournalBackend: Send + Sync {
    async fn open_deposit(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<DepositRow>>;
    async fn insert_deposit(&self, row: &DepositRow) -> anyhow::Result<DepositRow>;
    async fn deposit(&self, id: &DepositId) -> anyhow::Result<Option<DepositRow>>;
    async fn deposits_in(&self, states: &[DepositState]) -> anyhow::Result<Vec<DepositRow>>;
    async fn deposit_funding_txid(&self, id: &DepositId) -> anyhow::Result<Option<bitcoin::Txid>>;
    async fn set_deposit_quoted_fee(
        &self,
        id: &DepositId,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()>;
    async fn set_deposit_funding_txid(
        &self,
        id: &DepositId,
        txid: bitcoin::Txid,
    ) -> anyhow::Result<()>;
    async fn set_deposit_address_if(
        &self,
        id: &DepositId,
        address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
        expected: DepositState,
    ) -> anyhow::Result<ApplyResult>;
    async fn fail_deposit_if(
        &self,
        id: &DepositId,
        expected: DepositState,
        err: &str,
    ) -> anyhow::Result<ApplyResult>;
    async fn fail_deposit(&self, id: &DepositId, err: &str) -> anyhow::Result<()>;
    async fn set_deposit_aborted(&self, id: &DepositId, next: EventLogId) -> anyhow::Result<()>;
    async fn set_deposit_next_position(
        &self,
        id: &DepositId,
        next: EventLogId,
    ) -> anyhow::Result<()>;
    async fn claim_bind(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        id: &DepositId,
    ) -> anyhow::Result<BindResult>;
    async fn bound_outpoints(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<BTreeSet<bitcoin::OutPoint>>;
    async fn scan_watermark(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<ChronologicalOperationLogKey>>;
    async fn set_scan_watermark(
        &self,
        federation_id: &FederationId,
        key: ChronologicalOperationLogKey,
    ) -> anyhow::Result<()>;
    async fn finish_deposit(
        &self,
        id: &DepositId,
        outpoint: bitcoin::OutPoint,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()>;
    async fn close_deposit_done(&self, id: &DepositId) -> anyhow::Result<()>;
    async fn claim_adopt_if_absent(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        claim: AdoptedClaim,
    ) -> anyhow::Result<BindResult>;
    async fn close_deposit_done_if_open(
        &self,
        id: &DepositId,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()>;
    async fn cancel_deposit(&self, id: &DepositId) -> anyhow::Result<ApplyResult>;
    async fn insert_withdrawal(&self, row: &WithdrawalRow) -> anyhow::Result<()>;
    async fn withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<Option<WithdrawalRow>>;
    async fn withdrawals_in(
        &self,
        states: &[WithdrawalState],
    ) -> anyhow::Result<Vec<WithdrawalRow>>;
    async fn open_withdrawal(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<WithdrawalRow>>;
    async fn set_withdrawal_operation_id(
        &self,
        id: &WithdrawalId,
        operation_id: OperationId,
    ) -> anyhow::Result<()>;
    async fn set_withdrawal_quote(
        &self,
        id: &WithdrawalId,
        quote: FeeQuoteWire,
    ) -> anyhow::Result<()>;
    async fn set_withdrawal_failed(&self, id: &WithdrawalId, err: &str) -> anyhow::Result<()>;
    async fn set_broadcast(&self, id: &WithdrawalId, txid: bitcoin::Txid) -> anyhow::Result<()>;
    async fn set_refunded(&self, id: &WithdrawalId) -> anyhow::Result<()>;
    async fn set_failed(&self, id: &WithdrawalId) -> anyhow::Result<()>;
    async fn cancel_withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<ApplyResult>;
    async fn scan_for_send(&self, id: &WithdrawalId) -> anyhow::Result<Option<OperationId>>;
}

/// In-memory implementation of [`JournalBackend`], used by unit tests and
/// [`Journal::in_memory`].
#[derive(Debug, Default)]
pub struct MemoryJournal {
    inner: Arc<Mutex<JournalInner>>,
}

#[async_trait]
impl JournalBackend for MemoryJournal {
    async fn open_deposit(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<DepositRow>> {
        let inner = self.inner.lock().await;
        Ok(inner
            .deposits
            .values()
            .find(|row| {
                row.federation_id == *federation_id
                    && matches!(
                        row.state,
                        DepositState::AwaitingAddress | DepositState::AddressShown
                    )
            })
            .cloned())
    }

    async fn insert_deposit(&self, row: &DepositRow) -> anyhow::Result<DepositRow> {
        let mut inner = self.inner.lock().await;
        if is_open_deposit(row.state) {
            if let Some(existing) = inner.deposits.values().find(|existing| {
                existing.federation_id == row.federation_id && is_open_deposit(existing.state)
            }) {
                return Ok(existing.clone());
            }
        }
        inner.deposits.insert(row.id.clone(), row.clone());
        Ok(row.clone())
    }

    async fn deposit(&self, id: &DepositId) -> anyhow::Result<Option<DepositRow>> {
        let inner = self.inner.lock().await;
        Ok(inner.deposits.get(id).cloned())
    }

    async fn deposits_in(&self, states: &[DepositState]) -> anyhow::Result<Vec<DepositRow>> {
        let inner = self.inner.lock().await;
        Ok(inner
            .deposits
            .values()
            .filter(|row| states.contains(&row.state))
            .cloned()
            .collect())
    }

    async fn deposit_funding_txid(&self, id: &DepositId) -> anyhow::Result<Option<bitcoin::Txid>> {
        let inner = self.inner.lock().await;
        Ok(inner.deposits.get(id).and_then(|row| row.funding_txid))
    }

    async fn set_deposit_quoted_fee(
        &self,
        id: &DepositId,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.quoted_fee = Some(fee);
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_deposit_funding_txid(
        &self,
        id: &DepositId,
        txid: bitcoin::Txid,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.funding_txid = Some(txid);
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_deposit_address_if(
        &self,
        id: &DepositId,
        address: bitcoin::Address<bitcoin::address::NetworkUnchecked>,
        expected: DepositState,
    ) -> anyhow::Result<ApplyResult> {
        let mut inner = self.inner.lock().await;
        let Some(row) = inner.deposits.get_mut(id) else {
            return Ok(ApplyResult::Skipped);
        };
        if row.state != expected {
            return Ok(ApplyResult::Skipped);
        }
        let checked = address.assume_checked();
        row.address = Some(checked.to_string());
        row.address_script = Some(checked.script_pubkey().as_bytes().to_vec());
        row.state = DepositState::AddressShown;
        row.updated_at = now_ms();
        Ok(ApplyResult::Applied)
    }

    async fn fail_deposit_if(
        &self,
        id: &DepositId,
        expected: DepositState,
        err: &str,
    ) -> anyhow::Result<ApplyResult> {
        let mut inner = self.inner.lock().await;
        let Some(row) = inner.deposits.get_mut(id) else {
            return Ok(ApplyResult::Skipped);
        };
        if row.state != expected {
            return Ok(ApplyResult::Skipped);
        }
        row.state = DepositState::Failed;
        row.last_error = Some(err.to_owned());
        row.updated_at = now_ms();
        Ok(ApplyResult::Applied)
    }

    async fn fail_deposit(&self, id: &DepositId, err: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.state = DepositState::Failed;
            row.last_error = Some(err.to_owned());
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_deposit_aborted(&self, id: &DepositId, next: EventLogId) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.next_position = Some(next);
            row.state = DepositState::AddressShown;
            row.last_error = Some("claim aborted".to_owned());
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_deposit_next_position(
        &self,
        id: &DepositId,
        next: EventLogId,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.next_position = Some(next);
            row.updated_at = now_ms();
        }
        Ok(())
    }

    /// The single claim-key writer. `Bound` means this row owns the credit;
    /// `TakenBy` means another row already does.
    async fn claim_bind(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        id: &DepositId,
    ) -> anyhow::Result<BindResult> {
        let mut inner = self.inner.lock().await;
        let key = (*federation_id, outpoint);
        if let Some(existing) = inner.claims.get(&key) {
            if existing == id {
                return Ok(BindResult::Bound);
            }
            return Ok(BindResult::TakenBy(existing.clone()));
        }
        inner.claims.insert(key, id.clone());
        if let Some(row) = inner.deposits.get_mut(id) {
            row.funding_outpoint = Some(outpoint);
            row.updated_at = now_ms();
        }
        Ok(BindResult::Bound)
    }

    async fn bound_outpoints(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<BTreeSet<bitcoin::OutPoint>> {
        let inner = self.inner.lock().await;
        Ok(inner
            .claims
            .keys()
            .filter(|(fed, _)| fed == federation_id)
            .map(|(_, outpoint)| *outpoint)
            .collect())
    }

    async fn scan_watermark(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<ChronologicalOperationLogKey>> {
        let inner = self.inner.lock().await;
        Ok(inner.watermarks.get(federation_id).copied())
    }

    async fn set_scan_watermark(
        &self,
        federation_id: &FederationId,
        key: ChronologicalOperationLogKey,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        inner.watermarks.insert(*federation_id, key);
        Ok(())
    }

    async fn finish_deposit(
        &self,
        id: &DepositId,
        outpoint: bitcoin::OutPoint,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.funding_outpoint = Some(outpoint);
            row.gross_deposited = Some(gross);
            row.claim_fee = Some(fee);
            row.net_credit = Some(net_credit(gross, fee));
            row.state = DepositState::Done;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn close_deposit_done(&self, id: &DepositId) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.state = DepositState::Done;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    /// Adoption inserts the `Adopted`/`Done` row and binds the claim in one
    /// critical section. If another row holds the claim, nothing is inserted.
    async fn claim_adopt_if_absent(
        &self,
        federation_id: &FederationId,
        outpoint: bitcoin::OutPoint,
        claim: AdoptedClaim,
    ) -> anyhow::Result<BindResult> {
        let mut inner = self.inner.lock().await;
        let key = (*federation_id, outpoint);
        if let Some(existing) = inner.claims.get(&key) {
            return Ok(BindResult::TakenBy(existing.clone()));
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
        inner.claims.insert(key, id.clone());
        inner.deposits.insert(id, row);
        Ok(BindResult::Bound)
    }

    async fn close_deposit_done_if_open(
        &self,
        id: &DepositId,
        gross: bitcoin::Amount,
        fee: bitcoin::Amount,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        let Some(row) = inner.deposits.get_mut(id) else {
            return Ok(());
        };
        if !matches!(
            row.state,
            DepositState::AwaitingAddress | DepositState::AddressShown
        ) {
            return Ok(());
        }
        if row.net_credit.is_none() {
            row.gross_deposited = Some(gross);
            row.claim_fee = Some(fee);
            row.net_credit = Some(net_credit(gross, fee));
        }
        row.state = DepositState::Done;
        row.updated_at = now_ms();
        Ok(())
    }

    async fn cancel_deposit(&self, id: &DepositId) -> anyhow::Result<ApplyResult> {
        let mut inner = self.inner.lock().await;
        let Some(row) = inner.deposits.get_mut(id) else {
            return Ok(ApplyResult::Skipped);
        };
        if !matches!(
            row.state,
            DepositState::AwaitingAddress | DepositState::AddressShown
        ) || row.funding_txid.is_some()
        {
            return Ok(ApplyResult::Skipped);
        }
        row.state = DepositState::Cancelled;
        row.updated_at = now_ms();
        Ok(ApplyResult::Applied)
    }

    async fn insert_withdrawal(&self, row: &WithdrawalRow) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if inner.withdrawals.values().any(|existing| {
            existing.federation_id == row.federation_id
                && matches!(
                    existing.state,
                    WithdrawalState::Quoting | WithdrawalState::Submitted
                )
        }) {
            anyhow::bail!("an in-flight withdrawal already exists for this federation");
        }
        inner.withdrawals.insert(row.id.clone(), row.clone());
        Ok(())
    }

    async fn withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<Option<WithdrawalRow>> {
        let inner = self.inner.lock().await;
        Ok(inner.withdrawals.get(id).cloned())
    }

    async fn withdrawals_in(
        &self,
        states: &[WithdrawalState],
    ) -> anyhow::Result<Vec<WithdrawalRow>> {
        let inner = self.inner.lock().await;
        Ok(inner
            .withdrawals
            .values()
            .filter(|row| states.contains(&row.state))
            .cloned()
            .collect())
    }

    async fn open_withdrawal(
        &self,
        federation_id: &FederationId,
    ) -> anyhow::Result<Option<WithdrawalRow>> {
        let inner = self.inner.lock().await;
        Ok(inner
            .withdrawals
            .values()
            .find(|row| {
                row.federation_id == *federation_id
                    && matches!(
                        row.state,
                        WithdrawalState::Quoting | WithdrawalState::Submitted
                    )
            })
            .cloned())
    }

    async fn set_withdrawal_operation_id(
        &self,
        id: &WithdrawalId,
        operation_id: OperationId,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.operation_id = Some(operation_id);
            row.state = WithdrawalState::Submitted;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_withdrawal_quote(
        &self,
        id: &WithdrawalId,
        quote: FeeQuoteWire,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.quote = Some(quote);
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_withdrawal_failed(&self, id: &WithdrawalId, err: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.state = WithdrawalState::Failed;
            row.last_error = Some(err.to_owned());
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_broadcast(&self, id: &WithdrawalId, txid: bitcoin::Txid) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.txid = Some(txid);
            row.state = WithdrawalState::Broadcast { txid };
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_refunded(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.state = WithdrawalState::Refunded;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn set_failed(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.state = WithdrawalState::Failed;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    async fn cancel_withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<ApplyResult> {
        let mut inner = self.inner.lock().await;
        let Some(row) = inner.withdrawals.get_mut(id) else {
            return Ok(ApplyResult::Skipped);
        };
        if row.state != WithdrawalState::Quoting {
            return Ok(ApplyResult::Skipped);
        }
        row.state = WithdrawalState::Cancelled;
        row.updated_at = now_ms();
        Ok(ApplyResult::Applied)
    }

    async fn scan_for_send(&self, id: &WithdrawalId) -> anyhow::Result<Option<OperationId>> {
        let inner = self.inner.lock().await;
        Ok(inner.withdrawals.get(id).and_then(|row| row.operation_id))
    }
}

pub(crate) fn net_credit(gross: bitcoin::Amount, fee: bitcoin::Amount) -> fedimint_core::Amount {
    let net = gross.checked_sub(fee).unwrap_or(bitcoin::Amount::ZERO);
    fedimint_core::Amount::from_sats(net.to_sat())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn is_open_deposit(state: DepositState) -> bool {
    matches!(
        state,
        DepositState::AwaitingAddress | DepositState::AddressShown
    )
}

pub(crate) fn is_open_withdrawal(state: &WithdrawalState) -> bool {
    matches!(state, WithdrawalState::Quoting | WithdrawalState::Submitted)
}

/// Public journal handle. [`Journal::new`] is DB-backed; [`Journal::in_memory`]
/// is used by tests. The journal methods come from [`JournalBackend`] via
/// `Deref`.
#[derive(Clone)]
pub struct Journal {
    backend: Arc<dyn JournalBackend>,
}

impl Journal {
    pub fn new(db: Database, client_prefix: Vec<u8>) -> Self {
        Self {
            backend: Arc::new(crate::journal_db::DbJournal::new(db, client_prefix)),
        }
    }

    pub fn in_memory() -> Self {
        Self {
            backend: Arc::new(MemoryJournal::default()),
        }
    }
}

impl Deref for Journal {
    type Target = dyn JournalBackend;

    fn deref(&self) -> &Self::Target {
        self.backend.as_ref()
    }
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal").finish_non_exhaustive()
    }
}

impl Default for Journal {
    fn default() -> Self {
        Self::in_memory()
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::hashes::Hash as _;

    use super::*;

    #[tokio::test]
    async fn in_memory_backend_enforces_open_and_claim_invariants() {
        let journal = Journal::in_memory();
        let federation_id = FederationId::dummy();

        let first = DepositRow::requested(
            DepositId::new(),
            federation_id,
            EventLogId::LOG_START,
            bitcoin::Amount::from_sat(10_000),
        );
        let inserted = journal.insert_deposit(&first).await.expect("insert first");
        assert_eq!(inserted.id, first.id);

        let second = DepositRow::requested(
            DepositId::new(),
            federation_id,
            EventLogId::LOG_START.next(),
            bitcoin::Amount::from_sat(20_000),
        );
        let existing = journal
            .insert_deposit(&second)
            .await
            .expect("a second open insert returns the existing row");
        assert_eq!(existing.id, first.id);

        let open = journal
            .open_deposit(&federation_id)
            .await
            .expect("read open")
            .expect("open row");
        assert_eq!(open.id, first.id);

        let outpoint = bitcoin::OutPoint::new(bitcoin::Txid::from_byte_array([0x11; 32]), 0);
        assert_eq!(
            journal
                .claim_bind(&federation_id, outpoint, &first.id)
                .await
                .expect("bind"),
            BindResult::Bound
        );
        assert_eq!(
            journal
                .claim_bind(&federation_id, outpoint, &first.id)
                .await
                .expect("rebind"),
            BindResult::Bound
        );
        assert_eq!(
            journal
                .claim_bind(&federation_id, outpoint, &second.id)
                .await
                .expect("conflict"),
            BindResult::TakenBy(first.id.clone())
        );
        assert!(
            journal
                .bound_outpoints(&federation_id)
                .await
                .expect("bound")
                .contains(&outpoint)
        );

        assert_eq!(
            journal.cancel_deposit(&first.id).await.expect("cancel"),
            ApplyResult::Applied
        );
    }

    #[tokio::test]
    async fn concurrent_insert_deposit_yields_one_open_row() {
        let journal = Journal::in_memory();
        let federation_id = FederationId::dummy();
        let first = DepositRow::requested(
            DepositId::new(),
            federation_id,
            EventLogId::LOG_START,
            bitcoin::Amount::from_sat(10_000),
        );
        let second = DepositRow::requested(
            DepositId::new(),
            federation_id,
            EventLogId::LOG_START.next(),
            bitcoin::Amount::from_sat(20_000),
        );

        let (first_result, second_result) = tokio::join!(
            journal.insert_deposit(&first),
            journal.insert_deposit(&second)
        );
        let first_result = first_result.expect("first insert");
        let second_result = second_result.expect("second insert");
        assert_eq!(
            first_result.id, second_result.id,
            "a concurrent caller must observe the existing open row"
        );

        let open = journal
            .open_deposit(&federation_id)
            .await
            .expect("read open")
            .expect("open row");
        assert_eq!(open.id, first_result.id);

        let open_rows = journal
            .deposits_in(&[DepositState::AwaitingAddress, DepositState::AddressShown])
            .await
            .expect("list open deposits");
        assert_eq!(open_rows.len(), 1, "exactly one open row must exist");
    }
}
