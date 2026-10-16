# PLAN: Fedimint ↔ BDK Peg Wallet crate (UniFFI-exposable)

## Sources consulted (and cited)

| Source | What it gave me |
|---|---|
| https://docs.rs/fedimint-client/0.12.0 | Full `Client` / `ClientBuilder` / `ClientPreview` / `ClientHandle` API surface |
| https://github.com/fedimint/fedimint-clientd | `main.rs` entrypoint, `AppState`, `multimint` usage (`register_new`, `all`), REST endpoint shape (`/v2/onchain/deposit-address`, `await-deposit`, `withdraw`) |
| https://docs.rs/fedimint-mint-client/0.12.0 | `MintClientModule` methods (reissue/spend ecash) |
| https://docs.rs/fedimint-wallet-client/0.12.0 | `WalletClientModule` methods (deposit address, `peg_in`, `peg_out`, `withdraw`, fee quotes) |
| https://docs.rs/bdk-wallet/3.2.0 | `Wallet` API (create/load, addresses, `build_tx`, `apply_update`/`take_staged`/`persist`, chain sync) |
| https://github.com/douglaz/fedimint-wallets | `wallet-fedimint`: `MultiClient` (per-federation clients at `[0x01]++u32_le(db_prefix)`), `FedimintJournal` (app prefix `[0x00]`, raw-byte + serde_json rows, atomic intent+index transitions), `executor`/`runtime` (single-actor orchestrator, watch scheduler, recovery reservations, join lock) |

All three target crates are at **0.12.0** (released 27 Aug 2026) and **bdk_wallet 3.2.0** (released 20 Sep 2026), so they are contemporaries.

---

## 1. Crate layout (workspace)

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
      uniffi/                   # generated bindings (TS/Swift/Kotlin/etc. via uniffi-bindgen)
  crates/
    pegwallet-cli/              # optional dev CLI (not required by spec)
```

---

## 2. Version pins (for Cargo.toml)

| Crate | Version | Role |
|---|---|---|
| `fedimint-client` | `=0.12.0` | federation client, `Client`, `ClientBuilder` |
| `fedimint-mint-client` | `=0.12.0` | ecash mint module client (`MintClientInit`) |
| `fedimint-wallet-client` | `=0.12.0` | on-chain wallet module client (`WalletClientInit`) |
| `fedimint-client-module` | `=0.12.0` | `ClientModule` trait, `ClientModuleInit` |
| `fedimint-connectors` | `=0.12.0` | `ConnectorRegistry` |
| `fedimint-core` | `=0.12.0` | `Database`, `OperationId`, `Amount`, `OutPoint`, `FederationId` |
| `bdk_wallet` | `=3.2.0` | on-chain wallet (`Wallet`, `TxBuilder`, persistence) |
| `bdk_chain` | `=0.23.3` | chain data types (`Update`, `CheckPoint`) |
| `bdk_file_store` | `=0.22.0` | optional flat-file persistence for BDK ChangeSet |
| `rusqlite` | bundled with bdk | optional SQLite persistence for BDK |
| `bdk_esplora` / `bdk_electrum` / `bdk_bitcoind_rpc` | `0.22.1` / `0.23.2` / `0.22.0` | chain source (pick one at runtime) |
| `uniffi` | `=0.32.0` | UniFFI (optional feature in core; required in ffi) |
| `uniffi_bindgen` | `0.32.0` | build-time binding generation |
| `tokio` | `=1.48` | async runtime (fedimint pins `1.48`; bdk dev uses `1.38`, fine) |
| `anyhow` | `=1.0.98` | errors |
| `thiserror` | `=2.0.18` | typed errors |
| `serde` / `serde_json` | `=1.0.219` / `=1.0.149` | journal serialization |
| `tracing` | `=0.1.44` | logging |
| `async-trait` | `=0.1.88` | async traits (fedimint ecosystem) |
| `bitcoin` | `=0.32.8` | note: fedimint 0.12.0 depends on `0.32.6`, bdk on `0.32.8` — Cargo unifies to `0.32.8`, patch-level only |

**MSRV:** `1.85.0` (bdk_wallet's stated MSRV).
**Edition:** `2024`.

**Risk to flag:** `fedimint-client` enables `uniffi` only behind an optional feature — verify the `uniffi` feature is actually needed/available in `0.12.0` (it's listed as optional in its dependencies). The ffi crate will enable it.

---

## 3. Client construction & module registration (modeled on `fedimint-clientd`)

`fedimint-clientd` constructs its clients via `multimint` (`state.multimint.register_new(invite_code)`, `state.multimint.all()`). We will do the equivalent directly with `fedimint_client`:

```rust
// pegwallet-core/src/client.rs
use fedimint_client::{Client, ClientBuilder, ClientHandle, ClientHandleArc};
use fedimint_client::module::init::ClientModuleInit;   // re-exported as fedimint_client::module
use fedimint_mint_client::MintClientInit;
use fedimint_wallet_client::WalletClientInit;
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::db::Database;

