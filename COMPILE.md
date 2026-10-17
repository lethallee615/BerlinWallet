# COMPILE.md — pegwallet-core scaffold: compile-verified deviations

## Status

- `cargo check` → **EXIT 0** (`pegwallet-core v0.1.0`, workspace at the repo root).
- Toolchain: `rust-toolchain.toml` = **1.91.1**.
- `.cargo/config.toml`: `rustflags = ["--cfg", "bdk_wallet_unstable"]`; `bdk_wallet`
  features `["bdk-tx", "file_store", "rusqlite"]`.
- `pegwallet-ffi` was **not** created (per instruction). The `uniffi` dependency is
  present from PLAN §2 but unused.
- No real network I/O. Operation-log lookups that the plan glosses over are explicit
  stubs (see §8).

Workspace:

```
Cargo.toml                     [workspace] members = ["crates/pegwallet-core"]
crates/pegwallet-core/
  src/{lib,client,bdk,deposit,withdrawal,journal,ops,error,sync}.rs
```

## 1. `classify_error` parameter type

**Plan** (§4.1/§4.2): one `classify_error(&err)` used with `SendError` and `anyhow::Error`.

**Compile reality:** those are distinct types; one concrete parameter cannot cover both.
Implemented a trait plus a generic function:

- `crates/pegwallet-core/src/error.rs:15` `pub trait Classify`
- `crates/pegwallet-core/src/error.rs:56` `pub fn classify_error<E: Classify + ?Sized>`
- `crates/pegwallet-core/src/error.rs:43` `impl Classify for anyhow::Error` (downcasts to
  `SendError`/`ReceiveError`; unknown errors are conservatively `Transient`).

Docs: <https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/enum.SendError.html>,
<https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/enum.ReceiveError.html>.

## 2. `Address<NetworkUnchecked>` has no `Display`; rows store `String`

**Plan** (§4.1): `set_deposit_address(id, &address, ...)` and a typed `address` column.

**Compile reality:** `Address<NetworkUnchecked>` implements `Serialize` but **not**
`Display`, so `address.to_string()` fails (`E0599`, first compile error). The checked
`Address` implements `Display`.

- `crates/pegwallet-core/src/journal.rs:279` `address.assume_checked().to_string()`
- `crates/pegwallet-core/src/journal.rs:60` / `:155` `Option<String>` address fields
- `crates/pegwallet-core/src/deposit.rs:71` `address.into_unchecked()` (checked → unchecked)
- `crates/pegwallet-core/src/withdrawal.rs:63` `row.address.parse::<Address<NetworkUnchecked>>()`

Docs: <https://docs.rs/bitcoin/0.32.8/bitcoin/struct.Address.html>
(`assume_checked`, `script_pubkey`, `FromStr`).

## 3. Deposit address vs `build_tx` recipient

**Plan** (§4.1): "`build_tx()` paying `addr`".

**Compile reality:** `TxBuilder::add_recipient` takes a `ScriptBuf`, not an `Address`.

- `crates/pegwallet-core/src/bdk.rs:21`
  `builder.add_recipient(destination.script_pubkey(), amount);`

Docs: <https://docs.rs/bdk_wallet/3.2.0/bdk_wallet/struct.TxBuilder.html#method.add_recipient>,
<https://docs.rs/bitcoin/0.32.8/bitcoin/struct.Address.html#method.script_pubkey>.

## 4. `ClientPreview::join` returns `ClientHandle`, not `Client`

**Plan** (§3): `preview.join(...) -> Client` / `ClientHandleArc`.

**Compile reality:** `join` returns `ClientHandle`; `ClientHandleArc = Arc<ClientHandle>`.

- `crates/pegwallet-core/src/client.rs:23-24`
  `let client = preview.join(...).await?; Ok(Arc::new(client))`

Docs: <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientPreview.html#method.join>.

`ClientHandle` derefs to `Client`, so the flow functions' `&Client` parameters are reached
with `&*handle` (or `handle.as_ref()`).

