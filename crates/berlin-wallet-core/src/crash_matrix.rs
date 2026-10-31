//! Deterministic crash/restart matrix.
//!
//! Each test performs a partial operation, drops the journal (the process
//! crash), reopens the same database, and asserts the invariant that must hold
//! after restart. The in-memory `MemDatabase` is shared through `Database`'s
//! `Arc`, so reopening models a process restart without a live federation.
//!
//! The debug-only `fault::crash_point` hooks cover the live process aborts:
//!
//! - `after_deposit_address` / `after_deposit_broadcast` / `after_deposit_done`
//! - `after_send`
//! - `after_broadcast`
//!
//! The one window that cannot be reproduced with the journal alone is the
//! gap between a real BDK network broadcast and the journal's `funding_txid`
//! write. It is explicitly devimint-only and listed in `DEVIMINT_ONLY_WINDOWS`.

use std::str::FromStr;

use bitcoin::hashes::Hash as _;
use fedimint_core::config::FederationId;
use fedimint_core::core::OperationId;
use fedimint_core::db::Database;
use fedimint_core::db::mem_impl::MemDatabase;
use fedimint_core::module::registry::ModuleDecoderRegistry;
use fedimint_eventlog::EventLogId;
use fedimint_walletv2_client::FinalSendOperationState;

use crate::journal::{
    ApplyResult, BindResult, DepositRow, DepositState, Journal, WithdrawalRow, WithdrawalState,
};
use crate::ops::{DepositId, WithdrawalId};
use crate::withdrawal::{AdoptedSend, adopt_send_claims};

/// Valid regtest address used by the journal-level deposit transitions.
const ADDRESS: &str = "bcrt1qdpejjzcmhslvkf0katf7wnjumg8qlj50rxarzj66tfl0vl8cgdtsyy7fjf";

/// The documented windows and their CI coverage.
pub(crate) const WINDOWS: &[&str] = &[
    "address_shown_before_funding",
    "funding_broadcast_before_claim",
    "crash_during_confirmation",
    "crash_after_done",
    "after_send_before_operation_id",
    "after_final_send_before_broadcast",
    "after_broadcast_before_bdk_and_journal_persistence",
    "change_output_finalization_timeout",
];

/// Windows that need a live chain/federation and are therefore not reproduced
/// by the in-memory journal suite.
pub(crate) const DEVIMINT_ONLY_WINDOWS: &[&str] =
    &["after_broadcast_before_bdk_and_journal_persistence"];

struct Store {
    db: Database,
}

impl Store {
    fn new() -> Self {
        Self {
            db: Database::new(MemDatabase::new(), ModuleDecoderRegistry::default()),
        }
    }

    fn journal(&self) -> Journal {
        Journal::new(self.db.clone(), vec![0x00])
    }
}

fn txid(byte: u8) -> bitcoin::Txid {
    bitcoin::Txid::from_byte_array([byte; 32])
}

fn outpoint(byte: u8, vout: u32) -> bitcoin::OutPoint {
    bitcoin::OutPoint::new(txid(byte), vout)
}

fn requested(federation_id: FederationId, amount_sat: u64) -> DepositRow {
    DepositRow::requested(
        DepositId::new(),
        federation_id,
        EventLogId::LOG_START,
        bitcoin::Amount::from_sat(amount_sat),
    )
}

async fn show_address(journal: &Journal, id: &DepositId) -> ApplyResult {
    let address = bitcoin::Address::from_str(ADDRESS).expect("valid regtest address");
    journal
        .set_deposit_address_if(id, address, DepositState::AwaitingAddress)
        .await
        .expect("address transition")
}

#[test]
fn matrix_and_devimint_exceptions_are_explicit() {
    assert_eq!(WINDOWS.len(), 8);
    assert!(WINDOWS.contains(&"after_broadcast_before_bdk_and_journal_persistence"));
    assert_eq!(
        DEVIMINT_ONLY_WINDOWS,
        &["after_broadcast_before_bdk_and_journal_persistence"]
    );
}

