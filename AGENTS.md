# AGENTS.md — durable project context: pegwallet

## Crate layout

```
pegwallet/                      # workspace root
  Cargo.toml                    # workspace definition (edition 2024, rust 1.85 MSRV)
  crates/
    pegwallet-core/             # pure Rust orchestration (no uniffi, no FFI types)
      src/lib.rs
      src/client.rs             # Fedimint Client construction + module registration
      src/bdk.rs                # BDK wallet wrapper + chain sync
      src/pegin.rs              # peg-in flow
      src/pegout.rs             # peg-out flow
      src/journal.rs            # crash-safe write-ahead journal (douglaz pattern)
      src/sync.rs               # concurrent BDK chain task + fedimint federation task
      src/ops.rs                # OperationId mapping / idempotency keys
      error.rs
    pegwallet-ffi/              # thin UniFFI crate (depends on pegwallet-core)
      src/lib.rs                # uniffi::export, PegWallet object
      uniffi/                   # generated bindings via uniffi-bindgen
  crates/
    pegwallet-cli/              # optional dev CLI (not required by spec)
```

## Pinned dependencies

| Crate | Version | Role |
|---|---|---|
| `fedimint-client` | `=0.12.0` | federation client, `Client`, `ClientBuilder` |
| `fedimint-mint-client` | `=0.12.0` | ecash mint module (`MintClientInit`) |
| `fedimint-wallet-client` | `=0.12.0` | on-chain wallet module (`WalletClientInit`) |
| `fedimint-client-module` | `=0.12.0` | `ClientModule` trait, `ClientModuleInit` |
| `fedimint-connectors` | `=0.12.0` | `ConnectorRegistry` |
| `fedimint-core` | `=0.12.0` | `Database`, `OperationId`, `Amount`, `OutPoint`, `FederationId` |
| `bdk_wallet` | `=3.2.0` | on-chain wallet (`Wallet`, `TxBuilder`, persistence) |
| `bdk_chain` | `=0.23.3` | chain data types |
| `bdk_file_store` | `=0.22.0` | optional flat-file persistence |
| `rusqlite` | bundled | optional SQLite persistence |
| `bdk_esplora` / `bdk_electrum` / `bdk_bitcoind_rpc` | `0.22.1` / `0.23.2` / `0.22.0` | chain source (pick one at runtime) |
| `uniffi` | `=0.32.0` | UniFFI (optional feature in fedimint-client; required in ffi crate) |
| `uniffi_bindgen` | `0.32.0` | build-time binding generation |
| `tokio` | `=1.48` | async runtime |
| `anyhow` / `thiserror` | `=1.0.98` / `=2.0.18` | errors |
| `serde` / `serde_json` | `=1.0.219` / `=1.0.149` | journal serialization |
| `tracing` | `=0.1.44` | logging |
| `async-trait` | `=0.1.88` | async traits |
| `bitcoin` | `=0.32.8` | unified across fedimint (`0.32.6`) + bdk (`0.32.8`) |

MSRV: 1.85.0. Edition: 2024.

## Architecture

- One `fedimint_client::Client` per federation, stored in an `Arc<RwLock>` map keyed by `FederationId` (mirrors douglaz `MultiClient`).
- Storage: one shared `Database`; fedimint client `i` lives at `db.with_prefix([0x01] ++ u32_le(db_prefix))`; the app journal lives under a separate prefix `[0x00]` (separate RocksDB memtables — co-location causes `TryAgain`/`WriteConflict`, per douglaz).
- Crash-safe journal: raw byte API + serde_json versioned envelopes (`ROW_VERSION = 1`); rows keyed `[tag] ++ id_bytes`; intent + pending index committed in ONE `dbtx` (`begin_transaction … commit_tx`); idempotency keys + deterministic `OperationId`s; `FederationInfo` registry rows so restart can `ClientBuilder::open` existing partitions rather than re-joining.
- Two long-running tokio sync tasks sharing the journal:
  - BDK chain sync: `Wallet::start_sync_with_revealed_spks()` → `apply_update` → `take_staged` → persist via `WalletPersister`; reconcile withdrawal txids to operations.
  - Fedimint federation sync: `spawn_federation_reconnect()` + `start_executor()` + `handle_events` / `get_event_log_transient_receiver()`; record deposit credits and withdrawal broadcasts in journal.