## 5. `start_executor` receiver and `inner_arc` feature gate

**Plan** (§3): `start_executor(&self)` / `self: &Arc<Self>`.

**Compile reality:** `Client::start_executor(self: &Arc<Self>)`; `ClientHandle` has its own
`start_executor(&self)`. `ClientHandle::inner_arc()` is `#[cfg(feature = "uniffi")]` only,
so it is **not** available in this scaffold.

- `crates/pegwallet-core/src/client.rs:42-44`
  `pub fn start_executor(handle: &ClientHandleArc) { handle.start_executor(); }`

Docs: <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.start_executor>,
<https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientHandle.html#method.start_executor>.

## 6. `Database::with_prefix` takes `Vec<u8>`

**Plan** (§3): `db.with_prefix([0x01] ++ u32_le(db_prefix))`.

**Compile reality:** `pub fn with_prefix(&self, prefix: Vec<u8>) -> Self`; the `++`
expression must be built as a `Vec<u8>`.

- `crates/pegwallet-core/src/client.rs:29` `db.with_prefix(vec![0x00])`
- `crates/pegwallet-core/src/client.rs:31-36` builds `[0x01] ++ u32_le(db_prefix)` as `Vec<u8>`

Docs: <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/struct.Database.html#method.with_prefix>.

## 7. `FeeQuote`: `PartialEq` yes, `Serialize` no

**Plan** (§4.2): `fresh_quote != quote`, and a `quote BLOB` column.

- `PartialEq` **is** implemented (`#[derive(Debug, Clone, PartialEq, Eq)]`), so
  `crates/pegwallet-core/src/withdrawal.rs:59` compiles unchanged.
- `FeeQuote` is **not** `Serialize`/`Deserialize` in 0.12.0, so the row cannot persist it
  directly. The scaffold stores `Option<String>`:
  `crates/pegwallet-core/src/journal.rs:116` (`pub quote: Option<String>`), with a note at
  `:112-115`. A production journal needs a typed wire encoding.

Docs: <https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/transaction/struct.FeeQuote.html>.

## 8. `await_receive` returns no operation id / claim figures

**Plan** (§4.1 Success arm): use `ReceiveMeta.outpoint/value/fee` and persist `operation_id`.

**Compile reality:** `await_receive(position) -> anyhow::Result<(FinalReceiveOperationState,
EventLogId)>` — it returns **only** the final state and the cursor. The operation id comes
from `ReceivePaymentEvent.operation_id` in the event log; the figures come from
`WalletOperationMeta::Receive(ReceiveMeta)`.

- `crates/pegwallet-core/src/deposit.rs:226-236` `claim_figures` stub
- `crates/pegwallet-core/src/deposit.rs:250-251` `scan_receive_claims` stub

These are the only two plan steps left as explicit `bail!` stubs; the surrounding call
sequence (timeout loop, cursor advance, `claim_bind`, `finish_deposit`) is real.

Docs: <https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/struct.WalletClientModule.html#method.await_receive>,
<https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/enum.WalletOperationMeta.html>.

## 9. Journal persistence (superseded by §18)

**Plan** (§4.4): "a Journal with the row/index dbtx transitions"; raw rows under `[0x00]`.

**Original scaffold:** `Arc<tokio::sync::Mutex<JournalInner>>` with `BTreeMap` rows and a
`(federation_id, outpoint) -> row` claim map. All state transitions, the single-open invariants,
conditional terminal writes, and `claim_bind`/`TakenBy` were implemented for real; only the
`Database`/`DatabaseTransaction` persistence layer was deferred.

**Current code (see §18):** that implementation is now `MemoryJournal` (`journal.rs:266`) behind
the `JournalBackend` trait, and `journal_db.rs` adds `DbJournal`. `Journal::new(Database, Vec<u8>)`
is the DB-backed constructor; `Journal::in_memory()` is the no-`Database` test path. Prefix layout,
dbtx signatures, and per-transaction invariants are in §18.

