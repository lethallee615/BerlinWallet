# AGENTS.md — durable project context: Berlin Wallet

## Read first

Every new session must read this file, [`docs/STATUS.md`](docs/STATUS.md), and
[`docs/JOURNAL.md`](docs/JOURNAL.md) before changing code, docs, or the roadmap.
`AGENTS.md` holds current scope and working rules; `STATUS.md` is the dated
snapshot and the open queue lives in GitHub Issues; the journal explains how the
project got here. [`docs/TRACKING.md`](docs/TRACKING.md) defines the labels,
milestones, status vocabulary, and the agent protocol. None of these replaces the
source or `docs/API-VERIFICATION.md` for current API behavior.

## Current scope (2026-10-30)

The near-term product is a **CLI-first MVP**. Desktop GUI and mobile are deferred
until after the MVP.

In scope:

- On-chain BDK send/receive.
- Fedimint `walletv2` peg-in/peg-out and `mintv2` ecash balance.
- Lightning send/receive through `fedimint-lnv2-client`.
- Three BDK chain backends: Esplora, Electrum, and bitcoind RPC.
- Restore/backup, crash safety, deposit fee preview, `watch` progress, and age-encrypted mnemonic/backups.

Current reality: peg-in/peg-out and ecash balance are live-tested; the combined
`deposit` path is fixed (adoption/watcher race) and was live-gated on 2026-10-31.
Lightning (`lnv2`) is **registered**, with paid send/receive, expiry, and
restart-adoption live-gated on devimint (§49); a pre-lnv2 client data directory
needs a fresh join/restore to gain the module. All three BDK chain backends are
wired behind `chain::ChainBackend`; Esplora, Electrum, and bitcoind RPC all have
live coverage (bitcoind's reorg path was verified on 2026-10-24; Electrum's
sync/deposit/withdrawal/restart/`chain.json` gate on 2026-10-31).
The BDK path has a first-class on-chain send with a fee quote and history. The
crash matrix is a deterministic CI suite (`src/crash_matrix.rs`); the
broadcast-before-BDK/journal-persistence window remains devimint-only. See `ROADMAP.md` for the MVP
checklist and `docs/STATUS.md` for the current snapshot.

## Crate layout

```
berlin_wallet/                  # workspace root
  Cargo.toml                    # workspace definition (edition 2024, rust 1.91 MSRV)
  crates/
    berlin-wallet-core/         # pure Rust orchestration (no uniffi, no FFI types)
      src/lib.rs
      src/client.rs             # Fedimint Client construction + module registration
      src/bdk.rs                # BDK wallet, BIP84, sync, reorg anchor check, DB swap
      src/chain/mod.rs          # ChainBackend trait, ChainKind/Tip/Options, ChainError
      src/chain/config.rs       # chain-source parsing + backend construction
      src/chain/esplora.rs      # Esplora backend
      src/chain/electrum.rs     # Electrum backend
      src/chain/bitcoind.rs     # bitcoind JSON-RPC backend (block-walk, birthday)
      src/chain/persist.rs      # chain.json resolution (never-silent backend switch)
      src/deposit.rs            # deposit request/address/funding/watch/adoption
      src/withdrawal.rs         # quote/send/reconcile/adoption
      src/lightning.rs          # lnv2 send/receive, quotes, adoption, expiry
      src/journal.rs            # row types, state enums, Journal trait
      src/journal_db.rs         # versioned DB journal, tags, atomic transitions
      src/sync.rs               # BDK, fedimint, and adoption tasks
      src/backup.rs             # age-encrypted full-directory backup/import
      src/crash_matrix.rs       # test-only deterministic crash/restart matrix
      src/seed.rs               # BIP39, canonical root secret, client identity
      src/network.rs            # chain-id -> bitcoin::Network, recorded choice
      src/restore_policy.rs     # restore warnings + balances-only assertions
      src/sqlite.rs             # SQLite Database backend (mobile)
      src/wire.rs               # FeeQuoteWire
      src/error.rs              # PegError and transient/permanent classification
      src/ops.rs                # DepositId / WithdrawalId
      src/fault.rs              # debug-only crash injection
    berlin-wallet-ffi/          # UniFFI crate (depends on berlin-wallet-core)
      src/lib.rs                # uniffi::export, PegWallet object
      src/bin/uniffi-bindgen.rs
    berlin-wallet-cli/          # development REPL (`berlin-wallet`)
```