- Peg-in: `WalletClientModule::allocate_deposit_address_pooled` → BDK pays deposit address → `subscribe_deposit` / `await_primary_bitcoin_module_output` → ecash minted via mint module.
- Peg-out: `get_withdraw_fees` → `withdraw(address, amount, fee, extra_meta)` → `subscribe_withdraw_updates` → BDK chain sync detects withdrawal tx → reconcile in journal.
- Thin UniFFI crate: `#[uniffi::export]` `PegWallet` object (`join_federation`, `balance_sat`, `peg_in_address`, `peg_in`, `peg_out`, `operation_status`, `sync`); errors mapped to a uniffi-compatible `thiserror` enum (not `anyhow`).

## Confirmed API names (cite these, do not invent)

**fedimint-client 0.12.0**: `Client::builder()`, `ClientBuilder::with_module()`, `preview()`, `join()/open()`, `Client::get_balance_for_unit()`, `get_first_module::<M>()`, `get_first_module_arc::<M>()`, `finalize_and_submit_transaction()`, `transaction_updates()`, `await_primary_bitcoin_module_output()`, `subscribe_balance_changes()`, `spawn_federation_reconnect()`, `start_executor()`, `db()`, `executor()`, `handle_events()`, `get_event_log_transient_receiver()`, `chain_id()`, `federation_id()`, `config()`, `OutPointRange`, `TransactionBuilder`, `OperationId`.

**fedimint-wallet-client 0.12.0**: `WalletClientModule::allocate_deposit_address_pooled()`, `allocate_deposit_address_expert_only()`, `safe_allocate_deposit_address()`, `peg_in(PegInRequest)`, `withdraw()`, `get_withdraw_fees()`, `send_fee_quote()`, `max_withdrawable_amount()`, `subscribe_deposit()`, `subscribe_withdraw_updates()`, `create_withdraw_output()`, `WalletClientInit`.

**fedimint-mint-client 0.12.0**: `MintClientModule::reissue_external_notes()`, `send_oob_notes()`, `spend_notes_with_selector()`, `create_output()`, `await_output_finalized()`, `get_note_counts_by_denomination()`, `estimate_spend_all_fees()`, `MintClientInit`.

**bdk_wallet 3.2.0**: `Wallet::create()`, `Wallet::load()`, `reveal_next_address()`, `next_unused_address()`, `list_unspent()`, `get_utxo()`, `get_tx()`, `build_tx()`, `create_psbt()`, `sign_with_signers()`, `finalize_psbt()`, `take_staged()`, `apply_update()`, `balance()`, `calculate_fee()`, `insert_txout()`, `start_sync_with_revealed_spks()`, `apply_update_events()` (`WalletEvent::TxConfirmed`), `WalletPersister`/`PersistedWallet` (rusqlite or bdk_file_store).

**fedimint-clientd (reference daemon)**: `AppState::new(db_path)`, `multimint.register_new(invite_code)`, `multimint.all()`; REST endpoints `/v2/onchain/deposit-address`, `/v2/onchain/await-deposit`, `/v2/onchain/withdraw`.

**douglaz/fedimint-wallets (crash-safe patterns)**: `MultiClient` per-federation clients at `[0x01] ++ u32_le(db_prefix)`, `join_lock`, `active_recoveries` reservation, `open_all` best-effort, `wait_for_all_active_state_machines`, `Client::shutdown()` before replacing a handle; `FedimintJournal` app prefix `[0x00]`, raw byte API + serde_json, atomic status+index transitions; `runtime::Runtime` single actor + watch scheduler.

## Mandatory rule for all future work

**Always read docs.rs + GitHub source before writing code; cite real API names; flag guesses.**

Before touching any crate (fedimint-client, fedimint-mint-client, fedimint-wallet-client, bdk_wallet, uniffi), do this:
1. Open the docs.rs page for the exact pinned version (e.g. https://docs.rs/fedimint-client/0.12.0/).
2. Confirm each function/module/type name you plan to use against the rendered API docs.
3. Where the plan says "guessing at", resolve it by reading the crate source on GitHub (e.g. fedimint/fedimint-client, fedimint/fedimint-clientd, douglaz/fedimint-wallets) before implementing.
4. Never invent function names or module paths — if you can't confirm a name from docs/source, mark it clearly as a guess and either verify or redesign around the confirmed API.

If anything in this AGENTS.md conflicts with the actual crate APIs at build time, update this file to reflect the confirmed reality and proceed from there.