Docs: <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/struct.Database.html>,
<https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/struct.DatabaseTransaction.html>.

## 10. `WalletClientConfig` not constructed

**Plan** (§8.10): unverified. The scaffold only calls `WalletClientInit::default()`
(`crates/pegwallet-core/src/client.rs:19`) and does not read or validate
`WalletClientConfig`. `dust_limit`/`fee_consensus` therefore remain unverified for
`SendError::DustValue` and `max_sendable_amount`.

Docs: <https://docs.rs/fedimint-walletv2-common/0.12.0/fedimint_walletv2_common/config/struct.WalletClientConfig.html>.

## 11. `receive()` returns `Address`, not `Result`

**Plan** (§4.1): address task retries "unrecoverable" errors.

**Compile reality:** `pub async fn receive(&self) -> Address` — there is no error value, so
the address task's only terminal path is retry-budget exhaustion:

- `crates/pegwallet-core/src/deposit.rs:56-77` (`MAX_ADDRESS_ATTEMPTS`, timeout loop,
  conditional `fail_deposit_if`).

Docs: <https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/struct.WalletClientModule.html#method.receive>.

## 12. Confirmed signatures that needed no change

| Item | Result |
|---|---|
| `WalletClientModule::await_receive` | `(&self, position: EventLogId) -> anyhow::Result<(FinalReceiveOperationState, EventLogId)>` |
| `WalletClientModule::send` | `(&self, Address<NetworkUnchecked>, bitcoin::Amount, Option<bitcoin::Amount>, serde_json::Value) -> Result<OperationId, SendError>` |
| `WalletClientModule::send_fee` / `send_fee_quote` | as in VERIFY §8.2 |
| `WalletClientModule::await_final_send_operation_state` | `(&self, OperationId) -> anyhow::Result<FinalSendOperationState>` |
| `WalletClientModule::receive_fee` | `(&self) -> Result<bitcoin::Amount, ReceiveError>` |
| `WalletClient::build_tx` / `sign_with_signers` / `finalize_psbt` / `extract_tx` | compiled with `bdk-tx` + `bdk_wallet_unstable` |
| `Wallet::sign_with_signers` | `(&self, &mut Psbt, &[&SignersContainer], SignOptions) -> Result<bool, SignerError>` — `signers` is an explicit parameter in the scaffold |
| `EventLogId` | `Copy + Clone + Ord + Serialize + Deserialize` |
| `OperationId` | manual `Serialize`/`Deserialize` in `fedimint-core` (no derive needed) |
| `FederationId` | `Copy + Ord + Serialize + Deserialize` |
| `FeeQuote` | `Clone + PartialEq + Eq + Debug` (no serde) |

## 13. Build note

`cargo check` was run with `PATH="$HOME/.local/bin:$PATH"` only so a clean rebuild can find
`cmake`; the cached `aws-lc-sys` build did not need it. `LIBCLANG_PATH` and
`BINDGEN_EXTRA_CLANG_ARGS` are already in `.cargo/config.toml [env]`.

---

## 14. Follow-up compile pass: `FeeQuoteWire` + `address_script`

Both items now compile: `cargo check` → **EXIT 0**, and `cargo test -p pegwallet-core --lib`
passes the `FeeQuoteWire` round-trip (`wire::tests::fee_quote_round_trip ... ok`).

### 14.1 `FeeQuoteWire` — the sketched shape **held** (no deviation)

Confirmed against the real source:

- `FeeQuote { input: Amounts, output: Amounts, dust: Amounts }` — public fields.
- `Amounts` implements `IntoIterator<Item = (AmountUnit, Amount)>`.
- `AmountUnit` derives `Ord` (valid `BTreeMap` key) and is `Serialize`/`Deserialize`.
- `Amount` is `#[serde(transparent)]` and `Serialize`/`Deserialize`.
- `Amounts::ZERO` is `pub const`; `checked_add_unit(self, amount, unit) -> Option<Self>` is public.

