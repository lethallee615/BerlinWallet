# Architecture

This document describes the code as it exists in this repository. It is not a
future-design document. Where behavior is unverified or upstream behavior is
unknown, that is stated explicitly.

The short version:

- **BDK is the user's on-chain wallet.** It holds UTXOs, builds and signs the
  transaction that funds a federation deposit address, and maintains its own
  chain checkpoint. It is not the federation's wallet.
- **`walletv2` is the federation peg module.** It gives the client a deposit
  address, runs the receive/claim state machine, and sends withdrawals from the
  federation's on-chain wallet. `mintv2` is the primary BTC ecash module.
- **The journal is the app's crash-safe state store.** It records deposits,
  withdrawals, claim ownership, scan watermarks, single-open indexes, and
  pending net-credit rows. Its transitions are atomic database transactions; the
  external blockchain/federation effects around them are not one transaction.

## 1. Crate boundaries and direction

```text
berlin-wallet-core
  ├─ no UniFFI, no platform UI types
  ├─ fedimint-client + fedimint-walletv2-client + fedimint-mintv2-client
  └─ bdk_wallet + bdk_esplora + rusqlite
        │
        ├─ berlin-wallet-ffi (UniFFI object `PegWallet`)
        │     └─ desktop GUI / mobile app (future)
        │
        └─ berlin-wallet-cli (development REPL)
```

`berlin-wallet-core` is the only place that owns wallet state and state-machine
decisions. The FFI crate maps core rows to UniFFI records and owns no money
logic. The CLI is a manual test harness; it is intentionally not a production
UI.

### Core modules

| File | Responsibility |
|---|---|
| `crates/berlin-wallet-core/src/lib.rs` | Public module exports and `Journal`/row type re-exports. |
| `client.rs` | `ClientBuilder` setup, `mintv2` + `walletv2` registration, fresh join, existing open, mnemonic recovery, RocksDB/SQLite database constructors, wallet config/dust lookup. |
| `bdk.rs` | BIP84 descriptors, `PersistedWallet<Connection>`, address reveal, funding transaction build/sign/broadcast, full scan, partial sync, ancestry/reorg recovery and the DB swap. Persistence lives here and never in the chain backend. |
| `chain/mod.rs` | `ChainBackend` trait, `ChainKind`, `ChainTip`, `FullScanOptions`, `ChainError` kinds; re-exports the config types. |
| `chain/config.rs` | Chain-source parsing (`esplora:`/`electrum:`/`bitcoind:`), `canonical()`/`describe()` for `chain.json` and logs, backend construction. |
| `chain/esplora.rs` | Esplora (`bdk_esplora` async) sync, full scan, tip lookup, broadcast. |
| `chain/electrum.rs` | Electrum (`bdk_electrum`, blocking behind `spawn_blocking`) sync, full scan, tip lookup, broadcast. |
| `chain/bitcoind.rs` | bitcoind JSON-RPC (`bdk_bitcoind_rpc::Emitter`) block-walk sync, bounded full scan, mempool/eviction updates. |
| `chain/persist.rs` | `chain.json` load/store/resolve and the never-silent backend-switch rule. |
| `deposit.rs` | Deposit request, address task, funding, `await_receive` watcher, operation-log claim scan, orphan adoption, net-credit reconciliation. |
| `withdrawal.rs` | Quote persistence, dust validation, send, final send state, change-output wait, external-address reconciliation, Send adoption. |
| `journal.rs` | `DepositRow`, `WithdrawalRow`, state enums, `JournalBackend`, in-memory backend, net-credit math. |
| `journal_db.rs` | DB-backed journal, versioned envelopes, tag layout, indexes, atomic transitions, optimistic-conflict retries. |
| `sync.rs` | BDK chain sync task, Fedimint keepalive/event/reconcile task, adoption task. |
| `backup.rs` | Full data-directory backup/import and lock check. |
| `sqlite.rs` | Hand-written `SqliteDb` implementing Fedimint's raw database traits; retained for mobile. |
| `seed.rs` | BIP39 mnemonic creation/loading, canonical Fedimint root secret, client identity digest. |
| `network.rs` | Chain-id (block-1 hash) to `bitcoin::Network`; unknown chain IDs map to `Regtest`. |
| `wire.rs` | Serde-compatible `FeeQuoteWire`; `FeeQuote` itself is not `Serialize` in 0.12.0. |
| `error.rs` | `PegError` and transient/permanent error classification. |
| `fault.rs` | Debug-only `BERLIN_WALLET_CRASH_POINT` injection; no-op in release builds. |
| `ops.rs` | `DepositId` and `WithdrawalId` UUIDv7 strings. |

