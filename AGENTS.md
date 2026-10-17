# AGENTS.md — durable project context: Berlin Wallet

## Crate layout

```
berlin_wallet/                  # workspace root
  Cargo.toml                    # workspace definition (edition 2024, rust 1.91 MSRV)
  crates/
    berlin-wallet-core/         # pure Rust orchestration (no uniffi, no FFI types)
      src/lib.rs
      src/client.rs             # Fedimint Client construction + module registration
      src/bdk.rs                # BDK wallet wrapper + chain sync
      src/pegin.rs              # peg-in flow
      src/pegout.rs             # peg-out flow
      src/journal.rs            # crash-safe write-ahead journal (douglaz pattern)
      src/sync.rs               # concurrent BDK chain task + fedimint federation task
      src/ops.rs                # OperationId mapping / idempotency keys
      error.rs
    berlin-wallet-ffi/          # reserved: later UniFFI crate (depends on berlin-wallet-core)
      src/lib.rs                # uniffi::export, PegWallet object
      uniffi/                   # generated bindings via uniffi-bindgen
  crates/
    berlin-wallet-cli/          # optional dev CLI (not required by spec)
```

`berlin-wallet-ffi` is reserved for the later UniFFI crate and is not created by this scaffold.

## Pinned dependencies

Versions below are verified against the crates.io sparse index + the committed `Cargo.lock`
(see VERIFY.md §3).