The real wire shape is exactly the sketch:

```rust
pub struct FeeQuoteWire {
    pub input: BTreeMap<AmountUnit, fedimint_core::Amount>,
    pub output: BTreeMap<AmountUnit, fedimint_core::Amount>,
    pub dust: BTreeMap<AmountUnit, fedimint_core::Amount>,
}
```

- `crates/pegwallet-core/src/wire.rs:13` — the struct.
- `wire.rs:19` — `From<&FeeQuote>` via `into_iter().collect()`.
- `wire.rs:39` — `amounts()` rebuilds each `Amounts` with `Amounts::ZERO` + `checked_add_unit`.
- `wire.rs:54` — round-trip test (`FeeQuote` → wire → back).
- `crates/pegwallet-core/src/journal.rs:116` — `WithdrawalRow.quote: Option<FeeQuoteWire>`
  (the `Option<String>` placeholder is gone).
- `crates/pegwallet-core/src/withdrawal.rs:55` — the quote is persisted as the wire form.
- `crates/pegwallet-core/src/journal.rs:536` — `set_withdrawal_quote`.

Docs: <https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/transaction/struct.FeeQuote.html>,
<https://docs.rs/fedimint-core/0.12.0/fedimint_core/module/struct.Amounts.html>,
<https://docs.rs/fedimint-core/0.12.0/fedimint_core/module/struct.AmountUnit.html>.

### 14.2 `address_script` — added and used for matching

- `crates/pegwallet-core/src/journal.rs:61` — `DepositRow.address_script: Option<Vec<u8>>`.
- `journal.rs:282` — populated in `set_deposit_address_if`:
  `assume_checked().script_pubkey().as_bytes().to_vec()` (display string kept separately).
- `journal.rs:156` / `journal.rs:419` — `AdoptedClaim.address_script` and the adopted row carry it.
- `crates/pegwallet-core/src/deposit.rs:239` — `OrphanClaim.address_script`.
- `deposit.rs:168-176` — the watcher's Success arm now reuses `scan_receive_claims` and matches
  `claim.address_script.as_deref() == row.address_script.as_deref()`; `claim_figures` is removed.
- The `cargo check`-verified `Address<NetworkUnchecked>` rule still holds: no `Display`, so display
  goes through `assume_checked()`, matching through `script_pubkey().as_bytes()`.

Docs: <https://docs.rs/bitcoin/0.32.8/bitcoin/struct.Address.html#method.script_pubkey>,
<https://docs.rs/bitcoin/0.32.8/bitcoin/struct.ScriptBuf.html#method.as_bytes>.

### 14.3 Remaining runtime stub (unchanged)

`scan_receive_claims` is still a `bail!` stub (no network I/O), but it is now the **only**
operation-log lookup point: both the watcher Success arm and `adopt_orphan_claims` call it.
`claim_figures` no longer exists.

---

## 15. `scan_receive_claims` implemented (operation-log scan)

**Supersedes §14.3** — the stub is gone. `cargo check` → EXIT 0; `cargo test -p pegwallet-core
--lib` → 2 passed (`wire::tests::fee_quote_round_trip`,
`deposit::tests::filters_successful_unbound_unique_claims`).

### 15.1 Exact APIs (confirmed)