## Pinned dependencies

Versions below are verified against the crates.io sparse index + the committed `Cargo.lock`
(see `docs/API-VERIFICATION-FIRST-PASS.md` §3).

| Crate | Version | Role |
|---|---|---|
| `fedimint-client` | `=0.12.0` | federation client, `Client`, `ClientBuilder` |
| `fedimint-mintv2-client` | `=0.12.0` | ecash mintv2 module (`MintClientInit`) — primary BTC module; v1 `fedimint-mint-client` is not registered |
| `fedimint-mintv2-common` | `=0.12.0` | `MintClientConfig.fee_consensus` for the deposit fee preview |
| `fedimint-walletv2-client` | `=0.12.0` | walletv2 receive/send module (`WalletClientInit`, `WalletClientModule`) |
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
| `age` | `=0.12.1` | passphrase encryption for the mnemonic and backup archive |
| `tar` | `=0.4.46` | full-directory backup archive |
| `rpassword` | `=7.5.4` | CLI passphrase prompt |

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

Working setup (see `docs/API-VERIFICATION-FIRST-PASS.md` §0 for the alternatives that failed):

```bash
rustup toolchain install 1.91 --profile minimal --component rustfmt --component clippy
# Install cmake + a C++17 compiler + libclang for your platform.
# LIBCLANG_PATH and BINDGEN_EXTRA_CLANG_ARGS come from the shell; re-derive them
# per machine (see docs/API-VERIFICATION.md §40).
cargo check   # -> exit 0
```

## Architecture

- One `fedimint_client::Client` per federation, stored in an `Arc<RwLock>` map keyed by `FederationId` (mirrors douglaz `MultiClient`).
- Storage: one shared `Database`; fedimint client `i` lives at `db.with_prefix([0x01] ++ u32_le(db_prefix))`; the app journal lives under a separate prefix `[0x00]`. The CLI uses the RocksDB backend at `<data-dir>/fedimint.rocksdb` (`fedimint-rocksdb` git tag `v0.12.0` with all fedimint crates patched); the hand-rolled `SqliteDb` remains for mobile. Production can inject any `Database`.
- Crash-safe journal: raw byte API + serde_json versioned envelopes (`ROW_VERSION = 1`); rows keyed `[tag] ++ id_bytes`; intent + pending index committed in ONE `dbtx` (`begin_transaction … commit_tx`); idempotency keys + deterministic `OperationId`s; `FederationInfo` registry rows so restart can reopen existing partitions rather than re-joining.
- Root secret: canonical `Bip39RootSecretStrategy` (mirrored from fedimint-bip39 `v0.12.0`) → `RootSecret::StandardDoubleDerive`; BDK uses the same BIP39 seed via BIP84, with no raw key reuse.
- Chain backends: `BdkWallet` owns the `PersistedWallet<Connection>` and is the only place that calls `persist()`. A `chain::ChainBackend` implementation receives `&mut Wallet`, stages changes, and does nothing else. **Hard rule: a chain backend never persists and never owns the BDK database.** Persistence ordering is what makes the reorg rebuild/rename crash-safe, so it cannot be delegated. The reorg anchor check (`tip` + `block_hash_at` + the pure `reorg_decision`) also lives in `BdkWallet::sync`, above the backend, so every backend gets the same behaviour. `ChainError::{Transient, Permanent}` is one enum over one classifier *per backend*, not a shared upstream contract.
- Two long-running tokio sync tasks sharing the journal (plus the adoption task):
  - BDK chain sync: `Wallet::start_sync_with_revealed_spks()` → source an `Update` → `apply_update` → `take_staged` → persist via `WalletPersister`. For `bdk_esplora` the update comes from `bdk_esplora::EsploraAsyncExt::sync` (partial) or `::full_scan` (full) — there is **no `EsploraClient` type and no `scan` method**. Then reconcile withdrawal txids to operations.
  - Fedimint federation sync: `spawn_federation_reconnect()` + `start_executor()` + `handle_events` / `get_event_log_transient_receiver()`; record deposit credits and withdrawal broadcasts in the journal.