The FFI crate (`crates/berlin-wallet-ffi/src/lib.rs`) exposes one
`#[derive(uniffi::Object)] PegWallet` with async methods, records
`InitView`, `WalletInfo`, `DepositView`, `WithdrawalView`, `RestoreView`, and one
error enum `PegWalletError::Message { message }`. It deliberately does **not**
enable `fedimint-client`'s `uniffi` feature and keeps a single namespace. The FFI
does not install a tracing subscriber; the host owns logging.

The CLI (`crates/berlin-wallet-cli/src/main.rs`) opens RocksDB, installs a
stderr `tracing_subscriber`, starts the same three tasks, and exposes commands
such as `request-deposit`, `fund-deposit`, `watch`, `request-withdraw`,
`send-withdraw`, `backup`, and `restore-backup`.

## 2. Data directory and database layout

The CLI's default data directory is `.berlin-wallet`; `BERLIN_WALLET_DATA_DIR`
overrides it. The FFI constructor takes the directory as a string. The directory
contains:

```text
<data-dir>/
  mnemonic                12-word BIP39 phrase, plaintext, mode 0600
  chain.json              selected chain source, mode 0600 (see below)
  bdk.sqlite              BDK wallet + chain checkpoint (SQLite)
  bdk.sqlite-wal/-shm     SQLite sidecars while the DB is live
  fedimint.rocksdb/       Fedimint client + app journal (RocksDB directory)
  fedimint.db.lock        advisory lock used by fedimint-rocksdb / backup
```

`chain.json` is a versioned `{"version":1,"source":"<canonical chain source>"}`
envelope. It exists because a chain source used to be a per-call Esplora URL, so
a restart could not disagree with itself; with more than one backend, silently
opening an Electrum-backed wallet against the default Esplora URL would hide a
misconfiguration. `resolve_chain_config` prefers an explicit caller value,
otherwise the stored one, and logs a warning when the two differ. It can contain
bitcoind credentials in the URL, which is the same plaintext-at-0600 posture as
`mnemonic`.

`berlin-wallet-core` has two database backends:

- `open_rocksdb` via `fedimint-rocksdb` at the Fedimint `v0.12.0` Git tag. The
  CLI and FFI use this by default.
- `open_sqlite` using the local `SqliteDb` implementation. It implements the
  raw database traits and optimistic version checks, and is retained for
  embedded/mobile hosts.

Both feed the same `Database` abstraction. The app partitions it with
`Database::with_prefix`:

```text
[0x00]                      app journal
[0x01] ++ u32_le(0)         fedimint client 0
```

The current CLI and FFI use client partition `0` and one active federation per
data directory. The core has the database helpers for multiple client
partitions, but a multi-federation manager/UI is not implemented.

The data directory is assumed to be opened by **one process at a time**. RocksDB
and SQLite provide their own file locking, but there is no application-level
multi-process coordinator. Running two wallet processes against the same
directory is unsupported and unsafe.

## 3. Keys, network, and client identity

`seed::load_or_create_mnemonic` creates a 12-word English BIP39 mnemonic if
`<data-dir>/mnemonic` is absent, writes it in plaintext, and sets mode `0600` on
Unix. `mnemonic_seed` returns the standard 64-byte BIP39 seed with an empty
passphrase.

Fedimint's root secret is derived with the canonical strategy mirrored from
`fedimint-bip39` v0.12.0:

```text
DerivableSecret::new_root(
    mnemonic.to_seed_normalized(""),
    b"Fedimint Client Salt",
)
```

`seed::root_secret_from_mnemonic` wraps that as
`RootSecret::StandardDoubleDerive`. The golden-vector test
`seed::tests::bip39_matches_canonical_golden_vector` pins the result.