| Crate | Version | Role |
|---|---|---|
| `fedimint-client` | `=0.12.0` | federation client, `Client`, `ClientBuilder` |
| `fedimint-mint-client` | `=0.12.0` | ecash mint module (`MintClientInit`) |
| `fedimint-wallet-client` | `=0.12.0` | on-chain wallet module (`WalletClientInit`) |
| `fedimint-client-module` | `=0.12.0` | `ClientModule` trait, `ClientModuleInit` |
| `fedimint-connectors` | `=0.12.0` | `ConnectorRegistry` |
| `fedimint-core` | `=0.12.0` | `Database`, `OperationId`, `Amount`, `OutPoint`, `FederationId` |
| `bdk_wallet` | `=3.2.0` | on-chain wallet; features `["bdk-tx", "file_store", "rusqlite"]` (see Build requirements) |
| `bdk_chain` | `=0.23.3` | chain data types |
| `bdk_file_store` | `=0.22.0` | optional flat-file persistence |
| `rusqlite` | `0.31.0` (`bundled`) | optional SQLite persistence (via `bdk_chain/rusqlite`; no system SQLite needed) |
| `bdk_esplora` / `bdk_electrum` / `bdk_bitcoind_rpc` | `0.22.1` / `0.23.2` / `0.22.0` | chain source (pick one at runtime) |
| `uniffi` | `=0.32.0` | UniFFI (optional feature in fedimint-client; required in ffi crate) |
| `uniffi_bindgen` | `0.32.0` | build-time binding generation |
| `tokio` | `1` (resolved `1.53.3`) | async runtime — **NOT `=1.48`**: `fedimint-connectors 0.12.0` → `iroh-mainline-address-lookup ^0.4.0` requires `tokio ^1.49` |
| `anyhow` / `thiserror` | `=1.0.98` / `=2.0.18` | errors |
| `serde` / `serde_json` | `1` (resolved `1.0.229`) / `=1.0.149` | journal serialization — **NOT `serde =1.0.219`**: `serde_json 1.0.149` requires `serde ^1.0.220` |
| `tracing` | `=0.1.44` | logging |
| `async-trait` | `=0.1.88` | async traits |
| `bitcoin` | `=0.32.8` | unified at a single version across fedimint + bdk |
| `bip39` | `=2.2.0` | BIP39 mnemonic/seed generation (direct dep; not `bdk_wallet`'s `keys-bip39`) |

Do **not** enable `bdk_wallet`'s `keys-bip39` feature: it requires `bip39 ^2.2.2`, which is not
published (crates.io index max is `2.2.0`). The core uses `bip39 =2.2.0` directly for the mnemonic
and seed only.

**MSRV: 1.91** (effective). `bdk_wallet` advertises 1.85, but `fedimint-connectors 0.12.0` pulls
`iroh 1.0.3` / `iroh-base 1.3.0` / `netwatch 0.19.3` / `portmapper 0.19.3` / `n0-*`, which all
declare `rust-version = 1.91`. Edition: 2024.

## Build requirements

- **Toolchain 1.91.1** — committed `rust-toolchain.toml` (`channel = "1.91.1"`). The default 1.89
  toolchain cannot build the graph.
- **`cmake`** — required by `aws-lc-sys`, a hard (non-optional) dependency of **both**
  `fedimint-core` and `fedimint-connectors`, which force its `bindgen` feature via the `ssl` feature.
- **`libclang`** — required by that bindgen step.
- **bdk PSBT/fee API** — `Wallet::create_psbt` (and `build_tx`) require the `bdk-tx` feature **and**
  `--cfg bdk_wallet_unstable`. `bdk_wallet`'s `src/lib.rs` otherwise emits
  `compile_error!("The \`bdk-tx\` feature requires \`--cfg bdk_wallet_unstable\`.")`.
  This is configured in `.cargo/config.toml`
  (`[build] rustflags = ["--cfg", "bdk_wallet_unstable"]`) plus `bdk_wallet` features
  `["bdk-tx", "file_store", "rusqlite"]`.

Working setup in this sandbox (see VERIFY.md §0 for the alternatives that failed):

```bash
rustup toolchain install 1.91 --profile minimal
uv tool install cmake
uv pip install --target /tmp/opencode/lc libclang   # glibc-compatible libclang (PyPI)
# LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS are set in .cargo/config.toml [env]
cargo check   # -> exit 0
```

## Architecture

- One `fedimint_client::Client` per federation, stored in an `Arc<RwLock>` map keyed by `FederationId` (mirrors douglaz `MultiClient`).
- Storage: one shared `Database`; fedimint client `i` lives at `db.with_prefix([0x01] ++ u32_le(db_prefix))`; the app journal lives under a separate prefix `[0x00]`. The CLI uses the RocksDB backend at `<data-dir>/fedimint.rocksdb` (`fedimint-rocksdb` git tag `v0.12.0` with all fedimint crates patched); the hand-rolled `SqliteDb` remains for mobile. Production can inject any `Database`.
- Crash-safe journal: raw byte API + serde_json versioned envelopes (`ROW_VERSION = 1`); rows keyed `[tag] ++ id_bytes`; intent + pending index committed in ONE `dbtx` (`begin_transaction … commit_tx`); idempotency keys + deterministic `OperationId`s; `FederationInfo` registry rows so restart can reopen existing partitions rather than re-joining.
- Root secret: canonical `Bip39RootSecretStrategy` (mirrored from fedimint-bip39 `v0.12.0`) → `RootSecret::StandardDoubleDerive`; BDK uses the same BIP39 seed via BIP84, with no raw key reuse.
- Two long-running tokio sync tasks sharing the journal (plus the adoption task):
  - BDK chain sync: `Wallet::start_sync_with_revealed_spks()` → source an `Update` → `apply_update` → `take_staged` → persist via `WalletPersister`. For `bdk_esplora` the update comes from `bdk_esplora::EsploraAsyncExt::sync` (partial) or `::full_scan` (full) — there is **no `EsploraClient` type and no `scan` method**. Then reconcile withdrawal txids to operations.
  - Fedimint federation sync: `spawn_federation_reconnect()` + `start_executor()` + `handle_events` / `get_event_log_transient_receiver()`; record deposit credits and withdrawal broadcasts in the journal.
- Peg-in: `WalletClientModule::allocate_deposit_address_pooled` → BDK pays deposit address → `subscribe_deposit` / `await_primary_bitcoin_module_output` → ecash minted via mint module.
- Peg-out: `get_withdraw_fees` → `withdraw(address, amount, fee: PegOutFees, extra_meta)` → `subscribe_withdraw_updates` → BDK chain sync detects withdrawal tx → reconcile in journal.
- Thin UniFFI crate: `#[uniffi::export]` `PegWallet` object (`join_federation`, `balance_sat`, `peg_in_address`, `peg_in`, `peg_out`, `operation_status`, `sync`); errors mapped to a uniffi-compatible `thiserror` enum (not `anyhow`).

## Confirmed API names (cite these, do not invent)

Compile-verified against the real crates; see VERIFY.md §1 for full signatures and source locations.

**fedimint-client 0.12.0**: `Client::builder()`, `ClientBuilder::with_module()`, `ClientBuilder::preview()`, `ClientPreview::join()` (**returns `ClientHandle`, not `Client`**), `Client::get_balance_for_unit()`, `get_first_module::<M>()`, `get_first_module_arc::<M>()`, `finalize_and_submit_transaction()`, `transaction_updates()`, `await_primary_bitcoin_module_output()`, `subscribe_balance_changes()`, `spawn_federation_reconnect()`, `start_executor()` (**receiver is `self: &Arc<Self>`**), `db()`, `executor()`, `handle_events()`, `get_event_log_transient_receiver()`, `chain_id()`, `federation_id()`, `config()`.
- `OutPointRange` = `fedimint_core::OutPointRange` (crate root)
- `TransactionBuilder` = `fedimint_client::module::transaction::TransactionBuilder`
- `OperationId` = `fedimint_core::core::OperationId`

**fedimint-wallet-client 0.12.0**: `WalletClientModule::allocate_deposit_address_pooled()`, `allocate_deposit_address_expert_only()`, `safe_allocate_deposit_address()`, `peg_in(PegInRequest)`, `withdraw()`, `get_withdraw_fees()`, `send_fee_quote()`, `max_withdrawable_amount()`, `subscribe_deposit()`, `subscribe_withdraw_updates()`, `create_withdraw_output()`, `WalletClientInit` (`= WalletClientInit(pub Option<DynBitcoindRpc>)`).
- `PegOutFees` = `fedimint_wallet_client::common::PegOutFees`

**fedimint-mint-client 0.12.0**: `MintClientModule::reissue_external_notes()`, `send_oob_notes()`, `spend_notes_with_selector()`, `create_output()`, `await_output_finalized()`, `get_note_counts_by_denomination()`, `estimate_spend_all_fees()`, `MintClientInit`.

**bdk_wallet 3.2.0**: `Wallet::create()` (→ `CreateParams`), `Wallet::load()` (→ `LoadParams`), `reveal_next_address()`, `next_unused_address()`, `list_unspent()`, `get_utxo()`, `get_tx()`, `build_tx()`, `create_psbt()` (requires `bdk-tx` + `--cfg bdk_wallet_unstable`), `sign_with_signers()`, `finalize_psbt()`, `take_staged()`, `apply_update()`, `balance()`, `calculate_fee()`, `insert_txout()`, `start_sync_with_revealed_spks()`, `apply_update_events()` (`WalletEvent::TxConfirmed`), `WalletPersister`/`PersistedWallet` (`PersistedWallet::create(&mut P, CreateParams)` / `PersistedWallet::load(&mut P, LoadParams)`; rusqlite or bdk_file_store).
- `Update` = `bdk_wallet::Update` (crate root — **not** `bdk_wallet::chain::Update`)

**bdk_esplora 0.22.1** (chain source): trait `bdk_esplora::EsploraAsyncExt` (blocking: `EsploraExt`), impl'd for `esplora_client::AsyncClient` / `BlockingClient`. Methods: `full_scan(request, stop_gap, parallel_requests)` (full) and `sync(request, parallel_requests)` (partial). There is **no `EsploraClient` type and no `scan` method**. `bdk_esplora::esplora_client` is re-exported.

**fedimint-core 0.12.0**: `Database::with_prefix(&self, prefix: Vec<u8>) -> Self`; `AmountUnit` = `fedimint_core::module::AmountUnit`.

**fedimint-client-module 0.12.0**: `ClientModuleInit` = `fedimint_client::module::module::init::ClientModuleInit` (≡ `fedimint_client_module::module::init::ClientModuleInit`); `FeeQuote` = `fedimint_client::module::transaction::FeeQuote`.

**douglaz/fedimint-wallets (crash-safe patterns)**: `MultiClient` per-federation clients at `[0x01] ++ u32_le(db_prefix)`, `join_lock`, `active_recoveries` reservation, `open_all` best-effort, `wait_for_all_active_state_machines`, `Client::shutdown()` before replacing a handle; `FedimintJournal` app prefix `[0x00]`, raw byte API + serde_json, atomic status+index transitions; `runtime::Runtime` single actor + watch scheduler.

## Not verified (reference only — not compile-verified)

**fedimint-clientd (reference daemon)**: `AppState::new(db_path)`, `multimint.register_new(invite_code)`, `multimint.all()`; REST endpoints `/v2/onchain/deposit-address`, `/v2/onchain/await-deposit`, `/v2/onchain/withdraw`. Read from the `fedimint/fedimint-clientd` `main.rs` GitHub source; `multimint` is a path crate, not a crates.io dependency, so VERIFY.md did **not** compile-verify these.

## Mandatory rule for all future work

**Always read docs.rs + GitHub source before writing code; cite real API names; flag guesses.**

Before touching any crate (fedimint-client, fedimint-mint-client, fedimint-wallet-client, bdk_wallet, uniffi), do this:
1. Open the docs.rs page for the exact pinned version (e.g. https://docs.rs/fedimint-client/0.12.0/).
2. Confirm each function/module/type name you plan to use against the rendered API docs.
3. Where the plan says "guessing at", resolve it by reading the crate source on GitHub (e.g. fedimint/fedimint-client, fedimint/fedimint-clientd, douglaz/fedimint-wallets) before implementing.
4. Never invent function names or module paths — if you can't confirm a name from docs/source, mark it clearly as a guess and either verify or redesign around the confirmed API.

If anything in this AGENTS.md conflicts with the actual crate APIs at build time, update this file to reflect the confirmed reality and proceed from there.