pub async fn build_client(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,   // derived from BIP39 mnemonic
    invite: InviteCode,
) -> anyhow::Result<ClientHandleArc> {
    let mut builder = Client::builder().await?;          // Client::builder()
    builder.with_module(MintClientInit);
    builder.with_module(WalletClientInit::default());    // module registration
    let preview = builder.preview(connectors.clone(), &invite).await?;  // ClientBuilder::preview
    let client = preview.join(db, root_secret).await?;   // ClientPreview::join -> Client
    Ok(Arc::new(client))   // ClientHandle wraps Client for lifecycle
}
```

Actual names confirmed from docs:
- `Client::builder() -> Result<ClientBuilder>`
- `ClientBuilder::with_module<M: ClientModuleInit>(&mut self, module_init: M)`
- `ClientBuilder::preview(connectors: ConnectorRegistry, invite_code: &InviteCode) -> Result<ClientPreview>`
- `ClientPreview::join(db: &Database, root_secret: RootSecret) -> Result<Client>`  (also `open` for existing configs)
- `Client::start_executor(&self)` — starts the central `crate::sm::executor::Executor` that runs all module state machines
- `Client::spawn_federation_reconnect(&self)` — background tasks maintaining guardian connections
- `Client::db() -> &Database`, `Client::executor() -> &(dyn IExecutor + ...)`
- `Client::get_first_module::<M: ClientModule>(&self) -> Result<ClientModuleInstance<'_, M>>`
- `Client::get_first_module_arc::<M: ClientModule>(&self) -> Result<Arc<M>>`

The root secret comes from a BIP39 mnemonic via `fedimint_derive-secret` / `RootSecret::StandardDoubleDerive(Bip39RootSecretStrategy)` — **guessing at the exact strategy type name** (douglaz uses `Bip39RootSecretStrategy::<12>` and `RootSecret::StandardDoubleDerive(...)`; seen in `multi_client.rs`).

Storage layout (from douglaz `MultiClient`, spec §4): one `Database` shared across federations; fedimint client `i` lives at `db.with_prefix([0x01] ++ u32_le(db_prefix))`. The app journal lives under a separate prefix `[0x00]` so journal churn never shares a RocksDB memtable with fedimint's long-held transactions (douglaz's hard-won lesson: co-location causes `TryAgain`/`WriteConflict`).

**Guessing at:** whether a single `Client` per federation vs. `multimint` facade is preferable here — we keep it simple: one `Client` per federation, stored in an `Arc<Mutex>`/`RwLock` map keyed by `FederationId`, mirroring douglaz's `MultiClient::clients`.

---

## 4. Peg-in (BDK → federation deposit address → mint ecash)

**Flow:**
1. BDK side: generate/receive address via `bdk_wallet::Wallet::reveal_next_address(KeychainKind::External)` (persist the ChangeSet after revealing — docs warn about address reuse). Sign + broadcast a transaction paying the **federation deposit address** using `Wallet::build_tx()` → `.add_recipient(deposit_script, amount)` → `.finish()` → `create_psbt` / `sign_with_signers` → `finalize_psbt` → `extract_tx()` → broadcast via chain source.
2. Fedimint side: `wallet_client.allocate_deposit_address_pooled(max_gap_size) -> (DepositAddressInfo, AllocateDepositOutcome)` — gives a federation-controlled BTC deposit address (tweak-based). **This is the address BDK pays.** (`WalletClientModule::allocate_deposit_address_pooled`; the 40 KB tx-limit caveat applies to `allocate_deposit_address_expert_only`; pooled is the recommended path.)
3. After broadcast, the fedimint wallet client's `pegin_monitor` state machine detects the on-chain deposit (requires a configured `BitcoindRpc` via `ClientBuilder::with_bitcoind_rpc`, chain ID cached after first fetch via `Client::chain_id()`). After `get_finality_delay()` confirmations, it mints ecash via the mint module (primary module).
4. Track completion: `wallet_client.subscribe_deposit(operation_id) -> Result<UpdateStreamOrOutcome<DepositStateV2>>`, or `client.transaction_updates(op_id)`, or `client.await_primary_bitcoin_module_output(op_id, out_point)`.
5. Balance: `client.get_balance_for_unit(AmountUnit::Bitcoin)` — the mint module is `supports_being_primary`, so ecash balance is readable via `get_balance_for_unit`.