- Peg-in: `WalletClientModule::receive()` → BDK pays the deposit address → `await_receive(position)` → operation-log claim scan → `claim_bind` → ecash credited through mintv2; `net_credit` uses the claim fee and `Client::get_operation_fees`. Funding returns after broadcast; a per-row background observer (`sync::DepositWatchers`) follows the claim, is resumed from the journal at boot, and emits a WARN if a funded row is still unclaimed after five minutes. The observer only observes: walletv2's own output scanner performs the claim. `funding_outpoint` is recorded from the built funding transaction, not only at claim time.
- Peg-out: `send_fee` / `send_fee_quote(value + fee)` → `WalletClientModule::send(address, value, fee, custom_meta)` → `await_final_send_operation_state(operation_id)` → bounded **ecash** change-output wait → journal `Broadcast`; external destinations reconcile from the federation txid, not a BDK scan. `SendMeta.change_outpoint_range` is a fedimint `OutPointRange`, not a Bitcoin txid.
- Reorg policy: no `Reorged` journal states. Walletv2 finalizes receive outputs at `tip − 6` and has an append-only output log; `Success` means claim accepted, not confirmed; send `Success(txid)` is broadcast/acceptance-only. BDK `LocalChain::apply_update`/`merge_chains` handles checkpoint reorgs, but a lower remote tip can be a no-op; `BdkWallet::sync` verifies ancestry and rebuilds/full-scans on an orphaned checkpoint; the swap checkpoints/closes both DBs, atomically renames, reopens a new connection against the final path, and preserves max(next-index). Deterministic reorg tests require devimint/bitcoind `invalidateblock`, not Esplora.
- Restore (fedimint-client 0.12.0): `ClientPreview::recover` → `wait_for_all_recoveries` → `ClientHandle::shutdown` → `ClientBuilder::open` → `Client::wait_for_all_active_state_machines` (the recovered-notes output state machine must settle before the balance is read). Fresh recovery restores **balances only**, leaves the local operation log empty, and does not resume in-flight operations; `restore_policy` owns the exact CLI/FFI warnings and the balances-only assertion. The recovery artifact is **mnemonic + client data directory**, not mnemonic alone. Deposit/withdrawal lists are empty by design; adoption only applies when the client DB (operation log) is retained. A new `receive()` after restore does **not** re-claim a stranded older deposit; the UI must warn. The mnemonic is age-passphrase-encrypted at rest; a legacy plaintext file is migrated on the first passphrase load. The CLI provides `backup`/`restore-backup` for a single age-encrypted tar including the encrypted mnemonic, SQLite WAL/SHM sidecars, and the RocksDB directory; it try-locks `fedimint.db.lock` and fails if the DB is live. The live databases rely on full-disk encryption; a journal-only export is not sufficient. BDK restore must use `start_full_scan`; `stop_gap = 20` is only a floor.
- Logging: CLI installs a stderr `tracing_subscriber` with a reloadable `EnvFilter` (`RUST_LOG`, default `info`) and idempotent `try_init`; immediately before shutdown it appends `fm::task=off` so expected task-cancellation noise is not logged, while normal task errors stay visible. FFI does not install a subscriber (host owns it). The BDK orphaned checkpoint path emits an INFO `BDK checkpoint is orphaned; rebuilding wallet DB and running full rescan`.
- Thin UniFFI crate: `#[derive(uniffi::Object)] PegWallet` with `new/init/open/join/restore/restore_backup/backup/shutdown`, deposit and withdrawal request/status/cancel/list methods, and `balance_sat/ecash_balance_msat/min_withdrawal_sat`; one `PegWalletError` enum. `join` is fresh-only; `open(chain_source: Option<String>, passphrase: String)` reopens an existing DB without an invite and falls back to the stored `chain.json`; `init`, `join`, `open`, `restore`, `backup`, and `restore_backup` take passphrases. `WalletInfo` reports `chain_source`/`chain_kind`. `request_deposit` spawns the address task and returns. `request_withdrawal` persists the quote; `WithdrawalView` exposes `quoted_fee_sat`, quote input/output/dust msat, and `total_debit_msat` (total debit, not net) and `recipient_amount_sat` for explicit confirmation before send. `shutdown()` is terminal for that instance (all other methods error; make a new instance and open) and stops tasks/client and releases locks; mobile must call it on backgrounding. Fedimint-client's `uniffi` feature is deliberately **not** enabled; use `ClientHandleArc` + Deref/`get_first_module_arc`, not `inner_arc()`. One UniFFI namespace.

