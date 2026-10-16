# VERIFY.md — verification of AGENTS.md / PLAN.md against real crates

**Deliverable:** a compiling crate (`pegwallet`, single binary at repo root) whose
`src/main.rs` references every API name claimed in AGENTS.md / PLAN.md, plus this report.

- `cargo check` → **exit 0** (763-package lockfile).
- Nothing was executed; all references are in dead `#[allow(...)]` functions and `main`
  only prints. No peg logic implemented.
- Method names are resolved against the **real downloaded crate sources** under
  `~/.cargo/registry/src/index.crates.io-*/`, not from memory.

> Section-numbering note: the task referred to "PLAN.md §6" for dependencies and "§7" for API
> names, but in the actual `PLAN.md` the dependency table is **§2** and the API map is **§8**.
> This report covers the actual dependency table (§2) and the full API surface (§8 + AGENTS.md
> "Confirmed API names").

---

## 0. Environment bootstrap (why "just `cargo check`" did not work)

The crate compiles, but only after fixing environment gaps that are *not* in the plan. These are
recorded because a fresh checkout cannot build without them:

| Blocker | Detail | Resolution used |
|---|---|---|
| **Toolchain too old** | Default `rustc` was **1.89.0**. `fedimint-connectors 0.12.0` → `iroh-mainline-address-lookup 0.4.0` → `iroh 1.0.3`, `iroh-base 1.3.0`, `netwatch 0.19.3`, … declare **`rust-version = 1.91`**. `cargo` hard-errors: *"rustc 1.89.0 is not supported"*. | `rustup toolchain install 1.91` → **1.91.1**; committed `rust-toolchain.toml` (`channel = "1.91.1"`). `--ignore-rust-version` bypasses the *gate* but the crate still genuinely needs 1.91. |
| **`cmake` missing** | `aws-lc-sys` (a **hard, non-optional** dependency of `fedimint-core` **and** `fedimint-connectors`) builds C via CMake. | `uv tool install cmake` → cmake 4.4.4. |
| **`libclang` missing** | `aws-lc-sys` forces the `bindgen` feature; bindgen needs `libclang.so`. No system libclang; `sudo` needs a password. | `uv pip install --target /tmp/opencode/lc libclang` (**libclang 18.1.1**, manylinux = system-glibc compatible). The nix-provided `libclang.so` **did not work** (see Surprises). |
| **clang builtin headers** | PyPI libclang ships no resource headers → `fatal error: 'stddef.h' file not found`. | `BINDGEN_EXTRA_CLANG_ARGS` pointing at clang-18 builtin headers + GCC include dir. |

All four are encoded in `.cargo/config.toml` (`[build] rustflags`, `[env]`) and
`rust-toolchain.toml`, so `cargo check` now works as-is in this sandbox.

---

## 1. Confirmed names (they exist) — with real signatures

All names below were **compiled** in `src/main.rs` and cross-checked at the given source
location (`fedimint *-0.12.0`, `bdk_*` as pinned). "✓ compiled" means `cargo check` accepted a use.