The same BIP39 seed is used for BDK, but through BIP84:
`wpkh(master/84'/coin_type'/0'/0/*)` and
`wpkh(master/84'/coin_type'/0'/1/*)` in `bdk::descriptors_from_seed`. Fedimint
applies its own salted derivation; no raw key material is shared. A compromise
of the mnemonic compromises both wallets, which is why the threat model treats
the seed as one asset.

`network::client_network` reads `Client::chain_id()` and maps known block-1
hashes:

- Bitcoin mainnet → `Network::Bitcoin`
- testnet → `Network::Testnet`
- signet-4 and Mutinynet → `Network::Signet`
- anything else → `Network::Regtest`

The fallback to regtest is a real operational hazard: an unknown federation
chain does not fail closed. Verify the federation and chain before funding.

`seed::client_identity` derives the same client secret path used by
`fedimint_client_module::secret::get_default_client_secret` and returns a
SHA-256 digest. It is safe to display; the underlying derived secret is not.

## 4. BDK wallet and walletv2 are different wallets

This split is central to understanding the code:

- **Deposit / peg-in.** The user wants ecash. The federation's `walletv2` module
  supplies a deposit address from `WalletClientModule::receive()`. The local BDK
  wallet builds, signs, and broadcasts a transaction paying that address with
  `BdkWallet::build_and_finalize`. The user's BTC leaves the BDK wallet and goes
  to the federation's peg address. The receive operation then claims it into
  ecash.
- **Withdrawal / peg-out.** The user supplies an external Bitcoin address. The
  federation's `walletv2` module sends from the federation's on-chain wallet.
  The destination is usually **not** one of the local BDK addresses, so
  reconciliation uses the federation's
  `FinalSendOperationState::Success(txid)`, not a BDK script scan.
- **BDK chain sync** is still needed to maintain the user's on-chain balance,
  fund deposits, detect reorgs, and reveal addresses. It does not own federation
  withdrawal reconciliation.

`fedimint-walletv2-client` is registered with `WalletClientInit`; the v1
`fedimint-wallet-client` is not registered. `fedimint-mintv2-client` is
registered with `MintClientInit` and is the primary BTC module. `lnv2` is in the
CLI-first MVP scope but is not registered yet.

## 5. Journal format and invariants

`Journal` is a thin handle over `Arc<dyn JournalBackend>`. `Journal::new` creates
the DB-backed `DbJournal`; `Journal::in_memory` is for tests. The app journal is
stored under the `[0x00]` prefix.

### 5.1 Envelope versioning

Each value is a serde JSON envelope:

```rust
struct Envelope<T> {
    version: u8,
    value: T,
}
```

`ROW_VERSION = 1`; `decode` rejects any other version. This is a keyed state
store, not an append-only event log: a transition usually overwrites the row
value. The version byte exists so a future format change can be rejected
explicitly rather than misread.

### 5.2 Prefix tags

Tags are constants in `crates/berlin-wallet-core/src/journal_db.rs`:

| Tag | Key layout | Meaning |
|---|---|---|
| `0x01 TAG_DEPOSIT` | `[0x01] ++ deposit_id` | `DepositRow` |
| `0x02 TAG_WITHDRAWAL` | `[0x02] ++ withdrawal_id` | `WithdrawalRow` |
| `0x03 TAG_CLAIM` | `[0x03] ++ federation_id ++ txid ++ vout_le` | unique owner (`DepositId`) of a funded outpoint |
| `0x04 TAG_WATERMARK` | `[0x04] ++ federation_id` | last scanned operation-log key |
| `0x05 TAG_OPEN_DEPOSIT` | `[0x05] ++ federation_id` | single open deposit id |
| `0x06 TAG_OPEN_WITHDRAWAL` | `[0x06] ++ federation_id` | single open withdrawal id |
| `0x07 TAG_PENDING_NET_CREDIT` | `[0x07] ++ deposit_id` | `Done` row whose `net_credit` is still unknown |
| `0x08 TAG_WITHDRAWAL_OP` | `[0x08] ++ operation_id(32)` | unique withdrawal row for one walletv2 Send |

The claim key includes the federation id and the exact outpoint. The
withdrawal-op key includes the full 32-byte `OperationId`. Ids are UUIDv7 strings
for app-created deposits/withdrawals.

### 5.3 Atomic transitions and retry

Every mutating journal method in `DbJournal` opens a
`DatabaseTransaction`, reads and checks the current state, writes the row and any
index/claim/watermark changes, and commits once. Partial index updates are not
supposed to exist.

