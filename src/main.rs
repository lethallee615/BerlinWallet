//! Verification stub — NOT the real wallet.
//!
//! The sole purpose of this crate is to *type-check* the API names claimed in
//! AGENTS.md / PLAN.md against the real published crates. Nothing here is ever
//! executed (every function is dead code; `main` only prints). Arguments are
//! supplied with `todo!()` so the compiler resolves names/signatures without us
//! having to build real values.
//!
//! Where a claimed name did not exist, the **real** name is used here instead and
//! the delta is documented in VERIFY.md (we never silently substitute).

#![allow(
    dead_code,
    unused_variables,
    unused_imports,
    unused_mut,
    unused_must_use,
    unreachable_code
)]

use anyhow::Result;

// ---- fedimint-client -------------------------------------------------------
use fedimint_client::module::module::init::ClientModuleInit; // NOTE: claimed path was `module::init`
use fedimint_client::module::transaction::{FeeQuote, TransactionBuilder}; // re-exported from builder
use fedimint_client::{Client, ClientBuilder, ClientHandle, ClientPreview, RootSecret};
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::core::OperationId;
use fedimint_core::db::Database;
use fedimint_core::invite_code::InviteCode;
use fedimint_core::module::AmountUnit;
use fedimint_core::OutPointRange;

// ---- fedimint modules ------------------------------------------------------
use fedimint_mint_client::{
    MintClientInit, MintClientModule, OOBNotes, SelectNotesWithAtleastAmount,
};
// The wallet module is walletv2. The v1 `fedimint-wallet-client` names are
// SUPERSEDED (kept in VERIFY.md for history) and are no longer used here.
use fedimint_walletv2_client::common::config::WalletClientConfig;
use fedimint_walletv2_client::common::{OutputInfo, TxInfo};
use fedimint_walletv2_client::events::{
    ReceivePaymentEvent, ReceivePaymentUpdateEvent, SendPaymentEvent, SendPaymentUpdateEvent,
};
use fedimint_walletv2_client::{
    FinalReceiveOperationState, FinalSendOperationState, ReceiveError, ReceiveMeta, SendError,
    SendMeta, WalletClientInit, WalletClientModule, WalletClientStateMachines, WalletOperationMeta,
};

// ---- BDK -------------------------------------------------------------------
use bdk_esplora::esplora_client::AsyncClient; // re-exported by bdk_esplora
use bdk_esplora::EsploraAsyncExt; // NOTE: `EsploraClient` does not exist
use bdk_wallet::rusqlite::Connection;
use bdk_wallet::{KeychainKind, Wallet};

// ---------------------------------------------------------------------------
// fedimint-client: construction + module registration
// ---------------------------------------------------------------------------

fn _assert_is_module_init<M: ClientModuleInit>() {}

async fn _federation_api(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,
    invite: InviteCode,
) -> Result<()> {
    let mut builder: ClientBuilder = Client::builder().await?;
    builder.with_module(MintClientInit);
    builder.with_module(WalletClientInit::default());
    let preview: ClientPreview = builder.preview(connectors.clone(), &invite).await?;
    let _client: ClientHandle = preview.join(db.clone(), root_secret).await?;

    // Database partition split: journal [0x00], per-federation clients [0x01, ..]
    let _journal_db = db.with_prefix(vec![0x00]);
    Ok(())
}

async fn _client_methods(client: &Client) -> Result<()> {
    let _ = client.get_balance_for_unit(todo!()).await?;
    let _ = client.get_first_module::<WalletClientModule>()?;
    let _ = client.get_first_module::<MintClientModule>()?;
    let _ = client.get_first_module_arc::<MintClientModule>()?;

    let _ = client.transaction_updates(todo!()).await;
    client.await_primary_bitcoin_module_output(todo!(), todo!()).await?;
    let _ = client.subscribe_balance_changes(todo!()).await;

    client.spawn_federation_reconnect();
    let _ = client.chain_id().await?;
    let _ = client.federation_id();
    let _ = client.db();
    let _ = client.get_event_log_transient_receiver();

    let _: OutPointRange = client
        .finalize_and_submit_transaction(
            todo!(),
            "operation-type",
            |_out_point: OutPointRange| serde_json::Value::Null,
            todo!(),
        )
        .await?;
    Ok(())
}

fn _start_executor(client: &std::sync::Arc<Client>) {
    client.start_executor();
}

async fn _client_event_handler(client: &Client) -> Result<()> {
    let tracker = client.built_in_application_event_log_tracker();
    client
        .handle_events(tracker, |_dbtx, _event| async { Ok(()) })
        .await
}

// ---------------------------------------------------------------------------
// fedimint-walletv2-client: receive (peg-in) / send (peg-out)
// ---------------------------------------------------------------------------