### fedimint-client 0.12.0
| Name | Real signature (abridged) | Source |
|---|---|---|
| `Client::builder` ✓ | `pub async fn builder() -> Result<ClientBuilder>` | `client/builder.rs` |
| `ClientBuilder::with_module` ✓ | `pub fn with_module<M: ClientModuleInit>(&mut self, module_init: M)` | `client/builder.rs:182` |
| `ClientBuilder::preview` ✓ | `pub async fn preview(self, connectors: ConnectorRegistry, invite_code: &InviteCode) -> Result<ClientPreview>` | `client/builder.rs:396` |
| `ClientBuilder::preview_with_existing_config` | `pub async fn preview_with_existing_config(self, connectors, config, api_secret) -> Result<ClientPreview>` | `client/builder.rs:442` |
| `ClientPreview::join` ✓ | `pub async fn join(self, db_no_decoders: Database, pre_root_secret: RootSecret) -> Result<ClientHandle>` | `client/builder.rs:1483` |
| `Client::get_balance_for_unit` ✓ | `pub async fn get_balance_for_unit(&self, unit: AmountUnit) -> Result<Amount>` | `client.rs` |
| `Client::get_first_module` ✓ | `pub fn get_first_module<M: ClientModule>(&self) -> Result<ClientModuleInstance<'_, M>>` | `client.rs:1266` |
| `Client::get_first_module_arc` ✓ | `pub fn get_first_module_arc<M: ClientModule>(&self) -> Result<Arc<M>>` | `client.rs:1293` |
| `Client::finalize_and_submit_transaction` ✓ | `pub async fn finalize_and_submit_transaction<F, M>(&self, operation_id, operation_type: &str, operation_meta_gen: F, tx_builder: TransactionBuilder) -> Result<OutPointRange>` | `client.rs` |
| `Client::transaction_updates` ✓ | `pub async fn transaction_updates(&self, operation_id: OperationId) -> TransactionUpdates` | `client.rs` |
| `Client::await_primary_bitcoin_module_output` ✓ | `pub async fn await_primary_bitcoin_module_output(&self, operation_id, out_point: OutPoint) -> Result<()>` | `client.rs` |
| `Client::subscribe_balance_changes` ✓ | `pub async fn subscribe_balance_changes(&self, unit) -> BoxStream<'static, Amount>` | `client.rs` |
| `Client::spawn_federation_reconnect` ✓ | `pub fn spawn_federation_reconnect(&self)` | `client.rs` |
| `Client::start_executor` ✓ | `pub fn start_executor(self: &Arc<Self>)` **(receiver is `&Arc<Self>`, not `&self`)** | `client.rs` |
| `Client::chain_id` ✓ | `pub async fn chain_id(&self) -> Result<ChainId>` | `client.rs` |
| `Client::federation_id` ✓ | `pub fn federation_id(&self) -> FederationId` | `client.rs` |
| `Client::db` ✓ | `pub fn db(&self) -> &Database` | `client.rs` |
| `Client::get_event_log_transient_receiver` ✓ | `pub fn get_event_log_transient_receiver(&self) -> Receiver<EventLogEntry>` | `client.rs` |
| `Client::handle_events` ✓ | `pub async fn handle_events<F, R>(&self, tracker, handler_fn: F) -> Result<()>` | `client.rs:2694` |
| `Client::built_in_application_event_log_tracker` ✓ | `pub fn built_in_application_event_log_tracker(&self) -> DynEventLogTrimableTracker` | `client.rs` |

### fedimint-wallet-client 0.12.0
| Name | Real signature (abridged) | Source |
|---|---|---|
| `WalletClientModule::allocate_deposit_address_pooled` ✓ | `pub async fn allocate_deposit_address_pooled(&self, max_gap_size: usize) -> Result<(DepositAddressInfo, AllocateDepositOutcome)>` | `lib.rs:1405` |
| `allocate_deposit_address_expert_only` ✓ | `pub async fn allocate_deposit_address_expert_only<M>(&self, extra_meta: M) -> Result<DepositAddressInfo>` | `lib.rs:1167` |
| `safe_allocate_deposit_address` ✓ | `pub async fn safe_allocate_deposit_address<M>(&self, extra_meta: M) -> Result<DepositAddressInfo>` | `lib.rs:1135` |
| `subscribe_deposit` ✓ | `pub async fn subscribe_deposit(&self, operation_id: OperationId) -> Result<UpdateStreamOrOutcome<DepositStateV2>>` | `lib.rs:1509` |
| `subscribe_withdraw_updates` ✓ | `pub async fn subscribe_withdraw_updates(&self, operation_id) -> Result<UpdateStreamOrOutcome<WithdrawState>>` | `lib.rs:1960` |
| `withdraw` ✓ | `pub async fn withdraw<M: Serialize + MaybeSend + MaybeSync>(&self, address: &bitcoin::Address, amount: bitcoin::Amount, fee: PegOutFees, extra_meta: M) -> Result<OperationId>` | `lib.rs:1866` |
| `get_withdraw_fees` ✓ | `pub async fn get_withdraw_fees(&self, address: &Address, amount: Amount) -> Result<PegOutFees>` | `lib.rs:866` |
| `send_fee_quote` ✓ | `pub async fn send_fee_quote(&self, amount: bitcoin::Amount) -> Result<FeeQuote>` | `lib.rs:891` |
| `max_withdrawable_amount` ✓ | `pub async fn max_withdrawable_amount(&self, address: &Address, balance: Amount) -> Result<(Amount, PegOutFees)>` | `lib.rs:939` |
| `create_withdraw_output` ✓ | `pub fn create_withdraw_output(&self, operation_id, address, amount, fees) -> Result<ClientOutputBundle<WalletOutput, WalletClientStates>>` | `lib.rs:993` |
| `peg_in` ✓ | `pub async fn peg_in(&self, req: PegInRequest) -> Result<PegInResponse>` | `lib.rs:1029` |
| `recheck_pegin_address` ✓ | `pub async fn recheck_pegin_address(&self, tweak_idx: TweakIdx) -> Result<()>` | `lib.rs:1755` |
| `WalletClientInit` ✓ | `pub struct WalletClientInit(pub Option<DynBitcoindRpc>)` → `WalletClientInit::default()` works | `lib.rs:202` |
| `DepositAddressInfo` / `AllocateDepositOutcome` / `DepositStateV2` / `WithdrawState` / `PegInRequest` / `PegInResponse` / `PegOutRequest` | all exist | `lib.rs:140/168/118/179/752/…/781` |