The transaction helper `retry_on_conflict` retries the **whole closure** on
`DatabaseError::WriteConflict` or `SnapshotTooOld`, up to 5 attempts, with
jittered exponential backoff. The closure opens a new transaction and re-reads
state on each attempt. This matters for the concurrent open-row and claim cases.

### 5.4 Single-open invariants

There is at most one open deposit and one open withdrawal per federation:

- Open deposit states: `AwaitingAddress`, `AddressShown`.
- Open withdrawal states: `Quoting`, `Submitted`.

`insert_deposit` checks `TAG_OPEN_DEPOSIT`; if present, it returns the existing
row instead of inserting another. The row and open-index are written in the same
transaction. `insert_withdrawal` rejects a second open withdrawal. `cancel_deposit`
only applies while the deposit is open and has no `funding_txid`;
`cancel_withdrawal` only applies to `Quoting`. Terminal/failed/broadcast paths
clear the open index only if it still points at that row
(`clear_open_deposit` / `clear_open_withdrawal`).

This is an application-level single-flight rule, not a promise that the
federation cannot see two requests; it prevents a second local row from being
created while one is open.

### 5.5 `claim_bind`, adoption, and the race

A federation deposit outpoint may be observed more than once: by the foreground
`watch_deposit` pass, by the periodic adoption pass, or by a restarted process.
`TAG_CLAIM` is the exclusive owner key.

`claim_bind(federation_id, outpoint, id)`:

1. Transactionally reads the claim owner.
2. If it exists and is this row, returns `BindResult::Bound`.
3. If it exists for another row, returns `BindResult::TakenBy(other_id)`.
4. Otherwise writes the claim key and the row's `funding_outpoint` in the same
   transaction, then returns `Bound`.

`claim_adopt_if_absent` does the equivalent for an adopted claim: it checks the
claim key and, if absent, creates a new `DepositOrigin::Adopted` row and the claim
key in one transaction. Concurrent callers race on the database transaction; one
wins, the others see `TakenBy`. `bound_outpoints(federation_id)` filters the
operation-log scan so an already-bound outpoint is not offered for adoption
again.

If the foreground watcher loses the bind (`TakenBy`), it closes its open row as
`Done` without crediting it (`close_deposit_done`); the winning row owns the
credit. The deposit credit is therefore tied to one claim key, not to a local
request row.

The `await_receive` return and the local operation-log scan are not one atomic
observation. After `await_receive` returns
`FinalReceiveOperationState::Success`, `watch_deposit` retries
`scan_receive_claims` with backoff. If the claim is not visible yet, the row stays
`AddressShown` so the adoption pass can bind it. No credit is written merely
because `await_receive` returned; the credit path needs the claim metadata/fee
and the claim bind.

### 5.6 Terminal-only watermark

The receive scan pages the operation log backwards with
`OperationLog::paginate_operations_rev(256, last_seen)` and resumes from the
per-federation watermark. It overlays a 32-operation overlap to catch outcomes
that became visible late.

`should_advance_watermark(reached_terminal, state_wait_exhausted)` returns true
only when the scan reached its terminal condition (found the watermark+overlap
boundary or the end of the log) **and** the bounded state-wait budget was not
exhausted. A truncated page or an operation whose final state is not cached yet
leaves the watermark unchanged, so the next pass re-covers the gap. The newest
key is only advanced with a key not older than the previous watermark; an idle
scan cannot move the cursor backwards.

The state wait is bounded by `SCAN_TOTAL_STATE_WAIT = 2 s` and
`SCAN_STATE_WAIT_PER_OP = 500 ms`; this is deliberately conservative, not an
attempt to await every operation to finality inline.

### 5.7 Net credit

`net_credit_with_fees(gross, claim_fee, operation_fee)` computes:

```text
gross sats - on-chain claim fee sats - mintv2 operation fee (msat)
```

It uses saturating subtraction. If `operation_fee` is `None`, the result is
`None`, never a zero-fee estimate. A `Done` row with `net_credit = None` and a
known `operation_id` is indexed under `TAG_PENDING_NET_CREDIT`; the adoption
task calls `Client::get_operation_fees(operation_id)` with bounded retries and
fills `net_credit` only if it was unset. The UI must render `None` as
**pending**.