## Confirmed API names (cite these, do not invent)

Compile-verified against the real crates; see `docs/API-VERIFICATION-FIRST-PASS.md` §1 for full signatures and source locations.

**fedimint-client 0.12.0**: `Client::builder()`, `ClientBuilder::with_module()`, `ClientBuilder::preview()`, `ClientPreview::join()` / `ClientPreview::recover(...)` (**both return `ClientHandle`, not `Client`**), `Client::wait_for_all_recoveries()`, `Client::all_modules_usable()`, `ClientHandle::shutdown(self)`, `Client::get_balance_for_unit()`, `get_operation_fees(operation_id)`, `wait_for_all_active_state_machines()`, `get_first_module::<M>()`, `get_first_module_arc::<M>()`, `finalize_and_submit_transaction()`, `transaction_updates()`, `await_primary_bitcoin_module_output()`, `subscribe_balance_changes()`, `spawn_federation_reconnect()`, `start_executor()` (**receiver is `self: &Arc<Self>`**), `db()`, `executor()`, `handle_events()`, `get_event_log_transient_receiver()`, `chain_id()`, `federation_id()`, `config()`.
- `OutPointRange` = `fedimint_core::OutPointRange` (crate root)
- `TransactionBuilder` = `fedimint_client::module::transaction::TransactionBuilder`
- `OperationId` = `fedimint_core::core::OperationId`

> The v1 `fedimint-wallet-client` names below are kept as historical reference. This workspace registers `fedimint-walletv2-client`; use the v2 flow above and verify exact names against the pinned source before coding.

**fedimint-wallet-client 0.12.0**: `WalletClientModule::allocate_deposit_address_pooled()`, `allocate_deposit_address_expert_only()`, `safe_allocate_deposit_address()`, `peg_in(PegInRequest)`, `withdraw()`, `get_withdraw_fees()`, `send_fee_quote()`, `max_withdrawable_amount()`, `subscribe_deposit()`, `subscribe_withdraw_updates()`, `create_withdraw_output()`, `WalletClientInit` (`= WalletClientInit(pub Option<DynBitcoindRpc>)`).
- `PegOutFees` = `fedimint_wallet_client::common::PegOutFees`

**fedimint-mintv2-client 0.12.0**: `MintClientInit`/`MintClientModule`; v1 `fedimint-mint-client` is not registered. The live federation exposes `lnv2`/`meta`/`mintv2`/`walletv2`.

**fedimint-lnv2-client 0.12.0**: register `LightningClientInit::default()` (it discovers federation gateways; no per-client URL). `LightningClientModule::{send, receive, await_final_send_operation_state, await_final_receive_operation_state, get_invoice_send_status, select_gateway, list_gateways, routing_info, send_fee_quote, receive_fee_quote, spendable_amount}`; `FinalSendOperationState::{Success, Refunded, Failure}`; `FinalReceiveOperationState::{Expired, Claimed, Failure}`; `LightningOperationMeta::{Send, Receive, LnurlReceive}`; events `payment-send`, `payment-send-update`, `payment-receive`. Source-verified §48; live gate §49. Existing client DBs need a fresh join to gain the module.