#[tokio::test]
async fn address_shown_before_funding_survives_restart() {
    let store = Store::new();
    let federation_id = FederationId::dummy();
    let journal = store.journal();
    let row = journal
        .insert_deposit(&requested(federation_id, 10_000))
        .await
        .expect("insert");
    assert_eq!(show_address(&journal, &row.id).await, ApplyResult::Applied);

    // Crash after the address write, before funding.
    drop(journal);
    let journal = store.journal();

    let row = journal.deposit(&row.id).await.expect("read").expect("row");
    assert_eq!(row.state, DepositState::AddressShown);
    assert!(row.funding_txid.is_none());
    assert_eq!(
        journal
            .open_deposit(&federation_id)
            .await
            .expect("open")
            .expect("open row")
            .id,
        row.id,
        "the address-shown row is the deliberate resumable open row"
    );

    // A retry must reuse it, not create a second open slot.
    let second = requested(federation_id, 20_000);
    assert_eq!(
        journal
            .insert_deposit(&second)
            .await
            .expect("retry insert")
            .id,
        row.id
    );
    assert_eq!(journal.deposits_all().await.expect("rows").len(), 1);

    assert_eq!(
        journal.cancel_deposit(&row.id).await.expect("cancel"),
        ApplyResult::Applied
    );
    assert!(
        journal
            .open_deposit(&federation_id)
            .await
            .expect("open")
            .is_none(),
        "cancelling the pre-funding row must release the slot"
    );
}

#[tokio::test]
async fn funding_broadcast_before_claim_survives_restart_without_rebroadcast() {
    let store = Store::new();
    let federation_id = FederationId::dummy();
    let journal = store.journal();
    let row = journal
        .insert_deposit(&requested(federation_id, 10_000))
        .await
        .expect("insert");
    show_address(&journal, &row.id).await;
    let funding = outpoint(0x21, 1);
    journal
        .set_deposit_funding_txid(&row.id, funding.txid)
        .await
        .expect("txid");
    journal
        .set_deposit_funding_outpoint(&row.id, funding)
        .await
        .expect("outpoint");

    // Crash after broadcast and before the claim.
    drop(journal);
    let journal = store.journal();

    assert_eq!(
        journal
            .deposit_funding_txid(&row.id)
            .await
            .expect("funding txid"),
        Some(funding.txid),
        "fund_deposit's early return is this recorded txid; a retry must not rebuild"
    );
    let row = journal.deposit(&row.id).await.expect("read").expect("row");
    assert_eq!(row.state, DepositState::AddressShown);
    assert_eq!(row.funding_outpoint, Some(funding));
    assert_eq!(
        journal
            .open_deposit(&federation_id)
            .await
            .expect("open")
            .expect("open row")
            .id,
        row.id
    );

    // Re-writing the same txid is idempotent.
    journal
        .set_deposit_funding_txid(&row.id, funding.txid)
        .await
        .expect("idempotent txid");
    assert_eq!(
        journal
            .deposit_funding_txid(&row.id)
            .await
            .expect("funding txid"),
        Some(funding.txid)
    );
}

#[tokio::test]
async fn confirmation_restart_claims_exactly_once() {
    let store = Store::new();
    let federation_id = FederationId::dummy();
    let journal = store.journal();
    let row = journal
        .insert_deposit(&requested(federation_id, 50_000))
        .await
        .expect("insert");
    show_address(&journal, &row.id).await;
    let funding = outpoint(0x22, 0);
    journal
        .set_deposit_funding_outpoint(&row.id, funding)
        .await
        .expect("funding");
    let operation_id = OperationId::new_random();
    let gross = bitcoin::Amount::from_sat(50_000);
    let claim_fee = bitcoin::Amount::from_sat(324);
    let operation_fee = fedimint_core::Amount::from_msats(105_696);

    // Crash after the claim was accepted and bound, before the row was closed.
    assert_eq!(
        journal
            .claim_bind(&federation_id, funding, &row.id)
            .await
            .expect("bind"),
        BindResult::Bound
    );
    drop(journal);
    let journal = store.journal();

    assert_eq!(
        journal
            .claim_bind(&federation_id, funding, &row.id)
            .await
            .expect("rebind after restart"),
        BindResult::Bound
    );
    journal
        .finish_deposit(
            &row.id,
            operation_id,
            funding,
            gross,
            claim_fee,
            Some(operation_fee),
        )
        .await
        .expect("finish");

    // Crash after Done, then let a second watcher replay the same claim.
    drop(journal);
    let journal = store.journal();
    assert_eq!(
        journal
            .claim_bind(&federation_id, funding, &row.id)
            .await
            .expect("idempotent bind"),
        BindResult::Bound
    );
    journal
        .finish_deposit(
            &row.id,
            operation_id,
            funding,
            gross,
            claim_fee,
            Some(operation_fee),
        )
        .await
        .expect("idempotent finish");

    let row = journal.deposit(&row.id).await.expect("read").expect("row");
    let expected = fedimint_core::Amount::from_msats(49_570_304);
    assert_eq!(row.state, DepositState::Done);
    assert_eq!(row.net_credit, Some(expected));
    assert_eq!(
        journal
            .bound_outpoints(&federation_id)
            .await
            .expect("bound")
            .len(),
        1,
        "one funding outpoint must have one claim owner"
    );

    // A new row cannot steal the same outpoint or add a second credit.
    let later = journal
        .insert_deposit(&requested(federation_id, 10_000))
        .await
        .expect("new open row");
    assert_eq!(
        journal
            .claim_bind(&federation_id, funding, &later.id)
            .await
            .expect("steal attempt"),
        BindResult::TakenBy(row.id.clone())
    );
    assert_eq!(
        journal
            .deposit(&row.id)
            .await
            .expect("read")
            .expect("row")
            .net_credit,
        Some(expected)
    );
    assert_eq!(journal.deposits_all().await.expect("rows").len(), 2);
}