**The `peg_in` entry point:** `WalletClientModule::peg_in(PegInRequest) -> PegInResponse` exists — I'm **guessing at** whether it's needed in addition to `allocate_deposit_address_pooled` (it may register/claim a peg-in). I'll treat `allocate_deposit_address_pooled` + `subscribe_deposit` as the core path and flag `peg_in` for verification against the source.

**BDK API names confirmed:**
- `Wallet::reveal_next_address(KeychainKind) -> AddressInfo`
- `Wallet::build_tx() -> TxBuilder`
- `Wallet::create_psbt(PsbtParams<CreateTx>) -> Result<(Psbt, Finalizer)>`
- `Wallet::sign_with_signers(&mut Psbt, &[&SignersContainer], SignOptions) -> Result<bool>`
- `Wallet::finalize_psbt(&mut Psbt, SignOptions) -> Result<bool>`
- `Wallet::apply_update(Update) -> Result<(), CannotConnectError>` (stages a ChangeSet)
- `Wallet::take_staged() -> Option<ChangeSet>` (to persist)
- `Wallet::list_unspent()`, `Wallet::get_utxo(OutPoint)`, `Wallet::get_tx(Txid) -> Option<WalletTx>`
- `Wallet::balance() -> Balance`, `Wallet::calculate_fee(&Transaction) -> Result<Amount>`
- `Wallet::insert_txout(OutPoint, TxOut)` (for foreign UTXO fee calc)
- `Wallet::start_sync_with_revealed_spks() -> SyncRequestBuilder` (spk-based partial sync)
- Persistence: via `bdk_wallet::rusqlite` (module `rusqlite_impl`) or `bdk_file_store` — `WalletPersister` trait; `PersistedWallet::create/load`. **Guessing at:** exact persist call shape (`wallet.persist(&mut conn)?` shown in docs example).

---

## 5. Peg-out (federation withdrawal → BDK receives + reconciles)

**Flow:**
1. Quote: `wallet_client.get_withdraw_fees(&address, amount) -> Result<PegOutFees>` (or `send_fee_quote(amount) -> FeeQuote` for federation-only fees; miner fee is inside the output amount). For a sweep: `max_withdrawable_amount(&address, balance) -> (Amount, PegOutFees)`.
2. Submit: `wallet_client.withdraw(&address, amount, fee: PegOutFees, extra_meta) -> Result<OperationId>`. This creates a wallet output funded by ecash inputs via the primary module (mint), the federation signs the BTC withdrawal transaction and broadcasts it.
3. Track: `wallet_client.subscribe_withdraw_updates(op_id) -> Result<UpdateStreamOrOutcome<WithdrawState>>`; `client.transaction_updates(op_id)`.
4. BDK reconciliation (the BDK chain-sync task): watch for the withdrawal transaction paying to a BDK-controlled address. When `get_tx(txid)` confirms (anchored in best chain), match `txid` → `OperationId` (stored in the journal at peg-out submission), mark the operation reconciled/done in the crash-safe journal.
5. If the withdrawal gets stuck, RBF: `create_rbf_withdraw_output` / `rbf_withdraw` (deprecated since 0.4 — flagged).

**Confirmed names:**
- `WalletClientModule::withdraw(address, amount, fee, extra_meta) -> OperationId`
- `WalletClientModule::get_withdraw_fees(address, amount) -> PegOutFees`
- `WalletClientModule::send_fee_quote(amount) -> FeeQuote`
- `WalletClientModule::max_withdrawable_amount(address, balance) -> (Amount, PegOutFees)`
- `WalletClientModule::subscribe_withdraw_updates(op_id) -> UpdateStreamOrOutcome<WithdrawState>`
- `WalletClientModule::create_withdraw_output(op_id, address, amount, fees) -> ClientOutputBundle<WalletOutput, WalletClientStates>` (lower-level; `withdraw` is the high-level wrapper)

---

## 6. Concurrent sync tasks with crash-safe persistence