| API | Signature | Location |
|---|---|---|
| `Client::operation_log` | `pub fn operation_log(&self) -> &OperationLog` | `fedimint-client-0.12.0/src/client.rs:719` |
| `OperationLog::paginate_operations_rev` | `pub async fn paginate_operations_rev(&self, limit: usize, last_seen: Option<ChronologicalOperationLogKey>) -> Vec<(ChronologicalOperationLogKey, OperationLogEntry)>` | `fedimint-client-0.12.0/src/oplog.rs:131` |
| `OperationLog::get_operation` | `pub async fn get_operation(&self, operation_id: OperationId) -> Option<OperationLogEntry>` | `fedimint-client-0.12.0/src/oplog.rs:194` |
| `OperationLogEntry::operation_module_kind` | `pub fn operation_module_kind(&self) -> &str` | `fedimint-client-module-0.12.0/src/oplog.rs:161` |
| `OperationLogEntry::try_meta` | `pub fn try_meta<M: DeserializeOwned>(&self) -> Result<M, serde_json::Error>` | `fedimint-client-module-0.12.0/src/oplog.rs:178` |
| `OperationLogEntry::try_outcome` | `pub fn try_outcome<D: DeserializeOwned>(&self) -> Result<Option<D>, serde_json::Error>` | `fedimint-client-module-0.12.0/src/oplog.rs:207` |
| `ChronologicalOperationLogKey` | `pub struct { pub creation_time: SystemTime, pub operation_id: OperationId }` | `fedimint-client-0.12.0/src/db.rs:193` |
| `WalletClientModule::await_final_receive_operation_state` | `pub async fn ...(&self, OperationId) -> anyhow::Result<FinalReceiveOperationState>` | VERIFY §8.2 |

**Trait imports / features:** none for the operation log — `operation_log`, `paginate_operations_rev`,
`try_meta`, and `try_outcome` are inherent. The only trait import needed is in the unit test:
`bitcoin::hashes::Hash` (for `Txid::from_byte_array`). No crate feature is required.

### 15.2 Implementation

`crates/pegwallet-core/src/deposit.rs`:

- `pub async fn scan_receive_claims(client, journal, federation_id)` at `:297`.
- Pages 256 operations at a time via `paginate_operations_rev` (`:310`).
- Reads `WalletOperationMeta` with `try_meta` (`:316`), then takes
  `ReceiveMeta { outpoint, value, fee, address, .. }`.
- Final state from `try_outcome::<FinalReceiveOperationState>()` (`:333`); when the outcome is not
  cached, a bounded 500 ms `await_final_receive_operation_state` check is used.
- `operation_id` comes from `ChronologicalOperationLogKey::operation_id` (the same id carried by
  `ReceivePaymentEvent.operation_id`).
- `address_script` from `assume_checked().script_pubkey().as_bytes()`; display string kept too.
- `Journal::bound_outpoints` (`journal.rs:369`) filters already-bound claims.
- `filter_orphan_claims` (`deposit.rs:259`) keeps only `Success`, de-duplicates outpoints, and drops
  bound ones.
- Callers wired at `deposit.rs:171` (watcher Success arm) and `:204` (adoption pass).
- `bail!` removed; `claim_figures` remains gone.

### 15.3 Signature deviation

The request said `scan_receive_claims(...) -> Vec<OrphanClaim>`; implemented as
`-> anyhow::Result<Vec<OrphanClaim>>`. A failed operation-log read must not be reported as "no
orphans" (fund-loss risk); both callers already propagate `Result`. `OrphanClaim` is `pub` with
`pub` fields (`deposit.rs:238`).

### 15.4 Unit test

`deposit::tests::filters_successful_unbound_unique_claims` (`deposit.rs:403`) builds a fake claim
set: Success+unbound, Aborted, Success+bound, final-state-unknown, and a duplicate outpoint. It
asserts exactly one orphan with the expected `operation_id`, outpoint, value, fee, and
`address_script`.

Docs: <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.operation_log>,
<https://docs.rs/fedimint-client/0.12.0/fedimint_client/oplog/struct.OperationLog.html#method.paginate_operations_rev>,
<https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/oplog/struct.OperationLogEntry.html#method.try_meta>,
<https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/oplog/struct.OperationLogEntry.html#method.try_outcome>,
<https://docs.rs/fedimint-client/0.12.0/fedimint_client/db/struct.ChronologicalOperationLogKey.html>,
<https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/struct.WalletClientModule.html#method.await_final_receive_operation_state>.