## 6. Deposit state machine

`DepositState` is defined in `journal.rs`:

```text
AwaitingAddress -> AddressShown -> Done
        |               |
        +-> Failed      +-> Cancelled / Failed
```

The main paths:

1. **Request.** `deposit::request_deposit` checks `open_deposit(federation_id)`;
   if one exists, returns it. Otherwise it reads the next event-log position with
   `Client::get_next_event_log_id`, creates a `DepositRow::requested` with
   `AwaitingAddress`, and inserts it.
2. **Address.** `run_address_task` calls `WalletClientModule::receive()` with a
   120 s timeout and up to 5 attempts. It writes the address with
   `set_deposit_address_if(..., DepositState::AwaitingAddress)`, so a concurrent
   cancel wins and the address is not resurrected.
3. **Funding.** `fund_deposit` returns an existing `funding_txid` if present;
   otherwise it quotes `WalletClientModule::receive_fee()`, builds/signs/broadcasts
   via `BdkWallet::build_and_finalize`, and writes `funding_txid` after the
   broadcast. The journal write is the app's idempotency record for later calls.
4. **Watch.** `watch_deposit` loops on
   `WalletClientModule::await_receive(position)`. `Aborted` stores
   `next_position`, returns the row to `AddressShown`, waits with backoff, and
   loops. Transient errors retry; permanent errors mark `Failed`. A 600 s
   timeout per wait also retries. Only `Success` leaves the loop.
5. **Scan and bind.** On `Success`, it persists `next_position`, scans
   `ReceiveMeta` entries with `scan_receive_claims`, selects a claim by address
   script, and calls `claim_bind`.
6. **Finish.** `Bound` calls `finish_deposit` with the operation id, outpoint,
   gross, claim fee, and (possibly unavailable) operation fee, setting
   `Done` and `net_credit`. `TakenBy` closes the losing row as `Done` without
   credit.
7. **Adoption.** `adopt_orphan_claims` scans the operation log, filters to
   `Success` claims with known outpoints, filters already-bound outpoints, and
   calls `claim_adopt_if_absent`. It also reconciles pending net credits.

Credit is written only by the `finish_deposit`/adoption paths after a successful
receive final state. The address/funding stage never credits ecash.

## 7. Withdrawal state machine

`WithdrawalState` is defined in `journal.rs`:

```text
Quoting -> Submitted -> Broadcast { txid }
   |           |
   +-> Cancelled
               +-> Refunded (final state Aborted)
               +-> Failed   (final state Failure / permanent error)
```

`request_withdrawal`:

- Rejects a second open withdrawal for the federation.
- Parses the caller-supplied address as
  `bitcoin::Address<NetworkUnchecked>`.
- Persists a `Quoting` row with `custom_meta = {"id": "<uuid>"}`.
- Calls `WalletClientModule::send_fee()` and
  `WalletClientModule::send_fee_quote(value)`, then stores the fee and
  `FeeQuoteWire`.

`send_withdrawal`:

1. Reads the row and the federation `dust_limit` from
   `wallet_config(client).dust_limit`; rejects below-dust values before the
   quote/send path and persists `Failed`.
2. Recovers instead of re-sending if the row is already `Submitted` with an
   operation id, or if `find_sent_operation` finds a Send with this
   `custom_meta.id` in the journal/operation log.
3. Re-quotes with `send_fee()` and `send_fee_quote(value)` and compares against
   the persisted quote. On drift it stores the new quote and returns
   `PegError::QuoteChanged`; the caller must re-confirm.
4. Parses the address, calls
   `WalletClientModule::send(destination, value, Some(fresh_fee), custom_meta)`,
   persists the returned `OperationId`, and calls
   `await_final_send_operation_state(operation_id)`.
5. On `Success(txid)`, first waits for the primary-module change outputs with
   `Client::await_primary_bitcoin_module_output` for each
   `SendMeta.change_outpoint_range` outpoint
   (`await_change_outputs`, 20 s bound), then writes `Broadcast { txid }`.
   If the wait times out, the row stays `Submitted` and reconciliation retries.
6. `Aborted` writes `Refunded`; `Failure` writes `Failed`; permanent send errors
   are recorded.

