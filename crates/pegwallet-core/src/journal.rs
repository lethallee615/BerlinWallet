use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fedimint_client::module::transaction::FeeQuote;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_eventlog::EventLogId;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::ops::{DepositId, WithdrawalId};

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
    /// The scaffold persists a debug rendering; `FeeQuote` is not `Serialize`
    /// in 0.12.0 (see COMPILE.md). A production journal would use a typed
    /// wire form.
    pub quote: Option<String>,
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
    pub operation_id: OperationId,
    pub value: bitcoin::Amount,
    pub fee: bitcoin::Amount,
}

#[derive(Debug, Default)]
struct JournalInner {
    deposits: BTreeMap<DepositId, DepositRow>,
    withdrawals: BTreeMap<WithdrawalId, WithdrawalRow>,
    claims: BTreeMap<(FederationId, bitcoin::OutPoint), DepositId>,
}

/// In-memory scaffold of the journal. The state transitions and the
/// `claim_bind` invariant are real; the production crate will back the same
/// methods with fedimint `Database` raw rows and dbtx commits.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    inner: Arc<Mutex<JournalInner>>,
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn open_deposit(
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

    pub async fn insert_deposit(&self, row: &DepositRow) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if inner.deposits.values().any(|existing| {
            existing.federation_id == row.federation_id
                && matches!(
                    existing.state,
                    DepositState::AwaitingAddress | DepositState::AddressShown
                )
        }) {
            anyhow::bail!("an open deposit already exists for this federation");
        }
        inner.deposits.insert(row.id.clone(), row.clone());
        Ok(())
    }

    pub async fn deposit(&self, id: &DepositId) -> anyhow::Result<Option<DepositRow>> {
        let inner = self.inner.lock().await;
        Ok(inner.deposits.get(id).cloned())
    }

    pub async fn deposits_in(
        &self,
        states: &[DepositState],
    ) -> anyhow::Result<Vec<DepositRow>> {
        let inner = self.inner.lock().await;
        Ok(inner
            .deposits
            .values()
            .filter(|row| states.contains(&row.state))
            .cloned()
            .collect())
    }

    pub async fn deposit_funding_txid(
        &self,
        id: &DepositId,
    ) -> anyhow::Result<Option<bitcoin::Txid>> {
        let inner = self.inner.lock().await;
        Ok(inner.deposits.get(id).and_then(|row| row.funding_txid))
    }

    pub async fn set_deposit_quoted_fee(
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

    pub async fn set_deposit_funding_txid(
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

    pub async fn set_deposit_address_if(
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
        row.address = Some(address.assume_checked().to_string());
        row.state = DepositState::AddressShown;
        row.updated_at = now_ms();
        Ok(ApplyResult::Applied)
    }

    pub async fn fail_deposit_if(
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

    pub async fn fail_deposit(&self, id: &DepositId, err: &str) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.state = DepositState::Failed;
            row.last_error = Some(err.to_owned());
            row.updated_at = now_ms();
        }
        Ok(())
    }

    pub async fn set_deposit_aborted(
        &self,
        id: &DepositId,
        next: EventLogId,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.next_position = Some(next);
            row.state = DepositState::AddressShown;
            row.last_error = Some("claim aborted".to_owned());
            row.updated_at = now_ms();
        }
        Ok(())
    }

    pub async fn set_deposit_next_position(
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
    pub async fn claim_bind(
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

    pub async fn finish_deposit(
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

    pub async fn close_deposit_done(&self, id: &DepositId) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.deposits.get_mut(id) {
            row.state = DepositState::Done;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    /// Adoption inserts the `Adopted`/`Done` row and binds the claim in one
    /// critical section. If another row holds the claim, nothing is inserted.
    pub async fn claim_adopt_if_absent(
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

    pub async fn close_deposit_done_if_open(
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

    pub async fn cancel_deposit(&self, id: &DepositId) -> anyhow::Result<ApplyResult> {
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

    pub async fn insert_withdrawal(&self, row: &WithdrawalRow) -> anyhow::Result<()> {
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

    pub async fn withdrawal(
        &self,
        id: &WithdrawalId,
    ) -> anyhow::Result<Option<WithdrawalRow>> {
        let inner = self.inner.lock().await;
        Ok(inner.withdrawals.get(id).cloned())
    }

    pub async fn open_withdrawal(
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

    pub async fn set_withdrawal_operation_id(
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

    pub async fn set_withdrawal_failed(
        &self,
        id: &WithdrawalId,
        err: &str,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.state = WithdrawalState::Failed;
            row.last_error = Some(err.to_owned());
            row.updated_at = now_ms();
        }
        Ok(())
    }

    pub async fn set_broadcast(
        &self,
        id: &WithdrawalId,
        txid: bitcoin::Txid,
    ) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.txid = Some(txid);
            row.state = WithdrawalState::Broadcast { txid };
            row.updated_at = now_ms();
        }
        Ok(())
    }

    pub async fn set_refunded(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.state = WithdrawalState::Refunded;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    pub async fn set_failed(&self, id: &WithdrawalId) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(row) = inner.withdrawals.get_mut(id) {
            row.state = WithdrawalState::Failed;
            row.updated_at = now_ms();
        }
        Ok(())
    }

    pub async fn cancel_withdrawal(&self, id: &WithdrawalId) -> anyhow::Result<ApplyResult> {
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

    pub async fn scan_for_send(
        &self,
        id: &WithdrawalId,
    ) -> anyhow::Result<Option<OperationId>> {
        let inner = self.inner.lock().await;
        Ok(inner.withdrawals.get(id).and_then(|row| row.operation_id))
    }
}

fn net_credit(gross: bitcoin::Amount, fee: bitcoin::Amount) -> fedimint_core::Amount {
    let net = gross.checked_sub(fee).unwrap_or(bitcoin::Amount::ZERO);
    fedimint_core::Amount::from_sats(net.to_sat())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// Keep `FeeQuote` referenced so the documented non-serializable field is
// explicit to readers of this scaffold.
#[allow(dead_code)]
fn _quote_is_not_serde(_quote: &FeeQuote) {}