async fn _walletv2_client_module(client: &Client, w: &WalletClientModule) -> Result<()> {
    // ---- receive (peg-in) -------------------------------------------------
    // `receive()` returns ONLY an `Address`: no Result, no OperationId.
    // The deposit becomes an operation only once the federation finds it.
    // Race-free observation: read the event-log position BEFORE asking for
    // the address, then wait from that position.
    let position = client.get_next_event_log_id().await;
    let _address: bitcoin::Address = w.receive().await;
    let (_final, _next_position): (FinalReceiveOperationState, _) =
        w.await_receive(position).await?;

    // Lower-level per-operation observation (id obtained from event log / meta).
    let _: FinalReceiveOperationState = w.await_final_receive_operation_state(todo!()).await?;

    // ---- send (peg-out) ---------------------------------------------------
    let fee: Option<bitcoin::Amount> = None;
    let op: OperationId = w
        .send(todo!(), todo!(), fee, serde_json::Value::Null)
        .await?;
    let _: FinalSendOperationState = w.await_final_send_operation_state(op).await?;

    // fees / limits
    let _: bitcoin::Amount = w.receive_fee().await?;
    let _: bitcoin::Amount = w.send_fee().await?;
    let _: FeeQuote = w.send_fee_quote(todo!()).await?;
    let _: bitcoin::Amount = w.max_sendable_amount(todo!(), todo!()).await?;

    // chain views + output info
    let _: Vec<TxInfo> = w.tx_chain().await?;
    let _: Vec<TxInfo> = w.pending_tx_chain().await?;
    let _: Vec<OutputInfo> = todo!(); // public via `common`
    // `output_info_slice(start, end)` is an internal `WalletFederationApi`
    // method (private `mod api`), not public on `WalletClientModule`; the
    // public representation is the endpoint constant:
    let _endpoint = fedimint_walletv2_common::endpoint_constants::OUTPUT_INFO_SLICE_ENDPOINT;

    // config / operation meta / state machines / events
    let _: WalletClientConfig = todo!();
    let _: WalletOperationMeta = todo!();
    let _: SendMeta = todo!();
    let _: ReceiveMeta = todo!();
    let _: WalletClientStateMachines = todo!();
    let _: SendPaymentEvent = todo!();
    let _: SendPaymentUpdateEvent = todo!();
    let _: ReceivePaymentEvent = todo!();
    let _: ReceivePaymentUpdateEvent = todo!();
    let _: Option<SendError> = None;
    let _: Option<ReceiveError> = None;

    Ok(())
}

// ---------------------------------------------------------------------------
// fedimint-mint-client: ecash
// ---------------------------------------------------------------------------

async fn _mint_client_module(m: &MintClientModule) -> Result<()> {
    let _: OperationId = m
        .reissue_external_notes(todo!(), serde_json::Value::Null)
        .await?;
    let _ = m.subscribe_reissue_external_notes(todo!()).await?;
    let _: OOBNotes = m.send_oob_notes(todo!(), serde_json::Value::Null).await?;
    let (_op, _notes): (OperationId, OOBNotes) = m
        .spend_notes_with_selector(
            &SelectNotesWithAtleastAmount,
            todo!(),
            todo!(),
            todo!(),
            serde_json::Value::Null,
        )
        .await?;
    let _ = m.create_output(todo!(), todo!(), todo!(), todo!()).await;
    m.await_output_finalized(todo!(), todo!()).await?;
    let _ = m.get_note_counts_by_denomination(todo!()).await;
    let _ = m.estimate_spend_all_fees().await;
    Ok(())
}

// ---------------------------------------------------------------------------
// BDK: wallet, PSBT, fee, persistence, chain sync
// ---------------------------------------------------------------------------

fn _bdk_wallet(w: &mut Wallet, conn: &mut Connection) -> Result<()> {
    // Construction (CreateParams / LoadParams builders)
    let _ = Wallet::create("wpkh(<xprv>/0/*)", "wpkh(<xprv>/1/*)");
    let _ = Wallet::load();

    // Addresses / utxos / txs
    let _ = w.reveal_next_address(KeychainKind::External);
    let _ = w.list_unspent();
    let _ = w.get_utxo(todo!());
    let _ = w.get_tx(todo!());
    let _ = w.balance();

    // PSBT + fee  (require bdk-tx feature AND --cfg bdk_wallet_unstable)
    let _ = w.build_tx();
    let (_psbt, _finalizer) = w.create_psbt(todo!())?;
    w.insert_txout(todo!(), todo!());
    let _ = w.calculate_fee(todo!())?;
    let _ = w.sign_with_signers(todo!(), todo!(), todo!())?;
    let _ = w.finalize_psbt(todo!(), todo!())?;

    // Chain sync / staging / persistence
    let _ = w.apply_update(bdk_wallet::Update::default())?;
    let _ = w.take_staged();
    let _ = w.start_sync_with_revealed_spks();
    let _ = w.start_full_scan();
    Ok(())
}

async fn _esplora<C: EsploraAsyncExt + Send + Sync>(client: &C, w: &Wallet) {
    // NOTE: there is no `EsploraClient` and no `scan`; the real trait is
    // `EsploraAsyncExt` with `full_scan` (full) and `sync` (partial).
    let full = w.start_full_scan().build();
    let _ = client.full_scan(full, 20usize, 2usize).await;
    let partial = w.start_sync_with_revealed_spks().build();
    let _ = client.sync(partial, 2usize).await;
    let _client: Option<AsyncClient<()>> = None; // type exists, re-exported
}

#[tokio::main]
async fn main() -> Result<()> {
    println!(
        "pegwallet verification stub: API references type-checked; no peg logic executed."
    );
    Ok(())
}