#[tokio::test]
async fn crash_after_done_keeps_one_credit_and_releases_the_slot() {
    let store = Store::new();
    let federation_id = FederationId::dummy();
    let journal = store.journal();
    let row = journal
        .insert_deposit(&requested(federation_id, 10_000))
        .await
        .expect("insert");
    show_address(&journal, &row.id).await;
    let funding = outpoint(0x23, 0);
    journal
        .set_deposit_funding_outpoint(&row.id, funding)
        .await
        .expect("funding");
    journal
        .finish_deposit(
            &row.id,
            OperationId::new_random(),
            funding,
            bitcoin::Amount::from_sat(10_000),
            bitcoin::Amount::from_sat(100),
            Some(fedimint_core::Amount::ZERO),
        )
        .await
        .expect("finish");

    // Crash after the terminal write.
    drop(journal);
    let journal = store.journal();

    let row = journal.deposit(&row.id).await.expect("read").expect("row");
    assert_eq!(row.state, DepositState::Done);
    assert_eq!(
        row.net_credit,
        Some(fedimint_core::Amount::from_msats(9_900_000))
    );
    assert!(
        journal
            .open_deposit(&federation_id)
            .await
            .expect("open")
            .is_none(),
        "Done must release the single-flight slot"
    );
    assert_eq!(journal.deposits_all().await.expect("rows").len(), 1);
    let done = journal
        .deposits_in(&[DepositState::Done])
        .await
        .expect("done rows");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].id, row.id);

    // The slot is usable again.
    let next = journal
        .insert_deposit(&requested(federation_id, 20_000))
        .await
        .expect("new open row");
    assert_ne!(next.id, row.id);
}

#[tokio::test]
async fn send_after_send_before_operation_id_is_adopted_without_resend() {
    let store = Store::new();
    let federation_id = FederationId::dummy();
    let journal = store.journal();
    let id = WithdrawalId("after-send-before-operation-id".to_owned());
    let row = WithdrawalRow::new(
        id.clone(),
        federation_id,
        ADDRESS.to_owned(),
        bitcoin::Amount::from_sat(10_000),
    );
    journal
        .insert_withdrawal(&row)
        .await
        .expect("insert quoting row");

    // Crash after send() returned but before set_withdrawal_operation_id.
    drop(journal);
    let journal = store.journal();
    let operation_id = OperationId::new_random();
    let withdrawal_txid = txid(0x24);
    let send = AdoptedSend {
        operation_id,
        final_state: Some(FinalSendOperationState::Success(withdrawal_txid)),
        address: row.address.clone(),
        value: row.value,
        fee: bitcoin::Amount::from_sat(234),
        custom_meta: serde_json::json!({ "id": id.0 }),
    };

    adopt_send_claims(&journal, federation_id, vec![send.clone()])
        .await
        .expect("adopt");
    let recovered = journal.withdrawal(&id).await.expect("read").expect("row");
    assert_eq!(
        recovered.state,
        WithdrawalState::Broadcast {
            txid: withdrawal_txid
        }
    );
    assert_eq!(recovered.operation_id, Some(operation_id));
    assert_eq!(journal.withdrawals_all().await.expect("rows").len(), 1);
    assert_eq!(
        journal.scan_for_send(&id).await.expect("op index"),
        Some(operation_id)
    );

    // A second adoption of the same operation must not create a second row.
    adopt_send_claims(&journal, federation_id, vec![send])
        .await
        .expect("re-adopt");
    assert_eq!(journal.withdrawals_all().await.expect("rows").len(), 1);
}