---

## 16. Scan watermark, module filter, bounded waits (compiled)

`cargo check` and `cargo test -p pegwallet-core --lib` → **EXIT 0** (3 tests).

### 16.1 Module filter

`fedimint-walletv2-common` defines
`pub const KIND: ModuleKind = ModuleKind::from_static_str("walletv2")`
(`fedimint-walletv2-common-0.12.0/src/lib.rs:30`); `ModuleKind::as_str` is at
`fedimint-core-0.12.0/src/core.rs:185`. The scan now compares
`entry.operation_module_kind() != fedimint_walletv2_common::KIND.as_str()` **before** `try_meta`
(`deposit.rs:385`), so non-wallet operations are skipped without deserialization. Expected walletv2
kind string: **`"walletv2"`**.

### 16.2 Watermark

- `Journal::scan_watermark` / `set_scan_watermark` (`journal.rs:385/393`) store the newest
  `ChronologicalOperationLogKey` per federation.
- `ChronologicalOperationLogKey` is `Copy` (`fedimint-client-0.12.0/src/db.rs:193`).
- `scan_receive_claims` resumes from the watermark with `SCAN_OVERLAP = 32` older operations
  (`deposit.rs:25`, `:353`). Because `paginate_operations_rev` is reverse-chronological, a pass
  starts at "now" and stops at the previous watermark + overlap; an idle pass does not move the
  watermark backwards.
- The boot path (`adopt_orphan_claims`) and the watcher Success arm both use the watermark.

### 16.3 Success arm with no visible claim

If `await_receive` returns `Success` before the claim is visible, the watcher retries the scan
(`SCAN_RETRY_ATTEMPTS = 5`, backoff) and, if still absent, returns `Ok(())` leaving the row
`AddressShown`; the adoption pass binds it later and R takes the `TakenBy` path. The row is no
longer errored. Test:
`deposit::tests::success_without_a_visible_matching_claim_defers_to_adoption`
(`deposit.rs:540`).

### 16.4 Bounded state wait

The uncached-outcome fallback only runs for walletv2 `Receive` ops (after the module filter), at
most `SCAN_STATE_WAIT_PER_OP = 500 ms` each and `SCAN_TOTAL_STATE_WAIT = 2 s` per scan
(`deposit.rs:27`); after the budget an op is treated as not-final and left for a later scan.

Docs: <https://docs.rs/fedimint-core/0.12.0/fedimint_core/core/struct.ModuleKind.html#method.as_str>,
<https://docs.rs/fedimint-walletv2-common/0.12.0/fedimint_walletv2_common/constant.KIND.html>,
<https://docs.rs/fedimint-client/0.12.0/fedimint_client/db/struct.ChronologicalOperationLogKey.html>.

---

## 17. Watermark advances only on a terminal pass; adoption is scheduled

`cargo check` + `cargo test -p pegwallet-core --lib` → **EXIT 0** (5 tests).

### 17.1 Terminal-only watermark advance

- `should_advance_watermark(reached_terminal, state_wait_exhausted)` (`deposit.rs:318`) returns
  `reached_terminal && !state_wait_exhausted`.
- `scan_receive_claims` (`deposit.rs:352`) sets `reached_terminal` on:
  - an empty page,
  - the watermark + `SCAN_OVERLAP` boundary, or
  - a short page (end of the log).
- It sets `state_wait_exhausted` when the 2 s state-wait budget is hit. The watermark is advanced
  only when `should_advance_watermark` is true; a truncated pass (state-wait budget or a future
  page cap) or an errored pass (`?` returns before the update) leaves the watermark unchanged, so
  the next pass re-covers the gap.
- Test: `deposit::tests::truncated_pass_does_not_advance_watermark_and_next_pass_finds_claim`
  (`deposit.rs:577`).

### 17.2 Adoption schedule