### fedimint-mint-client 0.12.0
| Name | Real signature (abridged) | Source |
|---|---|---|
| `MintClientModule::reissue_external_notes` ✓ | `pub async fn reissue_external_notes<M: Serialize + Send>(&self, oob_notes: OOBNotes, extra_meta: M) -> Result<OperationId>` | `lib.rs:1981` |
| `subscribe_reissue_external_notes` ✓ | `pub async fn subscribe_reissue_external_notes(&self, op_id) -> Result<UpdateStreamOrOutcome<ReissueExternalNotesState>>` | `lib.rs:2069` |
| `send_oob_notes` ✓ | `pub async fn send_oob_notes<M: Serialize + Send>(&self, amount: Amount, extra_meta: M) -> Result<OOBNotes>` | `lib.rs:2307` |
| `spend_notes_with_selector` ✓ | `pub async fn spend_notes_with_selector<M>(&self, notes_selector: &impl NotesSelector, requested_amount, try_cancel_after: Option<Duration>, include_invite: bool, extra_meta: M) -> Result<(OperationId, OOBNotes)>` | `lib.rs:2185` |
| `create_output` ✓ | `pub async fn create_output(&self, dbtx, operation_id, notes_per_denomination: u16, exact_amount: Amount) -> ClientOutputBundle<…>` | `lib.rs:1367` |
| `await_output_finalized` ✓ | `pub async fn await_output_finalized(&self, operation_id, out_point) -> Result<()>` | `lib.rs:1541` |
| `get_note_counts_by_denomination` ✓ | `pub async fn get_note_counts_by_denomination(&self, dbtx) -> TieredCounts` | `lib.rs:1489` |
| `estimate_spend_all_fees` ✓ | `pub async fn estimate_spend_all_fees(&self) -> Amount` | `lib.rs:1519` |
| `MintClientInit` ✓ | `pub struct MintClientInit;` (unit) | `lib.rs:592` |
| `OOBNotes` ✓ / `SelectNotesWithAtleastAmount` ✓ | `pub struct OOBNotes(…)` / `pub struct SelectNotesWithAtleastAmount;` (impl `NotesSelector`) | `lib.rs:219` / `lib.rs:2763` |