#[tokio::test]
async fn final_send_state_before_broadcast_restart_sets_broadcast_once() {
    let store = Store::new();
    let federation_id = FederationId::dummy();
    let journal = store.journal();
    let id = WithdrawalId("final-state-before-broadcast".to_owned());
    let row = WithdrawalRow::new(
        id.clone(),
        federation_id,
        ADDRESS.to_owned(),
        bitcoin::Amount::from_sat(10_000),
    );
    journal.insert_withdrawal(&row).await.expect("insert");
    let operation_id = OperationId::new_random();
    journal
        .set_withdrawal_operation_id(&id, operation_id)
        .await
        .expect("persist operation id");

    // Crash after the final send state, before set_broadcast.
    drop(journal);
    let journal = store.journal();
    let recovered = journal.withdrawal(&id).await.expect("read").expect("row");
    assert_eq!(recovered.state, WithdrawalState::Submitted);
    assert_eq!(recovered.operation_id, Some(operation_id));
    assert_eq!(
        journal
            .withdrawals_in(&[WithdrawalState::Submitted])
            .await
            .expect("submitted")
            .len(),
        1,
        "the Submitted row is what reconciliation retries; it is not re-sent"
    );

    let withdrawal_txid = txid(0x25);
    journal
        .set_broadcast(&id, withdrawal_txid)
        .await
        .expect("broadcast");
    journal
        .set_broadcast(&id, withdrawal_txid)
        .await
        .expect("idempotent broadcast");
    let recovered = journal.withdrawal(&id).await.expect("read").expect("row");
    assert_eq!(
        recovered.state,
        WithdrawalState::Broadcast {
            txid: withdrawal_txid
        }
    );
    assert_eq!(journal.withdrawals_all().await.expect("rows").len(), 1);
    assert!(
        journal
            .open_withdrawal(&federation_id)
            .await
            .expect("open")
            .is_none(),
        "Broadcast must release the withdrawal slot"
    );
}

#[tokio::test]
async fn change_output_timeout_keeps_submitted_for_retry() {
    let store = Store::new();
    let federation_id = FederationId::dummy();
    let journal = store.journal();
    let id = WithdrawalId("change-timeout".to_owned());
    journal
        .insert_withdrawal(&WithdrawalRow::new(
            id.clone(),
            federation_id,
            ADDRESS.to_owned(),
            bitcoin::Amount::from_sat(10_000),
        ))
        .await
        .expect("insert");
    journal
        .set_withdrawal_operation_id(&id, OperationId::new_random())
        .await
        .expect("operation id");

    // A restart after the bounded change wait timed out must leave the row
    // Submitted; the reconciliation query must still find it.
    drop(journal);
    let journal = store.journal();
    assert_eq!(
        journal
            .withdrawal(&id)
            .await
            .expect("read")
            .expect("row")
            .state,
        WithdrawalState::Submitted
    );
    assert_eq!(
        journal
            .withdrawals_in(&[WithdrawalState::Submitted])
            .await
            .expect("retry set")
            .len(),
        1
    );

    // A later successful retry writes Broadcast exactly once.
    let withdrawal_txid = txid(0x26);
    journal
        .set_broadcast(&id, withdrawal_txid)
        .await
        .expect("retry broadcast");
    assert_eq!(
        journal
            .withdrawal(&id)
            .await
            .expect("read")
            .expect("row")
            .state,
        WithdrawalState::Broadcast {
            txid: withdrawal_txid
        }
    );
    assert_eq!(journal.withdrawals_all().await.expect("rows").len(), 1);
}

#[test]
#[ignore = "devimint-only: requires a real BDK broadcast and a crash before BDK/journal persistence"]
fn after_broadcast_before_bdk_and_journal_persistence() {
    // This is the one window the in-memory journal cannot reproduce: the
    // transaction is on the network before either BDK's own persistence or the
    // journal's `funding_txid` write. See API-VERIFICATION.md §53. The adjacent
    // post-persistence live window is §27.2; this narrower boundary is
    // intentionally documented as a devimint-only gap, and the CI matrix starts
    // from the recorded funding txid above.
}