`sync::adoption_task` (`sync.rs:21`) runs `deposit::adopt_orphan_claims` on
`tokio::time::interval(ADOPTION_INTERVAL)` where `ADOPTION_INTERVAL = 60 s` (`sync.rs:15`). The
first tick is immediate, so it covers boot as well as the periodic cadence. This is the schedule
the Success-arm deferral relies on: a row left `AddressShown` because the claim was not yet
visible is bound by the next tick, which releases the single-flight slot.

---

## 18. Real persistence: `JournalBackend`, `DbJournal`, prefix layout, dbtx deviations

`cargo check` + `cargo test -p pegwallet-core --lib` → **EXIT 0** (5 tests).

### 18.1 `Database` is a struct, not a trait

The requested `Arc<dyn Database>` does not exist: `fedimint_core::db::Database` is
`pub struct Database` (`fedimint-core-0.12.0/src/db/mod.rs:399`). It is `Clone` and internally
Arc-backed, so no outer `Arc` is needed. The constructor is therefore:

```rust
// crates/pegwallet-core/src/journal.rs:767
pub fn new(db: Database, client_prefix: Vec<u8>) -> Self
```

`Journal::in_memory()` (`journal.rs:773`) is the no-`Database` test path.
`Database::with_prefix(&self, prefix: Vec<u8>) -> Self` (`db/mod.rs:442`) applies the client-name
prefix once in `DbJournal::new` (`journal_db.rs:180`).

### 18.2 dbtx signatures actually used

```rust
// db/mod.rs:510
pub async fn begin_transaction<'s, 'tx>(&'s self) -> DatabaseTransaction<'tx, Committable>;
// db/mod.rs:521
pub async fn begin_transaction_nc<'s, 'tx>(&'s self) -> DatabaseTransaction<'tx, NonCommittable>;

// db/mod.rs:1530 — default is NonCommittable, NOT Committable
pub struct DatabaseTransaction<'tx, Cap = NonCommittable> { ... }

// db/mod.rs:883, trait IDatabaseTransactionOpsCore (must be imported for raw_*)
async fn raw_insert_bytes(&mut self, key: &[u8], value: &[u8])
    -> DatabaseResult<Option<Vec<u8>>>;
async fn raw_get_bytes(&mut self, key: &[u8]) -> DatabaseResult<Option<Vec<u8>>>;
async fn raw_remove_entry(&mut self, key: &[u8]) -> DatabaseResult<Option<Vec<u8>>>;
async fn raw_find_by_prefix(&mut self, key_prefix: &[u8])
    -> DatabaseResult<PrefixStream<'_>>;

// commit_tx_result consumes the Committable transaction.
async fn commit_tx_result(self) -> DatabaseResult<()>;
```

Deviations from the plan's sketch:

- **`DatabaseTransaction`'s default capability is `NonCommittable`.** The first compile pass used
  `&mut DatabaseTransaction<'_>` in helpers; `begin_transaction` actually returns
  `DatabaseTransaction<'_, Committable>`. Helpers now spell
  `&mut DatabaseTransaction<'_, Committable>` (`journal_db.rs:90`).
- **`commit_tx_result`, not bare `commit_tx`,** is used so a commit error becomes an
  `anyhow::Result` error instead of being dropped.
- **`raw_find_by_prefix` returns a borrowing async stream** (`PrefixStream<'_> =
  Pin<Box<dyn Stream<Item = (Vec<u8>, Vec<u8>)> + '_>>`), not a `Vec`. Scans collect or consume
  the stream before issuing a write (and explicitly `drop(stream)` in `bound_outpoints`,
  `journal_db.rs:399`).
- **`raw_insert_bytes` returns the previous value** (`Option<Vec<u8>>`); the journal ignores it.

### 18.3 Prefix layout

`Journal::new(db, client_prefix)` calls `db.with_prefix(client_prefix)`; all keys below are relative
to that prefix.