`reconcile_submitted` periodically retries `Quoting`/`Submitted` rows.
`adopt_orphan_sends` scans the full operation log and creates one row per
walletv2 Send operation. The `TAG_WITHDRAWAL_OP` index is the uniqueness rule;
`insert_adopted_withdrawal` either binds an existing `Quoting`/`Submitted` row or
creates an adopted row. A `custom_meta.id` can bind a requested row, but it must
not overwrite a row already bound to a different `OperationId`.

Withdrawals to external addresses are reconciled from the federation
`Success(txid)`, not a BDK script scan. A BDK scan is only meaningful when the
destination belongs to the local wallet (`BdkWallet::is_our_address`).

### Quote semantics and the known mismatch

`WithdrawalView` exposes `recipient_amount_sat`, `quoted_fee_sat`,
`quote_input_msat`, `quote_output_msat`, `quote_dust_msat`, and
`total_debit_msat`. `total_debit_msat` is the **total debit** from the wallet,
not a net amount:

```text
total_debit_msat =
    recipient_amount_sat * 1000
  + quoted_fee_sat * 1000
  + quote_input_msat + quote_output_msat + quote_dust_msat
```

The UI must show it before asking for confirmation.

The live notes record a 224 msat mismatch between one `send_fee_quote` result and
the amount `get_operation_fees` later reported (`docs/API-VERIFICATION.md` §38).
Accounting still reconciled, but the quote does not reconstruct the charge; this
is an open correctness/audit item, not a solved problem.

## 8. Crash-safety model

The journal protects state transitions that are entirely inside one database
transaction:

- row + open index
- row + claim index
- row + pending-net-credit index
- row + withdrawal-op index
- watermark row
- finish/broadcast/refund/failed transitions with the corresponding index
  cleanup

It does **not** make an external broadcast/federation operation atomic with the
database. The ordering is chosen so the app can recover or avoid a duplicate:

| Window | Current behavior | Residual risk |
|---|---|---|
| Deposit request → address task | Row and open index committed first; conditional address write. | None for the row; a crash leaves `AwaitingAddress` for retry. |
| Address shown → funding broadcast | `funding_txid` is written after a successful BDK broadcast; a later `fund_deposit` returns that txid. | A crash after the network broadcast but before BDK persists the unconfirmed tx and the journal records the txid leaves no recorded id; the next `fund_deposit` may attempt a new build. The documented §27.2 crash occurs after the journal write, so it does not cover this narrower window. |
| Funding confirmed → claim | `watch_deposit` is idempotent and re-observes from the stored position; claim binding is exclusive. | Re-running watch is safe after the claim is visible; a fresh restore is a separate failure mode. |
| `send()` → operation id persisted | `custom_meta.id` is written into the fedimint operation log before/with `send`; `find_sent_operation` searches it. | Recovery depends on the operation log being durable and readable. |
| Final send state → `Broadcast` | Change outputs are awaited first; timeout leaves `Submitted`. | `change_outpoint_range` is not externally verifiable (§37); the wait may report success without proving the named outpoints. |
| Reorg during BDK sync | Ancestry check; orphaned checkpoint triggers full rescan and DB swap. | Reorg of an already-credited deposit is still an open test (§41). |

The live crash matrix is `docs/API-VERIFICATION.md` §27. Fresh-restore
behavior is §30/§31 and is not a crash-recovery success: a mnemonic-only restore
does not resume an in-flight deposit or withdrawal.

### 8.1 Reorg handling

`BdkWallet::sync` (`crates/berlin-wallet-core/src/bdk.rs`) does not trust an
incremental backend update blindly:

1. Read `Wallet::latest_checkpoint()`.
2. Ask the chain backend for its tip: `/blocks/tip/height` and
   `/blocks/tip/hash` on Esplora.
3. If the remote height is at least the local height, ask the backend for the
   remote hash at the local height (`/block-height/{local_height}` on Esplora).
4. `reorg_decision` — a pure function — says `Orphaned` if the remote tip is
   lower or the hash at the local height differs.
5. If orphaned, build a fresh BDK SQLite database from the same seed/descriptors,
   full-scan it, checkpoint and close it, checkpoint/drop the old connection,
   atomically rename the scanned file over the live path, reopen a fresh
   `PersistedWallet`/`Connection`, and raise the next external/internal index to
   `max(rebuilt, previous)` so a revealed-but-unused address is not reused.