### bdk_wallet 3.2.0
| Name | Real signature (abridged) | Source |
|---|---|---|
| `Wallet::create` ✓ | `pub fn create<D>(descriptor: D, change_descriptor: D) -> CreateParams` | `wallet/mod.rs:288` |
| `Wallet::load` ✓ | `pub fn load() -> LoadParams` | `wallet/mod.rs:446` |
| `Wallet::reveal_next_address` ✓ | `pub fn reveal_next_address(&mut self, keychain: KeychainKind) -> AddressInfo` | `wallet/mod.rs:671` |
| `Wallet::build_tx` ✓ | `pub fn build_tx(&mut self) -> TxBuilder<'_, DefaultCoinSelectionAlgorithm>` | `wallet/mod.rs:1272` |
| `Wallet::create_psbt` ✓ | `pub fn create_psbt(&mut self, params: PsbtParams<CreateTx>) -> Result<(Psbt, Finalizer), CreatePsbtError>` | `wallet/mod.rs:3238` |
| `Wallet::insert_txout` ✓ | `pub fn insert_txout(&mut self, outpoint: OutPoint, txout: TxOut)` | `wallet/mod.rs:940` |
| `Wallet::calculate_fee` ✓ | `pub fn calculate_fee(&self, tx: &Transaction) -> Result<Amount, CalculateFeeError>` | `wallet/mod.rs:973` |
| `Wallet::sign_with_signers` ✓ | `pub fn sign_with_signers(&self, psbt: &mut Psbt, signers: &[&SignersContainer], options: SignOptions) -> Result<bool, SignerError>` | `wallet/mod.rs:1884` |
| `Wallet::finalize_psbt` ✓ | `pub fn finalize_psbt(&self, psbt: &mut Psbt, options: SignOptions) -> Result<bool, SignerError>` | `wallet/mod.rs:1996` |
| `Wallet::apply_update` ✓ | `pub fn apply_update(&mut self, update: impl Into<Update>) -> Result<(), CannotConnectError>` | `wallet/mod.rs:2459` |
| `Wallet::take_staged` ✓ | `pub fn take_staged(&mut self) -> Option<ChangeSet>` | `wallet/mod.rs:2582` |
| `Wallet::start_sync_with_revealed_spks` ✓ | `pub fn start_sync_with_revealed_spks(&self) -> SyncRequestBuilder<(KeychainKind, u32)>` | `wallet/mod.rs:2983` |
| `Wallet::start_full_scan` ✓ | `pub fn start_full_scan(&self) -> FullScanRequestBuilder<KeychainKind>` | `wallet/mod.rs:3008` |
| `Wallet::list_unspent` / `get_utxo` / `get_tx` / `balance` ✓ | present | `wallet/mod.rs` |
| `Update` ✓ | re-exported at **`bdk_wallet::Update`** (not `bdk_wallet::chain::Update`) | `lib.rs` |
| `PersistedWallet::create` / `load` | `pub fn create(&mut P, CreateParams) -> Result<Self, CreateWithPersistError<P::Error>>` / `pub fn load(&mut P, LoadParams) -> Result<Option<Self>, …>` | `wallet/persisted.rs:151/173` |

### bdk_esplora 0.22.1
| Name | Real signature | Source |
|---|---|---|
| `EsploraAsyncExt` ✓ (trait) | `pub trait EsploraAsyncExt` — impl'd for `esplora_client::AsyncClient<S>` | `src/async_ext.rs:23,60` |
| `EsploraAsyncExt::full_scan` ✓ | `async fn full_scan<K, R: Into<FullScanRequest<K>> + Send>(&self, request: R, stop_gap: usize, parallel_requests: usize) -> Result<FullScanResponse<K>, Error>` | `src/async_ext.rs:33` |
| `EsploraAsyncExt::sync` ✓ | `async fn sync<I, R: Into<SyncRequest<I>> + Send>(&self, request: R, parallel_requests: usize) -> Result<SyncResponse, Error>` | `src/async_ext.rs:61` |
| `EsploraExt` (blocking) | `pub trait EsploraExt` | `src/blocking_ext.rs:20` |
| `bdk_esplora::esplora_client` ✓ | re-export (`pub use esplora_client;`) | `src/lib.rs:28` |

### fedimint-core 0.12.0 / fedimint-client-module 0.12.0
| Name | Real path | Source |
|---|---|---|
| `Database::with_prefix` ✓ | `pub fn with_prefix(&self, prefix: Vec<u8>) -> Self` | `db/mod.rs:442` |
| `OperationId` ✓ | `fedimint_core::core::OperationId(pub [u8; 32])` | `core.rs:57` |
| `OutPointRange` ✓ | **`fedimint_core::OutPointRange`** (crate root, `lib.rs:474`) | `lib.rs:474` |
| `AmountUnit` ✓ | **`fedimint_core::module::AmountUnit(u64)`** | `module/mod.rs:77` |
| `ClientModuleInit` ✓ | `pub trait ClientModuleInit: ModuleInit + Sized` | `module/init.rs:422` |
| `TransactionBuilder` ✓ | `fedimint_client_module::transaction::builder::TransactionBuilder` | `transaction/builder.rs:648` |
| `FeeQuote` ✓ | `fedimint_client_module::transaction::builder::FeeQuote` | `transaction/builder.rs:453` |
| `PegOutFees` ✓ | `fedimint_wallet_common::PegOutFees` (re-exported `fedimint_wallet_client::common::PegOutFees`) | `fedimint-wallet-common/src/lib.rs:266` |