| Tag | Key | Value |
|---|---|---|
| `0x01` | `0x01 ++ deposit_id` (UUID ASCII) | `Envelope<DepositRow>` |
| `0x02` | `0x02 ++ withdrawal_id` (UUID ASCII) | `Envelope<WithdrawalRow>` |
| `0x03` | `0x03 ++ federation_id(32) ++ txid(32) ++ vout_le(4)` | `Envelope<DepositId>` |
| `0x04` | `0x04 ++ federation_id(32)` | `Envelope<ChronologicalOperationLogKey>` |

- `federation_id` bytes: `FederationId.0.to_byte_array()` (32-byte `sha256::Hash`); `bitcoin::Txid`
  likewise. The claim key is fixed-length (69 bytes), so `bound_outpoints` can recover the
  `OutPoint` from the key without decoding JSON (`journal_db.rs:76`).
- Envelope: `{"version": 1, "value": ...}` (`ROW_VERSION = 1`, `journal_db.rs:26`) so rows can
  evolve without reinterpreting old bytes. `FeeQuoteWire` and `address_script` are unchanged.
- Deposit/withdrawal rows carry `id` inside the value as well as the key; the key is authoritative
  for lookup.

### 18.4 Atomicity and invariants

- Every transition opens exactly one `begin_transaction`, performs its reads/scan/check and writes,
  then `commit_tx_result` (`journal_db.rs:188` onward). Read-only methods also close their
  transaction before returning.
- **One open deposit per federation:** `insert_deposit` scans `[0x01]` inside the same dbtx and
  bails if any other row for that federation is `AwaitingAddress`/`AddressShown`
  (`journal_db.rs:201`). `open_deposit` uses the same predicate.
- **One in-flight withdrawal per federation:** `insert_withdrawal` scans `[0x02]` and rejects a
  second `Quoting`/`Submitted` row (`journal_db.rs:539`).
- **Unique `(federation_id, funding_outpoint)`:** `claim_bind` and `claim_adopt_if_absent` read the
  fixed claim key, then insert it in the same dbtx; the same key for the same row returns
  `BindResult::Bound`, a different row returns `BindResult::TakenBy(existing)` (`journal_db.rs:360`).
- Conditional state changes (`set_deposit_address_if`, `fail_deposit_if`, `cancel_deposit`,
  `cancel_withdrawal`) still return `ApplyResult::Applied` / `Skipped`.
- The single-open checks are application-level and run inside the transaction that performs the
  insert. Concurrent writers that both scan before either commits are ultimately arbitrated by the
  backend's optimistic write-conflict detection at `commit_tx_result`; `claim_bind` is additionally
  protected by the tracked read of the fixed claim key. If a backend does not conflict on prefix
  scans, a dedicated per-federation open-index key is the next hardening step.

### 18.5 Trait split

`JournalBackend` (`journal.rs:176`) lists the unchanged transition signatures. Two implementations:

- `DbJournal` (`journal_db.rs:175`) — the production backend.
- `MemoryJournal` (`journal.rs:266`) — the unit-test backend (`Journal::in_memory()`), using the
  original `Arc<Mutex<JournalInner>>` state.

`Journal` (`journal.rs:762`) is a `Clone` handle over `Arc<dyn JournalBackend>` and implements
`Deref<Target = dyn JournalBackend>`, so callers keep `journal.open_deposit(...)`-style calls and
need no signature changes. `Journal::default()` remains the in-memory backend for tests.

### 18.6 Shutdown signal

`sync::adoption_task` (`sync.rs:20`) now takes
`tokio::sync::watch::Receiver<bool>` and exits on `true` or sender drop; the `tokio::select!`
races `ticker.tick()` against `shutdown.changed()` (`sync.rs:27`). This is documented in PLAN §5.

Docs: <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/struct.Database.html>,
<https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/struct.DatabaseTransaction.html>,
<https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/trait.IDatabaseTransactionOpsCore.html>,
<https://docs.rs/tokio/1.53.3/tokio/sync/watch/struct.Receiver.html>.