6. If ancestry is valid, run `start_sync_with_revealed_spks()` +
   `EsploraAsyncExt::sync(request, 4)` and persist.

Steps 1–5 are backend-independent: they use only
`ChainBackend::tip` and `ChainBackend::block_hash_at`, so the same anchor check
runs for every backend. Step 6 is `ChainBackend::sync`, which is where the three
upstream crates differ.

The detailed source evidence for the BDK lower-tip no-op is in
`docs/API-VERIFICATION.md` §34 and §35. The retry loop retries transient
failures 3 times with capped backoff and returns on the first permanent one.

### 8.2 Backend differences

The trait is uniform; the backends are not.

| | Esplora | Electrum | bitcoind RPC |
|---|---|---|---|
| Shape | async spk query | blocking spk query | block emitter |
| Sync | `EsploraAsyncExt::sync` | `BdkElectrumClient::sync` | `Emitter::next_block` + `apply_block_connected_to` |
| Full scan | gap limit from 0 | gap limit from 0 | birthday height + bounded script range |
| Mempool | in update | in update | `Emitter::mempool` (+ evictions) |
| Execution | native async | `spawn_blocking` | `spawn_blocking` |

Consequences worth knowing:

- **Restore is not equivalent.** Esplora and Electrum discover history with a
  stop gap. bitcoind cannot: a block walk only sees payments to scripts the
  wallet already tracks, so the scan reveals `stop_gap × 5` scripts per keychain
  and walks blocks from the configured birthday. `?birthday=` is required; a
  genesis scan is refused outside regtest (where it is allowed, because the
  devimint chain is short). BIP158 filters would be cheaper but need a
  pre-materialized script list and a node with the block filter index, and have
  not been verified here.
- **The reorg rebuild costs differently.** Steps 1–5 above are identical, but on
  bitcoind the rebuild re-scans from a derived floor:
  `min(configured birthday, earliest confirmed anchor in the old graph, orphaned
  checkpoint height)`. Because the graph is available at that moment, a birthday
  above the wallet's own history cannot silently drop it; the birthday only acts
  as a fallback for a wallet with no confirmed transactions. `restore` has
  neither a graph nor a checkpoint to derive from, so a wrong birthday there
  still drops history — which is why the value is persisted in `chain.json`.
- **Blocking backends need socket timeouts.** `tokio::time::timeout` cannot
  cancel a blocking task. Electrum sets `Config::timeout(30)` and `retry(0)`
  (its own reconnect loop sleeps up to 30 s); bitcoind is built through
  `jsonrpc::simple_http::Builder::timeout(30)` because
  `bitcoincore_rpc::Client::new` cannot set one.
- **bitcoind makes the deterministic reorg test possible.** `invalidateblock`
  against the devimint bitcoind is a supported operation, unlike against the
  devimint Esplora binary (§36/§40).

`ChainError::{Transient, Permanent}` is **one enum over three independent
classifiers, not a shared upstream contract**. Each backend decides what a
dropped socket, a malformed body, or a server rejection looks like in its own
error type; the `kind` field records which backend made the judgement. Today the
Esplora classifier treats connect/timeout/request failures as transient and
malformed bodies and HTTP status errors as permanent. The devimint Esplora
binary panics on a live `invalidateblock`; that upstream issue and the manual
restart recipe are §36/§40.

## 9. Sync tasks

`sync.rs` starts three long-running tasks behind one
`tokio::sync::watch::Receiver<bool>` shutdown signal:

- `bdk_chain_task`: immediate first pass, then every `BDK_SYNC_INTERVAL` (30 s)
  with ±10% jitter. Calls `BdkWallet::sync`; failures are logged and retried on
  the next tick.
- `fedimint_sync_task`: starts `Client::spawn_federation_reconnect`, drains
  `get_event_log_transient_receiver()` without blocking, and runs
  `withdrawal::reconcile_submitted` every `FEDIMINT_SYNC_INTERVAL` (30 s). The
  client executor was already started by `ClientBuilder::join`/`open`.
- `adoption_task`: immediate first tick, then `ADOPTION_INTERVAL` (60 s); runs
  claim adoption, pending net-credit reconciliation, and Send adoption.