---

## 2. Wrong names (claimed → actual)

| Claimed (AGENTS.md / PLAN.md) | Reality | Actual name / path | Source link |
|---|---|---|---|
| `EsploraClient` | **does not exist** anywhere | `bdk_esplora::EsploraAsyncExt` (async) / `EsploraExt` (blocking) traits, impl'd for `esplora_client::AsyncClient` / `BlockingClient` | https://docs.rs/bdk_esplora/0.22.1/bdk_esplora/trait.EsploraAsyncExt.html |
| `EsploraClient::scan` | **no `scan` method exists** | full sync = `EsploraAsyncExt::full_scan`; partial sync = `EsploraAsyncExt::sync` | https://docs.rs/bdk_esplora/0.22.1/bdk_esplora/trait.EsploraAsyncExt.html |
| `ClientPreview::join(...) -> Result<Client>` (PLAN §3) | returns a **`ClientHandle`**, not `Client` | `pub async fn join(self, db_no_decoders: Database, pre_root_secret: RootSecret) -> Result<ClientHandle>` | https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientPreview.html |
| import `fedimint_client::module::init::ClientModuleInit` (PLAN §3 code) | path is one `module` short | `fedimint_client::module::module::init::ClientModuleInit` (≡ `fedimint_client_module::module::init::ClientModuleInit`) | https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/module/init/trait.ClientModuleInit.html |
| `TransactionBuilder` (PLAN §8, listed with client types) | not in `fedimint-client`/`fedimint-core` root | `fedimint_client::module::transaction::TransactionBuilder` | https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/transaction/builder/struct.TransactionBuilder.html |
| `FeeQuote` (PLAN §5) | same | `fedimint_client::module::transaction::FeeQuote` | https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/transaction/builder/struct.FeeQuote.html |
| `PegOutFees` (implicitly wallet-client root) | lives in the `common` module | `fedimint_wallet_client::common::PegOutFees` | https://docs.rs/fedimint-wallet-client/0.12.0/fedimint_wallet_client/common/struct.PegOutFees.html |
| `OutPointRange` under `fedimint_core::core` (PLAN §8) | at crate root | `fedimint_core::OutPointRange` | https://docs.rs/fedimint-core/0.12.0/fedimint_core/struct.OutPointRange.html |
| `start_executor(&self)` (implied) | receiver is `&Arc<Self>` | `pub fn start_executor(self: &Arc<Self>)` | https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html |
| `bdk_wallet` feature `keys-bip39` (as added) | **unresolvable** (see §3) | requires `bip39 ^2.2.2`, not published | — |

Not compile-verifiable here (they live in the GitHub repo, not a published crate): the
`fedimint-clientd` claims `AppState::new(db_path)`, `multimint.register_new(invite_code)`,
`multimint.all()`, and REST routes `/v2/onchain/{deposit-address,await-deposit,withdraw}`.
These were read from `fedimint/fedimint-clientd` `main.rs` and were **not** exercised by this
stub (`multimint` is a path crate, not a crates.io dependency).

---

## 3. Version pins that failed + what resolved

Every planned version was checked against the crates.io sparse index; the ones that exist were
requested with `cargo add … @=<ver>`. Result:

| Planned pin | Exists? | Outcome | Resolved |
|---|---|---|---|
| `fedimint-client/mint-client/wallet-client/core/client-module/connectors =0.12.0` | yes | ✅ held | `0.12.0` |
| `bdk_wallet =3.2.0` | yes | ✅ held | `3.2.0` |
| `bdk_chain =0.23.3` | yes | ✅ held | `0.23.3` (newer 0.23.4 exists) |
| `bdk_file_store =0.22.0` | yes | ✅ held | `0.22.0` |
| `bdk_esplora =0.22.1` | yes | ✅ held | `0.22.1` (newer 0.22.3 exists) |
| `bdk_electrum =0.23.2` | yes | ✅ held | `0.23.2` (newer 0.24.1 exists) |
| `bdk_bitcoind_rpc =0.22.0` | yes | ✅ held | `0.22.0` |
| `uniffi =0.32.0` | yes | ✅ held | `0.32.0` |
| `bitcoin =0.32.8` | yes | ✅ held | `0.32.8` |
| `serde_json =1.0.149` | yes | ✅ held | `1.0.149` |
| `thiserror =2.0.18` | yes | ✅ held | `2.0.18` (a transitive `1.0.69` also present) |
| `anyhow =1.0.98` | yes | ✅ held | `1.0.98` |
| `tracing =0.1.44` | yes | ✅ held | `0.1.44` |
| `async-trait =0.1.88` | yes | ✅ held | `0.1.88` |
| **`serde =1.0.219`** | yes | ❌ **FAILED** — `serde_json 1.0.149` requires `serde ^1.0.220` | relaxed → **`1.0.229`** |
| **`serde =1.0.220`** (retry) | yes | ❌ **FAILED** — another crate requires `serde ^1.0.228` | → **`1.0.229`** |
| **`tokio =1.48.0`** | yes | ❌ **FAILED** — `fedimint-connectors 0.12.0 → iroh-mainline-address-lookup ^0.4.0` requires **`tokio ^1.49`** | relaxed → **`1.53.3`** |
| **`bdk_wallet` feature `keys-bip39`** | — | ❌ **FAILED** — feature requires `bip39 ^2.2.2`; index max is **2.2.0** | dropped (not needed) |

**Net:** 3 genuine pin failures (`serde`, `tokio`, and the `keys-bip39` feature). The PLAN's
`serde 1.0.219` and `tokio 1.48.0` are **internally inconsistent** with `serde_json 1.0.149` and
`fedimint-connectors 0.12.0` respectively and must be raised.

Also note: the initial `cargo add … @=…` wrote failing pins into `Cargo.toml` *before* resolution
failed; `Cargo.toml` now contains `serde = "1"`, `serde_json = "1"`, `tokio = "1"` (relaxed) plus
the exact pins that held (`=0.12.0`, `=3.2.0`, `=0.32.8`, …).

---

## 4. bitcoin unification: **YES**

`cargo tree -i bitcoin` shows exactly **one** `bitcoin` package, and afterwards
`Cargo.lock` contains a single entry:

```
"bitcoin" "0.32.8"
```

No `bitcoin 0.32.x` duplicate, no other major. So the shared `OutPoint` / `Txid` / `Address`
types are the **same** `bitcoin` crate across `fedimint-*` and `bdk_*`. This includes
`esplora-client` and `secp256k1 0.29.1`, which both sit on the same unified `bitcoin 0.32.8`.

`bdk_tx 0.2.0` also resolves to a single version (used only behind the `bdk-tx` feature).

(The lockfile *does* contain many unrelated duplicates — `iroh 0.35.0` **and** `iroh 1.0.3`,
`hickory-* 0.25/0.26`, `rustls 0.21/0.23`, `aws-lc-sys 0.41.0`/`0.46.0`, `syn 1/2/3`, etc. —
but **not** `bitcoin`.)

---

## 5. bdk unstable feature required: **YES**

`bdk_wallet 3.2.0` gates its PSBT/fee API behind **both** the `bdk-tx` feature **and** the
`--cfg bdk_wallet_unstable` cfg. From `bdk_wallet-3.2.0/src/lib.rs`:

```rust
#[cfg(all(feature = "bdk-tx", not(bdk_wallet_unstable)))]
compile_error!("The `bdk-tx` feature requires `--cfg bdk_wallet_unstable`.");
#[cfg(all(bdk_wallet_unstable, feature = "bdk-tx"))]
…
```

`Wallet::create_psbt` lives in an `impl Wallet` block gated
`#[cfg(all(bdk_wallet_unstable, feature = "bdk-tx"))]` (`wallet/mod.rs:3052`) and is itself
`#[cfg(feature = "std")]`.

Consequences, and what this crate does:
- `Cargo.toml`: `bdk_wallet = { version = "=3.2.0", features = ["bdk-tx", "file_store", "rusqlite"] }`.
- `.cargo/config.toml`: `[build] rustflags = ["--cfg", "bdk_wallet_unstable"]`.
- `create_psbt`/`build_tx` are referenced in `src/main.rs` and **do** type-check with the above.