Two long-running tokio tasks (plus the fedimint client's internal executor/reconnect tasks):

### Task A — BDK chain sync (`sync::bdk_chain_task`)
- Loop: call chain source (`bdk_esplora`/`bdk_electrum`/RPC) → build a `bdk_chain::Update` from `Wallet::start_sync_with_revealed_spks()` → `wallet.apply_update(update)` → `wallet.take_staged()` → persist ChangeSet via `WalletPersister` (rusqlite or bdk_file_store).
- Detect incoming BTC (peg-out arrival / peg-in confirmation seen from BDK side): `wallet.get_tx(txid)`, `wallet.list_unspent()`, `WalletEvent::TxConfirmed` via `apply_update_events`.
- On peg-out detection: look up `operation_id` by `txid` in the journal; write `Done` + `OperationRecord` to the append-only ledger (`TAG_LEDGER_ROW` pattern from douglaz).

### Task B — Fedimint federation sync (`sync::fedimint_sync_task`)
- `client.spawn_federation_reconnect()` (keeps guardian connections warm).
- Drive state machines: `client.start_executor()` (executor runs all module state machines in the background).
- React to events: `client.handle_events(tracker, handler_fn)` or `client.get_event_log_transient_receiver()` — e.g. when `DepositStateV2` reaches `Credited`, record the peg-in credit in the journal; when `WithdrawState` reaches success, record the withdrawal broadcast txid.
- Balance stream: `client.subscribe_balance_changes(unit)`.

### Crash-safe persistence (douglaz patterns, `journal.rs`)
- App journal under `Database` prefix `[0x00]`; fedimint clients under `[0x01] ++ u32_le(db_prefix)` — **separate partitions** (douglaz's RocksDB memtable-history lesson).
- Raw byte API + serde_json versioned envelopes (`ROW_VERSION = 1`): rows keyed by `[tag] ++ id_bytes` (`0x01 Intent`, `0x02 MoveRecord`, `0x03 FederationInfo`, `0x04 PendingIndex`, `0x05 LedgerRow`, …).
- **Intent + pending index committed in ONE `dbtx`:** `begin_transaction … commit_tx` — status change removes old index entry and inserts new one atomically, so a crash never leaves an intent indexed under a status it no longer holds.
- `Journal::pending` / `Journal::failed` read index + intents from ONE `begin_transaction_nc` snapshot — no double-count/drop mid-scan.
- Every money intent gets an **idempotency key** and a **deterministic `OperationId`**; on restart, re-drive the pending set (never double-claim a deposit, never double-submit a peg-out).
- `FederationInfo` registry row (`invite`, `db_prefix`, `joined_at`) persisted so restart can `ClientBuilder::open` existing partitions instead of re-joining.
- Recovery: `ClientPreview::recover(db, root_secret, ...)` then `wait_for_all_recoveries()`; the recovery-phase handle omits recovered modules, so you **shutdown the recovery client and reopen via `ClientBuilder::open`** before reading balance (douglaz `recover_inner` / `open_recovered_partition`).
- A `join_lock` (`Mutex<()>`) + `active_recoveries` reservation serialize prefix allocation so two concurrent joins/recoveries never initialize the same partition.
- `open_all` is best-effort: one un-openable federation is warn-logged and skipped, never aborting the batch (poison tolerance).

**Guessing at:** exact `Journal` trait shape from `wallet_core` (we'd either depend on douglaz's `wallet_core` or re-implement a smaller journal — flagged as a design decision).

---

## 7. Thin UniFFI crate (`pegwallet-ffi`)

`uniffi = "0.32.0"` (matches the optional uniffi dependency already present in fedimint-client 0.12.0).

- `#[uniffi::export]` a `PegWallet` ABI object (must be `Send + Sync + 'static`; internally `Arc<PegWalletCore>`).
- Methods (uniffi async — requires the uniffi async runtime component):
  - `join_federation(invite_code: String) -> Result<String>` (federation id hex)
  - `balance_sat() -> u64`
  - `peg_in_address() -> String`
  - `peg_in(sat_amount: u64) -> Result<String>` (operation id)
  - `peg_out(address: String, sat_amount: u64) -> Result<String>` (operation id)
  - `operation_status(op_id: String) -> String`
  - `sync() -> Result<()>` (trigger one BDK + fedimint sync tick)
- Streams: uniffi supports streams/callbacks via `&UniFFI` consumer callbacks; simpler to expose a polling `list_operations() -> Vec<OperationDto>` plus a callback subscription for balance/operation updates.

**Guessing at:** uniffi async method support in 0.32 requires `uniffi` crate's `async` feature and the generated bindings glue per target language; for the initial version I'd expose **blocking** methods (callers `block_on`) plus a synchronous `poll`/`list` API, then add async streams once bindings are verified. Also, uniffi cannot expose `anyhow::Error` directly — map to a `pegwallet_ffi::Error` enum with `thiserror` + uniffi-compatible variants.

---

## 8. Module map (confirmed function/module names)

**fedimint-client (0.12.0)**
- `Client::builder()`, `ClientBuilder::with_module()`, `preview()`, `join()/open()`
- `Client::get_balance_for_unit()`, `get_first_module::<M>()`, `get_first_module_arc::<M>()`
- `Client::finalize_and_submit_transaction(op_id, op_type, meta_gen, tx_builder) -> OutPointRange`
- `Client::transaction_updates(op_id) -> TransactionUpdates`
- `Client::await_primary_bitcoin_module_output(op_id, out_point)`
- `Client::subscribe_balance_changes(unit) -> BoxStream<Amount>`
- `Client::spawn_federation_reconnect()`, `start_executor()`, `db()`, `executor()`, `operation_log()`, `handle_events()`, `get_event_log_transient_receiver()`
- `Client::chain_id()`, `federation_id()`, `config()`
- `ClientContext`, `OutPointRange`, `TransactionBuilder`, `OperationId`

**fedimint-wallet-client (0.12.0)**
- `WalletClientModule::allocate_deposit_address_pooled(max_gap_size) -> (DepositAddressInfo, AllocateDepositOutcome)`
- `WalletClientModule::allocate_deposit_address_expert_only()`, `safe_allocate_deposit_address()`
- `WalletClientModule::peg_in(PegInRequest) -> PegInResponse` (verify usage)
- `WalletClientModule::withdraw(address, amount, fee: PegOutFees, extra_meta) -> OperationId`
- `WalletClientModule::get_withdraw_fees(address, amount) -> PegOutFees`
- `WalletClientModule::send_fee_quote(amount) -> FeeQuote`
- `WalletClientModule::max_withdrawable_amount(address, balance) -> (Amount, PegOutFees)`
- `WalletClientModule::subscribe_deposit(op_id) -> UpdateStreamOrOutcome<DepositStateV2>`
- `WalletClientModule::subscribe_withdraw_updates(op_id) -> UpdateStreamOrOutcome<WithdrawState>`
- `WalletClientModule::recheck_pegin_address(tweak_idx)`, `create_withdraw_output(...)`
- `WalletClientInit`

**fedimint-mint-client (0.12.0)**
- `MintClientModule::reissue_external_notes(oob_notes, extra_meta) -> OperationId` (peg-in ecash issuance)
- `MintClientModule::send_oob_notes(amount) -> OOBNotes`, `spend_notes_with_selector(...)`
- `MintClientModule::create_output(dbtx, op_id, notes_per_denomination, exact_amount)`
- `MintClientModule::await_output_finalized(op_id, out_point)`
- `MintClientModule::get_note_counts_by_denomination()`, `estimate_spend_all_fees()`
- `MintClientInit`

**bdk_wallet (3.2.0)**
- `Wallet::create(desc, change_desc)`, `Wallet::load()`
- `reveal_next_address(KeychainKind::External)`, `next_unused_address`, `list_unspent()`, `get_utxo()`, `get_tx()`
- `build_tx()`, `create_psbt()`, `sign_with_signers()`, `finalize_psbt()`, `take_staged()`, `apply_update()`, `balance()`, `calculate_fee()`, `insert_txout()`
- `start_sync_with_revealed_spks()`, `start_full_scan()`
- `WalletEvent::TxConfirmed` etc. via `apply_update_events()`
- Persistence via `bdk_wallet::rusqlite` (`PersistedWallet`) or `bdk_file_store`

**fedimint-clientd (reference daemon)**
- `AppState::new(db_path)`, `multimint.register_new(invite_code)`, `multimint.all()`
- `/v2/onchain/deposit-address`, `/v2/onchain/await-deposit`, `/v2/onchain/withdraw` (shape of the REST API we're replacing with UniFFI)

**douglaz/fedimint-wallets (crash-safe patterns)**
- `wallet-fedimint::MultiClient` — per-federation clients, `[0x01] ++ u32_le(db_prefix)` partitioning, `join_lock`, `active_recoveries` reservation, `open_all` best-effort, `wait_for_all_active_state_machines`, `Client::shutdown()` before replacing handle
- `wallet-fedimint::FedimintJournal` — app prefix `[0x00]`, raw byte API + serde_json, `Intent`/`MoveRecord`/`FederationInfo`/`OperationRecord` rows, atomic status+index transitions in one dbtx, `Journal::pending`/`failed` snapshot reads, idempotency keys, deterministic op-ids
- `wallet-fedimint::runtime::Runtime` — single actor owning everything, watch scheduler, per-operation IO driver tasks, restore-mnemonic recovery, settlement-stall watchdog
- Pinned fedimint rev `72b1e5be...` (branch `wallet-pin/iroh-recovery-tpe8838`)

---

## 9. What I'm guessing at (needs verification against source code)

1. **`peg_in` vs `allocate_deposit_address_pooled`** — whether peg-in requires an explicit `peg_in(PegInRequest)` call after funding the address, or whether the `pegin_monitor` state machine auto-mints ecash once the deposit confirms. Docs describe `subscribe_deposit` for "an ongoing deposit operation created with `allocate_deposit_address_expert_only`", suggesting allocation alone drives it; `peg_in`'s exact role is unclear.
2. **Whether `WalletClientModule` (on-chain) or `MintClientModule` is the primary module for BTC** — `get_balance_for_unit(AmountUnit::Bitcoin)` needs a primary module; the wallet module claims `supports_being_primary` (per `ClientModule` trait). Mint also claims it. Need to confirm which unit maps to which module for `get_balance_for_unit`.
3. **`ClientBuilder::open` vs `join` signatures** — `open(connectors, db_no_decoders, pre_root_secret)`; `preview_with_existing_config` exists. Exact flow for restoring an existing client (vs. joining fresh) needs confirming in `ClientPreview`.
4. **BDK persistence call shape** — docs show `wallet.persist(&mut conn)?` but the `WalletPersister` trait / `PersistedWallet` exact methods weren't fully expanded.
5. **Chain source choice** — esplora/electrum/bitcoind-rpc; which one the crate defaults to is a product decision. `ClientBuilder::with_bitcoind_rpc` lets fedimint modules watch the chain; BDK needs its own chain source.
6. **UniFFI async** — uniffi 0.32 async export support and per-language binding generation not verified.
7. **Depending on douglaz's `wallet_core`/`wallet-fedimint`** — it's AGPL-3.0 and not published on crates.io (git dep at a pinned commit). For our crate we'd either re-implement a small journal inline (MIT/Apache friendly) or take the git dep with the license consequence. Flagged as a licensing decision.
8. **`tx_builder` construction for fedimint transactions** — `finalize_and_submit_transaction` takes a `TransactionBuilder`; how to build one for a peg-out (which is driven by `wallet_client.withdraw`, not manual tx building) needs confirming — `withdraw` appears to be the high-level wrapper that builds/submits internally.

---

## 10. Suggested implementation order (for when you switch agents)

1. Workspace + `pegwallet-core` with pinned deps; `Client` construction + module registration (mint + wallet).
2. BDK wallet wrapper: create/load, reveal address, `build_tx`+sign+broadcast, persist, `start_sync_with_revealed_spks` loop.
3. Crash-safe journal (douglaz pattern): intent rows, idempotency keys, atomic dbtx status transitions, pending re-drive on startup.
4. Peg-in: `allocate_deposit_address_pooled` → BDK pays → `subscribe_deposit` / `await_primary_bitcoin_module_output` → ecash balance.
5. Peg-out: `get_withdraw_fees` → `withdraw` → `subscribe_withdraw_updates` → BDK chain sync detects withdrawal tx → reconcile in journal.
6. Two sync tasks (BDK chain + fedimint federation) sharing the journal; `spawn_federation_reconnect` + `start_executor`.
7. `pegwallet-ffi` uniffi crate with `PegWallet` object; generate bindings for one target language as a smoke test.

---

**Bottom line:** all the API names I cite above are taken directly from the docs.rs pages for fedimint-client/mint-client/wallet-client 0.12.0 and bdk_wallet 3.2.0, plus the actual `main.rs`/`multi_client.rs`/`journal.rs` source of the two reference repos. The items marked "guessing at" are the only places where I'd want to read the crate source before writing code. Nothing has been implemented or modified.