**bdk_wallet 3.2.0**: `Wallet::create()` (→ `CreateParams`), `Wallet::load()` (→ `LoadParams`), `reveal_next_address()`, `next_unused_address()`, `list_unspent()`, `get_utxo()`, `get_tx()`, `build_tx()`, `create_psbt()` (requires `bdk-tx` + `--cfg bdk_wallet_unstable`), `sign_with_signers()`, `finalize_psbt()`, `take_staged()`, `apply_update()`, `balance()`, `calculate_fee()`, `insert_txout()`, `start_sync_with_revealed_spks()`, `start_full_scan()` (restore), `apply_update_events()` (`WalletEvent::TxConfirmed`), `WalletPersister`/`PersistedWallet` (`PersistedWallet::create(&mut P, CreateParams)` / `PersistedWallet::load(&mut P, LoadParams)`; rusqlite or bdk_file_store).
- `Update` = `bdk_wallet::Update` (crate root — **not** `bdk_wallet::chain::Update`)

**bdk_esplora 0.22.1** (chain source): trait `bdk_esplora::EsploraAsyncExt` (blocking: `EsploraExt`), impl'd for `esplora_client::AsyncClient` / `BlockingClient`. Methods: `full_scan(request, stop_gap, parallel_requests)` (full) and `sync(request, parallel_requests)` (partial). There is **no `EsploraClient` type and no `scan` method**. `bdk_esplora::esplora_client` is re-exported.

**fedimint-core 0.12.0**: `Database::with_prefix(&self, prefix: Vec<u8>) -> Self`; `AmountUnit` = `fedimint_core::module::AmountUnit`.

**fedimint-client-module 0.12.0**: `ClientModuleInit` = `fedimint_client::module::module::init::ClientModuleInit` (≡ `fedimint_client_module::module::init::ClientModuleInit`); `FeeQuote` = `fedimint_client::module::transaction::FeeQuote`.

**douglaz/fedimint-wallets (crash-safe patterns)**: `MultiClient` per-federation clients at `[0x01] ++ u32_le(db_prefix)`, `join_lock`, `active_recoveries` reservation, `open_all` best-effort, `wait_for_all_active_state_machines`, `ClientHandle::shutdown(self)` before replacing a handle; `FedimintJournal` app prefix `[0x00]`, raw byte API + serde_json, atomic status+index transitions; `runtime::Runtime` single actor + watch scheduler.

## Not verified (reference only — not compile-verified)

**fedimint-clientd (reference daemon)**: `AppState::new(db_path)`, `multimint.register_new(invite_code)`, `multimint.all()`; REST endpoints `/v2/onchain/deposit-address`, `/v2/onchain/await-deposit`, `/v2/onchain/withdraw`. Read from the `fedimint/fedimint-clientd` `main.rs` GitHub source; `multimint` is a path crate, not a crates.io dependency, so `docs/API-VERIFICATION-FIRST-PASS.md` did **not** compile-verify these.

## Branch and PR workflow

- Branch from `main`. Use a `feat/`, `fix/`, or `chore/` prefix.
- Open a pull request; do not commit directly to `main`.
- Squash-merge the PR into `main`.
- Keep the final commit message in the repository's existing style.

## Mandatory rule for all future work

**Always read docs.rs + GitHub source before writing code; cite real API names; flag guesses.**

Before touching any crate (fedimint-client, fedimint-mint-client, fedimint-wallet-client, bdk_wallet, uniffi), do this:
1. Open the docs.rs page for the exact pinned version (e.g. https://docs.rs/fedimint-client/0.12.0/).
2. Confirm each function/module/type name you plan to use against the rendered API docs.
3. Where the plan says "guessing at", resolve it by reading the crate source on GitHub (e.g. fedimint/fedimint-client, fedimint/fedimint-clientd, douglaz/fedimint-wallets) before implementing.
4. Never invent function names or module paths — if you can't confirm a name from docs/source, mark it clearly as a guess and either verify or redesign around the confirmed API.

If anything in this AGENTS.md conflicts with the actual crate APIs at build time, update this file to reflect the confirmed reality and proceed from there.