Feature facts verified from `bdk_wallet-3.2.0/Cargo.toml`:
- `default = ["std"]` → `bdk-tx` is **not** on by default.
- `bdk-tx = ["dep:bdk_tx"]`; `rusqlite = ["bdk_chain/rusqlite"]` → `rusqlite 0.31.0` with the
  **`bundled`** feature (no system SQLite needed); `file_store = ["bdk_file_store"]`.
- `bdk-tx` requires `std` (its example `psbt` carries `required-features = ["bdk-tx"]`).
- `keys-bip39 = ["bip39"]` is **unresolvable** (see §3).

---

## 6. Surprises

1. **The plan's "same-version" claim about `bitcoin` was right, but the pins around it were not.**
   `bitcoin 0.32.8` unifies cleanly; it is `serde`/`tokio` that had to move.

2. **Effective MSRV is 1.91, not 1.85.** `bdk_wallet` advertises 1.85, and PLAN.md says MSRV 1.85,
   but `fedimint-connectors 0.12.0` pulls `iroh 1.0.3` / `iroh-base 1.3.0` / `netwatch 0.19.3` /
   `portmapper 0.19.3` / `n0-*` that all declare `rust-version = 1.91`. The workspace therefore
   cannot build on the default 1.89 toolchain. Committed `rust-toolchain.toml` = `1.91.1`.

3. **`aws-lc-sys` is a hard dependency of `fedimint-core` and `fedimint-connectors`** (not optional),
   so a from-scratch build needs a full native toolchain: `cmake` + a C compiler **+ `libclang`**
   (its `bindgen` feature is force-enabled via the `ssl` feature). This is not mentioned anywhere
   in PLAN.md and makes the crate much heavier to build than the plan implies.

4. **The nix-provided `libclang` is unusable here** despite clang being "installable":
   `libclang.so.21.1.8` from the nix store failed to `dlopen` under the system loader with
   `librt.so.1: undefined symbol: __pointer_chk_guard, version GLIBC_PRIVATE` (nix glibc 2.44 vs
   system glibc). The working solution was the **PyPI `libclang` wheel (18.1.1, manylinux)** via
   `uv`, plus `BINDGEN_EXTRA_CLANG_ARGS` for the builtin headers. No passwordless `sudo`/`apt`.

5. **`cargo add` writes the failing dependency to `Cargo.toml` before resolution succeeds**, so a
   failed `cargo add` still mutates the manifest. Had to relax and re-run.

6. **`serde` has a `serde_core` companion** in the lock (`serde_core 1.0.229`) pulled by `axum` and
   others — harmless, but shows how many moving parts `serde_json 1.0.149` ties together.

7. **Two `aws-lc-sys` versions** (`0.41.0`, `0.46.0`) and **two `iroh` versions** (`0.35.0`,
   `1.0.3`) are in the graph — a large, duplicated native/crypto tree. `iroh 1.0.3` is the one that
   forces rustc 1.91.

8. **`Client::start_executor` takes `&Arc<Self>`**, so it cannot be called on a bare `&Client`
   (the plan's prose implies `&self`).

9. **`EsploraClient` is a doubly-wrong name**: neither the type nor the `scan` method exists. The
   real surface is the `EsploraAsyncExt`/`EsploraExt` traits with `full_scan` (full) and `sync`
   (partial). Anyone coding from the plan would write non-compiling code.

10. **`ClientPreview::join` yields a `ClientHandle`**, and `Client` itself is not directly
    constructible — the plan's `build_client(...) -> ClientHandleArc` sketch is close but the
    return type is `ClientHandle`.

---

## 7. How to reproduce

```bash
# one-time environment (see §0)
rustup toolchain install 1.91 --profile minimal
uv tool install cmake
uv pip install --target /tmp/opencode/lc libclang        # glibc-compatible libclang

# build/type-check (rust-toolchain.toml + .cargo/config.toml already encode the rest)
cargo check          # -> Finished, exit 0
```

Files added by this verification: `Cargo.toml`, `Cargo.lock`, `src/main.rs`,
`rust-toolchain.toml`, `.cargo/config.toml`, `.gitignore`.