The FFI starts these tasks in `install_state`; `shutdown()` signals them, waits up
to 5 s per task, aborts stragglers, and consumes the `ClientHandle` with
`ClientHandle::shutdown`.

## 10. `[patch.crates-io]`: why 62 Fedimint crates are patched

The workspace pins `fedimint-*` at `=0.12.0` and patches **62** Fedimint crates
to one Git source:

```toml
fedimint-... = { git = "https://github.com/fedimint/fedimint", tag = "v0.12.0" }
```

Reasons:

- `fedimint-rocksdb` is not published on crates.io at 0.12.0 (published versions
  stop earlier). The CLI/FFI need the RocksDB backend.
- `fedimint-bip39` is also not published at 0.12.0; the core mirrors its
  derivation and patches the crate for the graph.
- Patching only one crate would leave other Fedimint crates resolved from
  crates.io, producing two different `fedimint-core`/`bitcoin`/`Database` type
  universes. Patching the whole Fedimint family to the same tag keeps one type
  identity across the client modules and the app.
- `Cargo.lock` pins the tag commit (`2ead392b` in the current lock). Updating the
  tag requires updating all 62 entries and re-running the API verification; a
  mixed tag is not supported.
- `deny.toml` allows exactly this Git source and requires a tag specifier.

The source-verified reason and the original build failures are in
`docs/API-VERIFICATION.md` §20.7 and §23.2.

## 11. What a GUI still needs

The near-term direction is `core → CLI-first MVP`; the GUI is deferred. The
`core → binding → GUI` path remains for later, but the binding is deliberately
thin. A real GUI needs facilities that do **not** exist yet:

1. **Event stream.** There is no push channel of deposit/withdrawal/balance
   updates. The FFI exposes polling methods (`deposit_status`,
   `withdrawal_status`, `list_*`, `balance_sat`, `ecash_balance_msat`), and the
   CLI's `watch` blocks. Core has `get_event_log_transient_receiver()` internally,
   but it is not bridged into FFI records/events.
2. **Structured errors.** Core uses `anyhow::Error` plus a small `PegError`
   enum; the FFI flattens everything to
   `PegWalletError::Message { message: String }`. A GUI cannot reliably branch
   on "quote changed", "below dust", "transient network", "DB locked", or
   "pending fee".
3. **Non-blocking long operations.** `request_deposit` returns after spawning the
   address task, but `fund_deposit`, `send_withdrawal`, `restore` (including a
   full BDK scan), and `backup` can await network/disk for a long time. There is
   no progress reporting or resumable job model.
4. **Cancellation.** There is no per-operation cancellation. `shutdown()` is
   global and terminal for the `PegWallet` instance; it waits/aborts tasks but is
   not a UI "cancel this deposit" primitive.
5. **Platform data directories.** `PegWallet::new(data_dir: String)` and the CLI
   require the caller to choose a path. There is no platform-specific default,
   permission model, migration, or "app support directory" integration.
6. **Product-level state.** There is no GUI model for pending operation lists,
   quote confirmation, restore warnings, event history, or multi-federation
   selection. The core exposes records; the UI contract has to be built.
7. **Release packaging, update, and telemetry.** None of this exists in this
   repository.

## 12. Unknowns and deliberate gaps

- No mainnet run is recorded. The live round trip is Mutinynet; local work is
  devimint.
- There is no public walletv2 rescan/re-claim path in 0.12.0. A mnemonic-only
  restore can strand a funded, unclaimed deposit and does not resume an
  in-flight withdrawal (`docs/API-VERIFICATION.md` §30/§31).
- The cause of the 224 msat quote/charge mismatch is unresolved (§38).
- `SendMeta.change_outpoint_range` names txids that were never observed on chain
  in testing; the change wait cannot verify them (§37).
- A reorg that orphans an already-credited deposit has not been tested
  deterministically; the BDK-side reorg bug is fixed, but the deposit/credit
  behavior is an open audit item (§41).
- Multi-federation orchestration is not exposed by the CLI/FFI.
- `lnv2` is in the CLI-first MVP scope but is not registered or implemented yet,
  even though the test federation exposes it. Its gateway/liquidity trust model
  is not yet analyzed.
- There is no independent audit, no hardware wallet support, no encrypted
  seed/backup, and no mobile application.
