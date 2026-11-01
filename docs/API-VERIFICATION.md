# API Verification

Compile-verified deviations for the `berlin-wallet-core` scaffold.

## Status

- `cargo check` → **EXIT 0** (`berlin-wallet-core v0.1.0`, workspace at the repo root).
- Toolchain: `rust-toolchain.toml` = **1.91.1**.
- `.cargo/config.toml`: `rustflags = ["--cfg", "bdk_wallet_unstable"]`; `bdk_wallet`
  features `["bdk-tx", "file_store", "rusqlite"]`.
- `berlin-wallet-ffi` was **not** created (per instruction). The `uniffi` dependency is
  present from PLAN §2 but unused.
- No real network I/O. Operation-log lookups that the plan glosses over are explicit
  stubs (see §8).

Workspace:

```
Cargo.toml                     [workspace] members = ["crates/berlin-wallet-core"]
crates/berlin-wallet-core/
  src/{lib,client,bdk,deposit,withdrawal,journal,ops,error,sync}.rs
```

## 1. `classify_error` parameter type

**Plan** (§4.1/§4.2): one `classify_error(&err)` used with `SendError` and `anyhow::Error`.

**Compile reality:** those are distinct types; one concrete parameter cannot cover both.
Implemented a trait plus a generic function:

- `crates/berlin-wallet-core/src/error.rs:15` `pub trait Classify`
- `crates/berlin-wallet-core/src/error.rs:56` `pub fn classify_error<E: Classify + ?Sized>`
- `crates/berlin-wallet-core/src/error.rs:43` `impl Classify for anyhow::Error` (downcasts to
  `SendError`/`ReceiveError`; unknown errors are conservatively `Transient`).

Docs: <https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/enum.SendError.html>,
<https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/enum.ReceiveError.html>.

## 2. `Address<NetworkUnchecked>` has no `Display`; rows store `String`

**Plan** (§4.1): `set_deposit_address(id, &address, ...)` and a typed `address` column.

**Compile reality:** `Address<NetworkUnchecked>` implements `Serialize` but **not**
`Display`, so `address.to_string()` fails (`E0599`, first compile error). The checked
`Address` implements `Display`.

- `crates/berlin-wallet-core/src/journal.rs:279` `address.assume_checked().to_string()`
- `crates/berlin-wallet-core/src/journal.rs:60` / `:155` `Option<String>` address fields
- `crates/berlin-wallet-core/src/deposit.rs:71` `address.into_unchecked()` (checked → unchecked)
- `crates/berlin-wallet-core/src/withdrawal.rs:63` `row.address.parse::<Address<NetworkUnchecked>>()`

Docs: <https://docs.rs/bitcoin/0.32.8/bitcoin/struct.Address.html>
(`assume_checked`, `script_pubkey`, `FromStr`).

## 3. Deposit address vs `build_tx` recipient

**Plan** (§4.1): "`build_tx()` paying `addr`".

**Compile reality:** `TxBuilder::add_recipient` takes a `ScriptBuf`, not an `Address`.

- `crates/berlin-wallet-core/src/bdk.rs:21`
  `builder.add_recipient(destination.script_pubkey(), amount);`

Docs: <https://docs.rs/bdk_wallet/3.2.0/bdk_wallet/struct.TxBuilder.html#method.add_recipient>,
<https://docs.rs/bitcoin/0.32.8/bitcoin/struct.Address.html#method.script_pubkey>.

## 4. `ClientPreview::join` returns `ClientHandle`, not `Client`

**Plan** (§3): `preview.join(...) -> Client` / `ClientHandleArc`.

**Compile reality:** `join` returns `ClientHandle`; `ClientHandleArc = Arc<ClientHandle>`.

- `crates/berlin-wallet-core/src/client.rs:23-24`
  `let client = preview.join(...).await?; Ok(Arc::new(client))`

Docs: <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientPreview.html#method.join>.

`ClientHandle` derefs to `Client`, so the flow functions' `&Client` parameters are reached
with `&*handle` (or `handle.as_ref()`).

## 5. `start_executor` receiver and `inner_arc` feature gate

**Plan** (§3): `start_executor(&self)` / `self: &Arc<Self>`.

**Compile reality:** `Client::start_executor(self: &Arc<Self>)`; `ClientHandle` has its own
`start_executor(&self)`. `ClientHandle::inner_arc()` is `#[cfg(feature = "uniffi")]` only,
so it is **not** available in this scaffold.

- `crates/berlin-wallet-core/src/client.rs:42-44`
  `pub fn start_executor(handle: &ClientHandleArc) { handle.start_executor(); }`

Docs: <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.start_executor>,
<https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientHandle.html#method.start_executor>.

## 6. `Database::with_prefix` takes `Vec<u8>`

**Plan** (§3): `db.with_prefix([0x01] ++ u32_le(db_prefix))`.

**Compile reality:** `pub fn with_prefix(&self, prefix: Vec<u8>) -> Self`; the `++`
expression must be built as a `Vec<u8>`.

- `crates/berlin-wallet-core/src/client.rs:29` `db.with_prefix(vec![0x00])`
- `crates/berlin-wallet-core/src/client.rs:31-36` builds `[0x01] ++ u32_le(db_prefix)` as `Vec<u8>`

Docs: <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/struct.Database.html#method.with_prefix>.

## 7. `FeeQuote`: `PartialEq` yes, `Serialize` no

**Plan** (§4.2): `fresh_quote != quote`, and a `quote BLOB` column.

- `PartialEq` **is** implemented (`#[derive(Debug, Clone, PartialEq, Eq)]`), so
  `crates/berlin-wallet-core/src/withdrawal.rs:59` compiles unchanged.
- `FeeQuote` is **not** `Serialize`/`Deserialize` in 0.12.0, so the row cannot persist it
  directly. The scaffold stores `Option<String>`:
  `crates/berlin-wallet-core/src/journal.rs:116` (`pub quote: Option<String>`), with a note at
  `:112-115`. A production journal needs a typed wire encoding.

Docs: <https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/transaction/struct.FeeQuote.html>.

## 8. `await_receive` returns no operation id / claim figures

**Plan** (§4.1 Success arm): use `ReceiveMeta.outpoint/value/fee` and persist `operation_id`.

**Compile reality:** `await_receive(position) -> anyhow::Result<(FinalReceiveOperationState,
EventLogId)>` — it returns **only** the final state and the cursor. The operation id comes
from `ReceivePaymentEvent.operation_id` in the event log; the figures come from
`WalletOperationMeta::Receive(ReceiveMeta)`.

- `crates/berlin-wallet-core/src/deposit.rs:226-236` `claim_figures` stub
- `crates/berlin-wallet-core/src/deposit.rs:250-251` `scan_receive_claims` stub

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

## 10. `WalletClientConfig` — resolved (§19)

**Plan** (§8.10): **RESOLVED**. `WalletClientInit` is a unit struct with `Default`; the
per-federation `WalletClientConfig` is not a client-builder argument. It arrives through
`ClientModuleInitArgs::cfg()` and is read after join via `Client::config().await` +
`ClientConfig::get_first_module_by_kind::<WalletClientConfig>(KIND)`. `client::wallet_client_config`
wraps that path, and `send_withdrawal` validates `value >= dust_limit` before quoting. See §19 for
signatures and the source defaults.

Docs: <https://docs.rs/fedimint-walletv2-common/0.12.0/fedimint_walletv2_common/config/struct.WalletClientConfig.html>.

## 11. `receive()` returns `Address`, not `Result`

**Plan** (§4.1): address task retries "unrecoverable" errors.

**Compile reality:** `pub async fn receive(&self) -> Address` — there is no error value, so
the address task's only terminal path is retry-budget exhaustion:

- `crates/berlin-wallet-core/src/deposit.rs:56-77` (`MAX_ADDRESS_ATTEMPTS`, timeout loop,
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
`BINDGEN_EXTRA_CLANG_ARGS` are supplied by the shell, not `.cargo/config.toml`; see §40.

---

## 14. Follow-up compile pass: `FeeQuoteWire` + `address_script`

Both items now compile: `cargo check` → **EXIT 0**, and `cargo test -p berlin-wallet-core --lib`
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

- `crates/berlin-wallet-core/src/wire.rs:13` — the struct.
- `wire.rs:19` — `From<&FeeQuote>` via `into_iter().collect()`.
- `wire.rs:39` — `amounts()` rebuilds each `Amounts` with `Amounts::ZERO` + `checked_add_unit`.
- `wire.rs:54` — round-trip test (`FeeQuote` → wire → back).
- `crates/berlin-wallet-core/src/journal.rs:116` — `WithdrawalRow.quote: Option<FeeQuoteWire>`
  (the `Option<String>` placeholder is gone).
- `crates/berlin-wallet-core/src/withdrawal.rs:55` — the quote is persisted as the wire form.
- `crates/berlin-wallet-core/src/journal.rs:536` — `set_withdrawal_quote`.

Docs: <https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/transaction/struct.FeeQuote.html>,
<https://docs.rs/fedimint-core/0.12.0/fedimint_core/module/struct.Amounts.html>,
<https://docs.rs/fedimint-core/0.12.0/fedimint_core/module/struct.AmountUnit.html>.

### 14.2 `address_script` — added and used for matching

- `crates/berlin-wallet-core/src/journal.rs:61` — `DepositRow.address_script: Option<Vec<u8>>`.
- `journal.rs:282` — populated in `set_deposit_address_if`:
  `assume_checked().script_pubkey().as_bytes().to_vec()` (display string kept separately).
- `journal.rs:156` / `journal.rs:419` — `AdoptedClaim.address_script` and the adopted row carry it.
- `crates/berlin-wallet-core/src/deposit.rs:239` — `OrphanClaim.address_script`.
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

**Supersedes §14.3** — the stub is gone. `cargo check` → EXIT 0; `cargo test -p berlin-wallet-core
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

`crates/berlin-wallet-core/src/deposit.rs`:

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

`cargo check` and `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (3 tests).

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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (37 tests).

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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (37 tests).

### 18.1 `Database` is a struct, not a trait

The requested `Arc<dyn Database>` does not exist: `fedimint_core::db::Database` is
`pub struct Database` (`fedimint-core-0.12.0/src/db/mod.rs:399`). It is `Clone` and internally
Arc-backed, so no outer `Arc` is needed. The constructor is therefore:

```rust
// crates/berlin-wallet-core/src/journal.rs:767
pub fn new(db: Database, client_prefix: Vec<u8>) -> Self
```

`Journal::in_memory()` (`journal.rs:773`) is the no-`Database` test path.
`Database::with_prefix(&self, prefix: Vec<u8>) -> Self` (`db/mod.rs:442`) applies the client-name
prefix once in `DbJournal::new` (`journal_db.rs:172`).

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
  `journal_db.rs:606`).
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
| `0x05` | `0x05 ++ federation_id(32)` | `Envelope<DepositId>` (open-deposit index) |
| `0x06` | `0x06 ++ federation_id(32)` | `Envelope<WithdrawalId>` (in-flight-withdrawal index) |

- `federation_id` bytes: `FederationId.0.to_byte_array()` (32-byte `sha256::Hash`); `bitcoin::Txid`
  likewise. The claim key is fixed-length (69 bytes), so `bound_outpoints` can recover the
  `OutPoint` from the key without decoding JSON (`journal_db.rs:77`).
- Envelope: `{"version": 1, "value": ...}` (`ROW_VERSION = 1`, `journal_db.rs:27`) so rows can
  evolve without reinterpreting old bytes. `FeeQuoteWire` and `address_script` are unchanged.
- Deposit/withdrawal rows carry `id` inside the value as well as the key; the key is authoritative
  for lookup.
- `0x05`/`0x06` are exact-key indexes for the single-open invariants. They are read, written and
  removed in the same dbtx as the row. Prefix scans of `[0x01]`/`[0x02]` remain only for listing
  (e.g. `deposits_in`); they are no longer on the invariant path.

### 18.4 Atomicity and invariants

- Every transition opens exactly one `begin_transaction`, performs its reads/checks and writes,
  then `commit_tx_result` (`journal_db.rs:305` onward). Read-only methods also close their
  transaction before returning.
- **One open deposit per federation:** `insert_deposit` reads the exact key `0x05 ++
  federation_id` with `raw_get_bytes` inside the dbtx (`journal_db.rs:321`). If present, it returns
  the existing row (same `DepositId`, no error); otherwise it writes the row and the index key in
  the same transaction. `open_deposit` reads the index and loads that row.
- **One in-flight withdrawal per federation:** `insert_withdrawal` does the same with `0x06 ++
  federation_id` (`journal_db.rs:798`); a second `Quoting`/`Submitted` insert is rejected with an
  explicit in-flight error. `open_withdrawal` reads the index.
- **Terminal transitions clear the index in the same dbtx:** deposit `finish_deposit`,
  `close_deposit_done`, `close_deposit_done_if_open`, `cancel_deposit`, `fail_deposit`,
  `fail_deposit_if`; withdrawal `set_broadcast`, `set_refunded`, `set_failed`,
  `set_withdrawal_failed`, `cancel_withdrawal`.
- **Unique `(federation_id, funding_outpoint)`:** `claim_bind` and `claim_adopt_if_absent` read the
  fixed claim key, then insert it in the same dbtx; the same key for the same row returns
  `BindResult::Bound`, a different row returns `BindResult::TakenBy(existing)` (`journal_db.rs:556`).
- Conditional state changes (`set_deposit_address_if`, `fail_deposit_if`, `cancel_deposit`,
  `cancel_withdrawal`) still return `ApplyResult::Applied` / `Skipped`.
- **Retry on optimistic conflict:** `retry_on_conflict` (`journal_db.rs:277`) re-runs the whole
  transition — fresh `begin_transaction`, re-read, re-check, re-write — up to
  `RETRY_ON_CONFLICT_ATTEMPTS = 5` (`journal_db.rs:181`) with jittered backoff. It retries
  `DatabaseError::WriteConflict` and `DatabaseError::SnapshotTooOld`; a lost race therefore resolves
  to the idempotent result (`Bound`/`TakenBy`, the existing open row, `Applied`/`Skipped`) instead
  of an error. It is applied to `insert_deposit`, `insert_withdrawal`, `claim_bind`,
  `claim_adopt_if_absent`, and all other read-check-write transitions.
- **Conflict tracking:** whether `raw_find_by_prefix` creates a conflict-tracked read is
  unverified; ask the fedimint dev. The single-open invariant no longer depends on the answer: it
  uses exact-key `raw_get_bytes`/`raw_insert_bytes`, which are the tracked operations, and the
  index key is correct either way.

### 18.5 Trait split

`JournalBackend` (`journal.rs:176`) lists the transition signatures. Two implementations:

- `DbJournal` (`journal_db.rs:167`) — the production backend.
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

---

## 19. `WalletClientConfig`: construction path, defaults, and pre-send dust validation

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (37 tests).

### 19.1 How the config is constructed and passed

`WalletClientInit` is a **unit struct** with an explicit `Default`:

```rust
// fedimint-walletv2-client-0.12.0/src/lib.rs:178-180
#[derive(Debug, Clone, Default)]
pub struct WalletClientInit;
```

`WalletClientInit::default()` is therefore correct. `WalletClientConfig` is **not** a
client-builder argument; it is per-federation module config supplied to the module initializer:

```rust
// fedimint-client-module-0.12.0/src/module/init.rs:99
pub fn cfg(&self) -> &<<C as ModuleInit>::Common as CommonModuleInit>::ClientConfig;

// fedimint-walletv2-client-0.12.0/src/lib.rs:195-216
impl ClientModuleInit for WalletClientInit {
    async fn init(&self, args: &ClientModuleInitArgs<Self>) -> anyhow::Result<Self::Module> {
        let module = WalletClientModule {
            cfg: args.cfg().clone(), // <- WalletClientConfig
            ...
        };
        ...
    }
}
```

For walletv2, `WalletCommonInit::ClientConfig = WalletClientConfig`
(`fedimint-walletv2-common-0.12.0/src/lib.rs:114`) and the module kind is
`KIND = ModuleKind::from_static_str("walletv2")` (`lib.rs:30`).

After joining, the decoded config is read without touching the module's private `cfg` field:

```rust
// fedimint-client-0.12.0/src/client.rs:567
pub async fn config(&self) -> ClientConfig;

// fedimint-core-0.12.0/src/config.rs:514
pub fn get_first_module_by_kind<T: Decodable + 'static>(
    &self,
    kind: impl Into<ModuleKind>,
) -> anyhow::Result<(ModuleInstanceId, &T)>;
```

The scaffold wraps this in `client::wallet_client_config` (`client.rs:37`):

```rust
pub async fn wallet_client_config(client: &Client) -> anyhow::Result<WalletClientConfig> {
    let config = client.config().await;
    let (_, wallet_config) =
        config.get_first_module_by_kind::<WalletClientConfig>(WALLET_V2_KIND)?;
    Ok(wallet_config.clone())
}
```

Type path: `fedimint_walletv2_common::config::WalletClientConfig`
(`fedimint-walletv2-common-0.12.0/src/config.rs:215`).

### 19.2 Actual defaults (from source)

`WalletClientConfig` itself has **no `Default` impl** and no constructor. The defaults are set when
the federation builds its consensus config via `WalletConfigConsensus::new(bitcoin_pks,
fee_consensus, network)` (`config.rs:77`); the client receives the resulting values in the
federation's `ClientConfig`.

| Field | Source value | Notes |
|---|---|---|
| `dust_limit` | `bitcoin::Amount::from_sat(10_000)` (`config.rs:128`) | 10,000 sat |
| `fee_consensus.base` | `Amount::from_sats(100)` (`config.rs:159`) | 100 sat per input/output |
| `fee_consensus.parts_per_million` | supplied by the federation, `0..=10_000` (`config.rs:152-155`) | No fixed default; max 1%. `FeeConsensus::new` rejects >10,000. |
| `send_tx_vbytes` | derived from `bitcoin_pks.len()` (`config.rs:111-116`) | No single default; 4 guardians → 228 vbytes |
| `receive_tx_vbytes` | derived from `bitcoin_pks.len()` (`config.rs:117-122`) | 4 guardians → 316 vbytes |
| `feerate_base` | `250` (`config.rs:127`) | Source comment: intentionally below the 1 sat/vB vote floor |

The source's guardian-count table (`config.rs:52-72`) includes 1 → 166/192, 4 → 228/316,
5 → 255/369, 6 → 281/423, 7 → 290/440, 8 → 317/494, 9 → 344/548, 10 → 352/565, …,
20 → 565/991 for send/receive vbytes.

**Mainnet assessment.**

- `dust_limit = 10,000 sat` is conservative and mainnet-safe as a policy floor: it is above
  Bitcoin Core's relay dust thresholds (330–546 sat for the common output types), so it does not
  create uneconomical outputs. It is stricter than protocol dust and may reject small but valid
  sends; that is intentional for a federation peg-out.
- `fee_consensus` is a fee policy, not a safety bound. The constructor enforces `base = 100 sat`
  and `parts_per_million <= 10,000` (≤1%), but the client decodes the federation's config without
  re-validating those fields; a malicious federation could publish a hand-crafted larger ppm.
  Treat it as part of the federation trust boundary and surface the quote before sending.
- `send_tx_vbytes`/`receive_tx_vbytes` are consensus constants derived from guardian count. They
  match the standard one-change-input/two-output transaction shape. They are mainnet-usable when
  the federation config is honest and the guardian count is correct; the client does not
  independently recompute them. More inputs than the formula assumes would underprice the mining
  fee, so the federation remains the source of truth.
- `feerate_base = 250` is intentionally below the 1 sat/vB minimum vote floor per the source
  comment; the actual feerate comes from federation consensus, so this is not a mainnet-safety
  concern by itself.

**Federation trust-boundary note (for the fedimint dev).** `WalletClientConfig` is decoded from
the federation's config; the client does **not** re-run `FeeConsensus::new`, so the 1% cap on
`parts_per_million` is not re-validated after decoding, and it does **not** recompute
`send_tx_vbytes`/`receive_tx_vbytes`. A malicious or buggy federation can therefore publish an
out-of-cap relative fee or inconsistent vbytes. The user-visible mitigation is the quote
(`send_fee()` + `FeeQuote`) before `send()`. Ask the fedimint dev whether `WalletClientModule`
should validate the decoded fee cap and vbytes; until then, treat both as federation
trust-boundary items.

### 19.3 Pre-send validation

`send_withdrawal` (`withdrawal.rs:57`) now takes `&Client`, resolves the module and config, and
rejects dust **before** `send_fee()`/`send_fee_quote()`:

```rust
// withdrawal.rs:44
pub fn validate_withdrawal_value(
    value: bitcoin::Amount,
    dust_limit: bitcoin::Amount,
) -> anyhow::Result<()> {
    if value < dust_limit {
        return Err(PegError::WithdrawalBelowDust { value, dust_limit }.into());
    }
    Ok(())
}
```

The typed error is `PegError::WithdrawalBelowDust { value, dust_limit }` (`error.rs:78`). On
rejection, the row is marked `Failed` through `set_withdrawal_failed` (which also clears the open
index) and the typed error is returned. Test:
`withdrawal::tests::dust_value_is_rejected_before_send` (`withdrawal.rs:173`).

`withdrawal::tests::wallet_config_defaults_match_source` constructs a 4-guardian
`WalletConfigConsensus::new(..., FeeConsensus::new(10_000)?, Network::Bitcoin)` and asserts
`dust_limit = 10,000 sat`, `fee_consensus.base = 100 sat`, `parts_per_million = 10,000`,
`send_tx_vbytes = 228`, `receive_tx_vbytes = 316`, and `feerate_base = 250`.

`WalletClientModule::send` still performs its own `value < self.cfg.dust_limit` check and returns
`SendError::DustValue` (`fedimint-walletv2-client-0.12.0/src/lib.rs:380-382`); the pre-send check is
the early, typed path.

The UI-facing minimum is `client::min_withdrawal_sat(client) -> anyhow::Result<u64>`
(`client.rs:47`), re-exported as `berlin_wallet_core::min_withdrawal_sat` (`lib.rs:19`). It returns
`wallet_client_config(client).await?.dust_limit.to_sat()`; PLAN §6 specifies the FFI method
`min_withdrawal_sat() -> Result<u64>` so the UI can display and enforce the same value before
creating a withdrawal row.

A dust-rejected withdrawal is persisted as `Failed` and must be re-created with a new
`WithdrawalId`; the row's `value` is immutable, so retrying `send_withdrawal` on the same row cannot
recover (PLAN §4.2).

### 19.4 Docs links

- <https://docs.rs/fedimint-walletv2-common/0.12.0/fedimint_walletv2_common/config/struct.WalletClientConfig.html>
- <https://docs.rs/fedimint-walletv2-common/0.12.0/fedimint_walletv2_common/config/struct.WalletConfigConsensus.html>
- <https://docs.rs/fedimint-walletv2-common/0.12.0/fedimint_walletv2_common/config/struct.FeeConsensus.html>
- <https://docs.rs/fedimint-walletv2-client/0.12.0/fedimint_walletv2_client/struct.WalletClientInit.html>
- <https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/module/init/struct.ClientModuleInitArgs.html>
- <https://docs.rs/fedimint-core/0.12.0/fedimint_core/config/struct.ClientConfig.html#method.get_first_module_by_kind>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.config>

---

## 20. Real client, BDK wallet, sync tasks, and CLI harness

`cargo check --workspace` + `cargo test --workspace` → **EXIT 0** (37 core tests + 4 CLI tests + 4 FFI tests).
The CLI was also run locally: `berlin-wallet init` created `<data-dir>/mnemonic`,
`<data-dir>/bdk.sqlite`, loaded the persisted wallet on a second run, and printed a regtest
`bcrt1...` address. The RocksDB test (below) reopens the database and verifies journal rows survive the drop;
the retained SQLite fallback tests cover the same invariant.

### 20.1 Client construction (confirmed signatures)

```rust
// fedimint-client-0.12.0/src/client.rs:304
pub async fn builder() -> anyhow::Result<ClientBuilder>;

// fedimint-client-0.12.0/src/client/builder.rs:182
pub fn with_module<M: ClientModuleInit>(&mut self, module_init: M);

// fedimint-client-0.12.0/src/client/builder.rs:396
pub async fn preview(
    self,
    connectors: ConnectorRegistry,
    invite_code: &InviteCode,
) -> anyhow::Result<ClientPreview>;

// fedimint-client-0.12.0/src/client/builder.rs:1483
pub async fn join(
    self,
    db_no_decoders: Database,
    pre_root_secret: RootSecret,
) -> anyhow::Result<ClientHandle>;

// fedimint-client-0.12.0/src/client/builder.rs:483
pub async fn open(
    self,
    connectors: ConnectorRegistry,
    db_no_decoders: Database,
    pre_root_secret: RootSecret,
) -> anyhow::Result<ClientHandle>;

// fedimint-client-0.12.0/src/client.rs:459
pub async fn get_config_from_db(db: &Database) -> Option<ClientConfig>;

// fedimint-client-0.12.0/src/client/handle.rs:52
pub fn start_executor(&self);
```

`client::build_client` (`client.rs:13`) joins a fresh database or opens an existing one:

```rust
let mut builder = Client::builder().await?;
// v2 family only: mintv2 is the primary BTC module.
builder.with_module(Mintv2ClientInit);
builder.with_module(WalletClientInit::default());
let client = if Client::get_config_from_db(&db).await.is_some() {
    builder.open(connectors, db, root_secret).await?
} else {
    builder.preview(connectors, &invite).await?
        .join(db, root_secret).await?
};
let client = Arc::new(client);
client.start_executor();
```

`ClientBuilder::open` verifies the stored pre-root-secret hash, so a restart with the same mnemonic
opens the existing client; a different mnemonic is rejected. `ClientHandleArc =
Arc<ClientHandle>`; `start_executor()` is called on the handle (it forwards to the inner `Client`).
`client_db(&db, db_prefix)` (`client.rs:72`) still supplies `[0x01] ++ u32_le(db_prefix)`;
`Journal::new(db, vec![0x00])` keeps the app partition separate.

The CLI builds the connector registry with:

```rust
// fedimint-connectors-0.12.0/src/lib.rs:340, :113
ConnectorRegistry::build_from_client_defaults() -> ConnectorRegistryBuilder;
ConnectorRegistryBuilder::bind(self) -> anyhow::Result<ConnectorRegistry>;
```

and parses the invite with `InviteCode::from_str(&str)` before `builder.preview(...)`.

### 20.2 Canonical BIP39 root secret

`Bip39RootSecretStrategy` is **not published on crates.io for 0.12.0** (`fedimint-bip39` stops at
0.8.1), but it exists at Git tag `v0.12.0` in `fedimint-bip39/src/lib.rs`. Its canonical
derivation is:

```rust
// fedimint-bip39 v0.12.0 (Git tag), mirrored in seed.rs
impl<const WORD_COUNT: usize> RootSecretStrategy for Bip39RootSecretStrategy<WORD_COUNT> {
    type Encoding = Mnemonic;

    fn to_root_secret(secret: &Mnemonic) -> DerivableSecret {
        const FEDIMINT_CLIENT_NONCE: &[u8] = b"Fedimint Client Salt";
        const EMPTY_PASSPHRASE: &str = "";
        DerivableSecret::new_root(
            secret.to_seed_normalized(EMPTY_PASSPHRASE).as_ref(),
            FEDIMINT_CLIENT_NONCE,
        )
    }
    ...
}
```

The scaffold mirrors that implementation (`seed::Bip39RootSecretStrategy`), and a test asserts it
is identical to `PlainRootSecretStrategy::to_root_secret(&mnemonic.to_seed(""))`:
`seed::tests::bip39_strategy_matches_plain_seed_strategy`. `root_secret_from_mnemonic` then wraps it
in:

```rust
RootSecret::StandardDoubleDerive(Bip39RootSecretStrategy::<12>::to_root_secret(mnemonic))
```

`PlainRootSecretStrategy` (`fedimint-client-module-0.12.0/src/secret.rs:80`) performs the same
inner `DerivableSecret::new_root(seed, "Fedimint Client Salt")`; using the mirrored canonical BIP39
strategy makes the wire-compatible derivation explicit.

The scaffold adds `bip39 = "=2.2.0"` for the mnemonic/seed only; it does **not** enable
`bdk_wallet`'s `keys-bip39` feature. `RootSecret` remains
`fedimint_client::RootSecret` (`client/builder.rs:94`, re-exported at `lib.rs:107`).

**Seed sharing decision:** the BIP39 seed is shared between fedimint and BDK, but the derivations
are domain-separated: fedimint hashes it through the canonical BIP39 strategy (the
`"Fedimint Client Salt"` root) and then applies its federation derivation; BDK uses the standard
BIP32/BIP84 hierarchy (`wpkh(master/84'/coin'/0'/0/*)` and `.../1/*`). No raw private key is shared.
This keeps a single mnemonic backup while avoiding direct key reuse.

### 20.3 BDK wallet and SQLite persistence

```rust
// bdk_wallet-3.2.0/src/wallet/mod.rs:288
pub fn create<D>(descriptor: D, change_descriptor: D) -> CreateParams;
// bdk_wallet-3.2.0/src/wallet/params.rs:158 / :190
pub fn network(self, network: Network) -> Self;
pub fn create_wallet<P>(self, persister: &mut P) -> Result<PersistedWallet<P>, ...>
where P: WalletPersister;

// bdk_wallet-3.2.0/src/wallet/mod.rs:446
pub fn load() -> LoadParams;
// bdk_wallet-3.2.0/src/wallet/params.rs:264 / :299
pub fn descriptor<D>(self, keychain: KeychainKind, expected: Option<D>) -> Self;
pub fn check_network(self, network: Network) -> Self;
// bdk_wallet-3.2.0/src/wallet/persisted.rs:173
pub fn load(persister: &mut P, params: LoadParams)
    -> Result<Option<PersistedWallet<P>>, ...>;
// bdk_wallet-3.2.0/src/wallet/persisted.rs:193
pub fn persist(&mut self, persister: &mut P) -> Result<bool, P::Error>;

// bdk_wallet-3.2.0/src/lib.rs:48
pub use bdk_chain::rusqlite;
// bdk_wallet-3.2.0/src/wallet/persisted.rs:283
impl WalletPersister for bdk_chain::rusqlite::Connection;
```

`bdk::BdkWallet` (`bdk.rs:50`) uses `rusqlite::Connection` at `<data-dir>/bdk.sqlite`
(`BDK_DB_FILE`, `bdk.rs:13`), creates with BIP84 descriptors and loads with the same descriptor
checks. The descriptors are derived from `Xpriv::new_master(network, seed)`
(`bitcoin-0.32.8/src/bip32.rs:576`).

Signing/recording uses:

```rust
// bdk_wallet-3.2.0/src/wallet/mod.rs:671, :1272, :1884, :2749
Wallet::reveal_next_address(KeychainKind::External);
Wallet::build_tx();
Wallet::sign_with_signers(&mut psbt, &[&ext, &int], SignOptions::default());
Wallet::apply_unconfirmed_txs([(tx, last_seen)]);
```

`BdkWallet::build_and_finalize` (`bdk.rs:132`) is async: it signs/finalizes, **broadcasts through
the configured Esplora URL**, then records the tx and persists. `reveal_destination` persists the
new address index immediately (`bdk.rs:122`). `BdkWallet::is_our_address` (`bdk.rs:113`) is the only gate for a local withdrawal script scan.

### 20.4 Esplora chain source

```rust
// bdk_esplora-0.22.1/src/async_ext.rs:47
async fn sync<I: Send, R: Into<SyncRequest<I>> + Send>(
    &self,
    request: R,
    parallel_requests: usize,
) -> Result<SyncResponse, Error>;

// bdk_wallet-3.2.0/src/wallet/mod.rs:2983, :2459
Wallet::start_sync_with_revealed_spks() -> SyncRequestBuilder<(KeychainKind, u32)>;
Wallet::apply_update(update: impl Into<Update>) -> Result<(), CannotConnectError>;

// esplora-client-0.12.3/src/lib.rs:190
Builder::new(base_url).build_async() -> Result<AsyncClient, Error>;
// esplora-client-0.12.3/src/async.rs:375
AsyncClient::broadcast(&transaction) -> Result<(), Error>;
```

`BdkWallet::sync` (`bdk.rs:161`) does a **partial** sync over revealed scripts, applies the update,
and persists. Sync and broadcast are each bounded by `ESPLORA_TIMEOUT = 30 s` (`bdk.rs:16`). It does
not perform a full gap scan; a restored wallet with unknown historical addresses would need
`full_scan`/`start_full_scan` added. The CLI defaults to regtest and `http://127.0.0.1:3002`
(`main.rs`).

### 20.5 Sync tasks

- `sync::bdk_chain_task(wallet: Arc<Mutex<BdkWallet>>, shutdown: watch::Receiver<bool>)`
  (`sync.rs:59`): immediate first pass, then `BDK_SYNC_INTERVAL = 30 s` with ±10% jitter; logs and
  retries errors; exits on shutdown.
- `sync::fedimint_sync_task(client: ClientHandleArc, journal: Journal, shutdown: watch::Receiver<bool>)`
  (`sync.rs:85`): calls `client.spawn_federation_reconnect()`, drains
  `client.get_event_log_transient_receiver()`, then calls
  `withdrawal::reconcile_submitted(client, &journal)` on a `FEDIMINT_SYNC_INTERVAL = 30 s` jittered
  tick; handles `Empty`/`Lagged`/`Closed`; exits on shutdown.
- Withdrawal reconciliation uses `await_final_send_operation_state(Success(txid))` and calls
  `journal.set_broadcast(txid)`; it does **not** require the destination to be in the BDK wallet.
  A local BDK scan is valid only when `BdkWallet::is_our_address(address)` is true.
- `sync::adoption_task` keeps its existing 60 s schedule and shutdown handling.

### 20.6 CLI harness

`crates/berlin-wallet-cli` (binary `berlin-wallet`) is a manual-run harness:

- `init --network <net>` — create/open the age-encrypted BIP39 mnemonic and SQLite BDK wallet,
  print a regtest address/balance; prompts for a wallet passphrase (env for automation).
- `join <invite>` — build a client, print federation id, client identity, wallet network, and minimum withdrawal.
- `backup <destination-file>` / `restore-backup <source-file>` — age-encrypted tar of the full data
  directory (`mnemonic`, `bdk.sqlite` + `-wal`/`-shm`, `fedimint.rocksdb` and sidecars); separate
  backup passphrase; non-blocking DB lock check; app must be stopped.
- `restore <invite> [esplora-url] [expected-fed-id] [expected-client-identity]` — recover on a fresh
  data dir, assert identity/federation, then full-scan BDK.
- `run <invite> [esplora-url]` — interactive session with `balance`, `ecash-balance`, `address`,
  `min`, `request-deposit <sat>`, `fund-deposit <id>`, `deposit <sat>` (both steps), `watch <id>`,
  `request-withdraw <address> <sat>`, `send-withdraw <id>`, `cancel-withdraw <id>`, `withdraw
  <address> <sat>` (both steps), `cancel-deposit <id>`, `list-withdrawals`, `status <id>`,
  `operations`, `operation-fees <id>`, `await-change <id>`, `sync`, `quit`.
- Data dir: `$BERLIN_WALLET_DATA_DIR` or `.berlin-wallet`; `run` uses the chain-id-derived network,
  `init` defaults to regtest.
- Persistence: `open_database` (`main.rs`) calls `client::open_rocksdb(<data-dir>/fedimint.rocksdb)`.
  Journal and client state share that RocksDB under `[0x00]` / `[0x01] ++ u32_le(0)`. On restart,
  `build_client` sees `Client::get_config_from_db(&db)` and calls `ClientBuilder::open` instead of
  joining again. The CLI's `join`/`run` commands therefore reopen the same client and journal.
- The withdrawal destination is caller-supplied: `request_withdrawal` (`withdrawal.rs:19`) stores
  the address and `send_withdrawal` (`withdrawal.rs:121`) parses it. The BDK wallet remains the
  peg-in funding source.

### 20.7 Primary RocksDB backend + retained SQLite fallback

**Availability re-check (before changing anything):**

- `fedimint-rocksdb` on crates.io: versions through **0.8.1 only**; no **0.12.0** release.
- `fedimint-sqlite`: **does not exist** at tag `v0.12.0` and is not published. The earlier 404 was
  correct; the root tree has no `fedimint-sqlite` directory.
- The `v0.12.0` workspace does publish DB backends as workspace crates:
  `fedimint-rocksdb` and `fedimint-cursed-redb` (plus `fedimint-db-locked`). None of those are on
  crates.io at 0.12.0 (`fedimint-cursed-redb` is not on crates.io at all).
- The Git tag **does** contain `fedimint-rocksdb` 0.12.0 and `fedimint-db-locked` 0.12.0.

**Chosen:** `fedimint-rocksdb` from Git tag `v0.12.0`, with `[patch.crates-io]` entries for every
`fedimint-*` workspace package so the whole graph resolves to one Git-fedimint-core instance. This
replaces the earlier SQLite primary while keeping the hand-rolled SQLite backend available.

Patch set:
- All 88 `fedimint-*` workspace packages are patched to
  `git = "https://github.com/fedimint/fedimint", tag = "v0.12.0"`.
- `fedimint-threshold-crypto` is **not** a workspace package (it is an external package dependency
  named via `package = "fedimint-threshold-crypto"`); its patch entry was removed because Cargo
  refused to resolve it from the Git workspace.
- Cargo emits unused-patch warnings for server/gateway/testing crates that are not in the client
  graph. They are harmless but noisy; removing them would defeat the "every crate" requirement.

Cascade effects observed:
- `fedimint-core`, `fedimint-client`, modules, connectors, eventlog, etc. now come from the same
  Git revision (`2ead392b`), so there is one `Database`/`IRawDatabase` type.
- New/replaced transitive packages: `fedimint-db-locked`, `fs-lock`, `fs4`, `bon`,
  `rocksdb 0.24.0`, `librocksdb-sys 0.17.3+10.4.2`, plus the RocksDB native compression deps
  (`zstd-sys`, `lz4-sys`, `libz-sys`, `bzip2-sys`). The lockfile now points the fedimint packages
  at the Git source.
- Build requirement: a C++17 compiler is mandatory. This sandbox had `cc` but no `c++`/`g++`;
  verification used a local Zig 0.17 C++ wrapper and a combined libc++/libc++abi/libunwind/ubsan
  static runtime. CI/packaging must install `g++` or `clang++`; do not depend on the wrapper.
- `RocksDb::open()` uses `block_in_place`, so the RocksDB test uses a multi-thread Tokio runtime.

Wiring:
```rust
// client.rs
pub async fn open_rocksdb(path: &Path) -> anyhow::Result<Database> {
    let raw = fedimint_rocksdb::RocksDb::build(path).open().await?;
    Ok(Database::new(raw, ModuleDecoderRegistry::default()))
}
```
The CLI uses `<data-dir>/fedimint.rocksdb`; `ClientBuilder::open` reopens the stored client config on
restart. `client::open_sqlite` and `sqlite::SqliteDb` remain available (and tested) for mobile or
environments where RocksDB is not appropriate.

Persistence tests:
- `client::tests::rocksdb_journal_rows_survive_reopen` — inserts a journal row, drops the
  `Database`, reopens the RocksDB path, and reads the row back.
- `sqlite::tests::journal_rows_survive_reopen` / `raw_rows_survive_reopen` — SQLite fallback.
- `sqlite::tests::write_conflict_is_reported` — SQLite optimistic conflict.
- Client-config persistence is wired through the same `Database` + `ClientBuilder::open`; an
  end-to-end federation restart was not run in this environment (no guardian).

RocksDB APIs: <https://github.com/fedimint/fedimint/blob/v0.12.0/fedimint-rocksdb/src/lib.rs>,
<https://docs.rs/rocksdb/0.24.0/rocksdb/struct.OptimisticTransactionDB.html>.

---
### 20.8 Docs links

- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.builder>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientBuilder.html#method.with_module>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientBuilder.html#method.preview>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientPreview.html#method.join>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientBuilder.html#method.open>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.get_config_from_db>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientHandle.html#method.start_executor>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/enum.RootSecret.html>
- <https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/secret/trait.RootSecretStrategy.html>
- <https://docs.rs/fedimint-client-module/0.12.0/fedimint_client_module/secret/struct.PlainRootSecretStrategy.html>
- <https://docs.rs/fedimint-connectors/0.12.0/fedimint_connectors/struct.ConnectorRegistry.html#method.build_from_client_defaults>
- <https://docs.rs/bdk_wallet/3.2.0/bdk_wallet/struct.Wallet.html#method.create>
- <https://docs.rs/bdk_wallet/3.2.0/bdk_wallet/struct.PersistedWallet.html>
- <https://docs.rs/bdk_wallet/3.2.0/bdk_wallet/struct.Wallet.html#method.start_sync_with_revealed_spks>
- <https://docs.rs/bdk_esplora/0.22.1/bdk_esplora/trait.EsploraAsyncExt.html#method.sync>
- <https://docs.rs/esplora-client/0.12.3/esplora_client/struct.AsyncClient.html#method.broadcast>
- <https://docs.rs/bdk_chain/0.23.3/bdk_chain/rusqlite/struct.Connection.html>
- <https://docs.rs/bitcoin/0.32.8/bitcoin/bip32/struct.Xpriv.html#method.new_master>
- <https://docs.rs/bip39/2.2.0/bip39/struct.Mnemonic.html>
- <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/trait.IRawDatabase.html>
- <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/trait.IRawDatabaseTransaction.html>
- <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/trait.IDatabaseTransactionOpsCore.html>
- <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/enum.DatabaseError.html>
- <https://docs.rs/rusqlite/0.31.0/rusqlite/struct.Connection.html>
- <https://github.com/fedimint/fedimint/blob/v0.12.0/fedimint-bip39/src/lib.rs>


---

## 21. Chain ID → network (Mutinynet)

### 21.1 Server function is not client-reachable

`network_from_chain_id` is in `fedimint-server-core/src/bitcoin_rpc.rs:35`. `fedimint-server-core`
is not a dependency of `fedimint-client`, `fedimint-client-module`, `fedimint-connectors`, or any
client-side crate in our graph. It is replicated client-side instead of adding a server crate.

The four constants (`bitcoin_rpc.rs:21-31`) are:

| Network | Block-1 hash |
|---|---|
| mainnet | `00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048` |
| testnet | `00000000b873e79784647a6c82962c70d228557d24a747ea4d1b8bbe878e1206` |
| signet-4 | `00000086d6b2636cb2a392d45edc4ec544a10024d30141c9adf4bfd9de533b53` |
| mutinynet | `000002855893a0a9b24eaffc5efc770558a326fee4fc10c9da22fc19cd2954f9` |

Unknown hashes map to `Network::Regtest`. Mutinynet and signet-4 both map to `Network::Signet`, so
`Network` alone cannot distinguish them; the Esplora/chain-source URL must.

### 21.2 Real client path for the chain id

`fedimint_core::ChainId(pub bitcoin::BlockHash)` (`fedimint-core/src/lib.rs:194`). The client obtains
it with:

```rust
// fedimint-client-0.12.0/src/client.rs:599
pub async fn chain_id(&self) -> anyhow::Result<ChainId>
```

`Client::chain_id` first reads `ChainIdKey` from the client DB; on a miss it fetches
`self.api.chain_id().await?` (`client.rs:612`) and caches it (`client.rs:616`). The federation's
`GlobalClientConfig` does **not** carry a chain id. Fetching `/block-height/1` directly from the
chain source is the server-side fallback (`fedimint-server-core/src/bitcoin_rpc.rs`), not the
client path.

### 21.3 Client-side implementation and BDK wiring

`berlin-wallet-core::network` (`network.rs`):

```rust
pub fn network_from_chain_id(chain_id: ChainId) -> Network; // network.rs:26
pub async fn client_network(client: &Client) -> anyhow::Result<Network>; // network.rs:37
```

Tests (`network.rs`): all four constants, plus unknown-hash → Regtest.

The CLI derives the network **after** joining/opening the client:

```rust
let chain_network = client_network(&client).await?;
let bdk = BdkWallet::open_or_create(
    &data_dir.join(BDK_DB_FILE),
    seed,
    chain_network,
    esplora_url,
)?;
```

`init` has no federation yet and defaults to `Network::Regtest`. Because signet-4 and mutinynet both
produce `Network::Signet`, the BDK address prefix is the same (`tb1...`); use the correct Esplora
URL (or a separate data dir) to distinguish them.

Docs:
- <https://docs.rs/fedimint-core/0.12.0/fedimint_core/struct.ChainId.html>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.chain_id>
- <https://docs.rs/bitcoin/0.32.8/bitcoin/enum.Network.html>
- <https://github.com/fedimint/fedimint/blob/v0.12.0/fedimint-server-core/src/bitcoin_rpc.rs>


---

## 22. Root secret byte-exact diff vs `fedimint-bip39` v0.12.0

### 22.1 Line-by-line diff

Git source: `fedimint-bip39/src/lib.rs` at tag `v0.12.0` (not on crates.io for 0.12.0).
Mirror: `crates/berlin-wallet-core/src/seed.rs`.

| Element | Canonical Git (`fedimint-bip39/src/lib.rs`) | Mirror (`seed.rs`) |
|---|---|---|
| Seed input | `secret.to_seed_normalized(EMPTY_PASSPHRASE)` at `:21-27` | `secret.to_seed_normalized(EMPTY_PASSPHRASE)` at `:61-72` |
| Passphrase | `const EMPTY_PASSPHRASE: &str = ""` (`:23`) | identical |
| Salt | `const FEDIMINT_CLIENT_NONCE: &[u8] = b"Fedimint Client Salt"` (`:24`) | identical |
| Root constructor | `DerivableSecret::new_root(seed.as_ref(), FEDIMINT_CLIENT_NONCE)` (`:25-28`) | identical |
| `consensus_encode` | `secret.to_entropy().consensus_encode(writer)` (`:30-32`) | identical |
| `consensus_decode_partial` | `Vec::<u8>::consensus_decode_partial(..., ModuleRegistry::default())?` then `Mnemonic::from_entropy(&bytes).map_err(DecodeError::from_err)` (`:34-41`) | identical |
| `random` | `Mnemonic::generate_in_with(rng, Language::English, WORD_COUNT).expect(...)` (`:43-50`) | identical |
| `RootSecret` variant | `RootSecret::StandardDoubleDerive(Bip39RootSecretStrategy::<12>::to_root_secret(mnemonic))` (`fedimint-cli/src/lib.rs:420`) | identical (`seed.rs:90`) |

**Salt check for the Plain test.** `PlainRootSecretStrategy` uses the same salt:
`fedimint-client-module/src/secret.rs:86` has
`const FEDIMINT_CLIENT_NONCE: &[u8] = b"Fedimint Client Salt";` and
`DerivableSecret::new_root(secret.as_ref(), FEDIMINT_CLIENT_NONCE)`. Therefore comparing the
mirrored BIP39 strategy against `PlainRootSecretStrategy` is valid; their salts are identical.
`bip39_strategy_matches_plain_seed_strategy` (`seed.rs`) asserts the 32-byte derived root is equal.

**Byte-exact golden vector.** `seed::tests::bip39_matches_canonical_golden_vector` pins:

```text
mnemonic: abandon abandon ... about (12 words, empty passphrase)
root.to_random_bytes::<32>() =
6c 63 84 4c f0 9e f2 98 e8 4a 57 50 c2 7d 9d c0
5d cf 45 c9 9c b8 2e 0c 27 f9 03 aa c3 a5 7d ba
```

**Dependency note.** Git `fedimint-bip39` depends on `bip39 = "2.2.2"` via its workspace, which is
not published (crates.io max `2.2.0`). The mirror uses `bip39 =2.2.0`; the BIP39 `to_seed` output
is identical for the same mnemonic/passphrase, so this is a packaging difference, not a derivation
difference.

Docs:
- <https://github.com/fedimint/fedimint/blob/v0.12.0/fedimint-bip39/src/lib.rs>
- <https://github.com/fedimint/fedimint/blob/v0.12.0/fedimint-client-module/src/secret.rs>
- <https://docs.rs/bip39/2.2.0/bip39/struct.Mnemonic.html#method.to_seed_normalized>
- <https://docs.rs/fedimint-client/0.12.0/fedimint_client/secret/struct.PlainRootSecretStrategy.html>

---

## 23. Pre-join checks

### 23.1 `ModuleDecoderRegistry::default()` for the client DB is correct

The `Database` passed to `ClientPreview::join` / `ClientBuilder::open` is named
`db_no_decoders`. `ClientBuilder::build_stopped` computes module decoders, decodes the federation
config with them, then installs them:

```rust
// fedimint-client-0.12.0/src/client/builder.rs:627-630
let decoders = self.decoders(config);
let config = Self::config_decoded(config, &decoders)?;
let db = db_no_decoders.with_decoders(decoders.clone());
```

`Database::with_decoders` is `fedimint-core/src/db/mod.rs:475`. `open_rocksdb`/`open_sqlite`
(`client.rs`) therefore correctly construct `Database::new(raw, ModuleDecoderRegistry::default())`:
that is the no-decoder input; the client builder supplies decoders from the module inits and
federation config. An empty registry is not a federation fault.

### 23.2 Pinned Git rev is the tag commit

`git ls-remote https://github.com/fedimint/fedimint`:

```text
6cdc15b1c5df2e3849db532d9e5b833dfd079890  refs/tags/v0.12.0
2ead392ba22141bbe5898cdb724ec3dbc55387a9  refs/tags/v0.12.0^{}
8c9d15c7bf62605efb966e5e6b6b468d2ef8618b  refs/heads/master
```

Cargo's checkout is `2ead392b` — the peeled **tag** commit, not `master`. The `Cargo.lock` source
fragment shows `?tag=v0.12.0#2ead392b`.

### 23.3 C++17 build requirement

`librocksdb-sys` compiles RocksDB C++17 and requires a real `g++` or `clang++`; `cc` alone is not
enough. This is documented in PLAN §2. The Zig C++ wrapper + combined libc++/ubsan runtime used to
verify this sandbox is **sandbox-only and uncommitted**; CI must install a C++17 compiler.

Docs: <https://docs.rs/fedimint-core/0.12.0/fedimint_core/db/struct.Database.html#method.with_decoders>,
<https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientBuilder.html#method.open>,
<https://github.com/fedimint/fedimint/tree/v0.12.0>.


---

## 24. First live join: exact outcome

Command/data:
```text
BERLIN_WALLET_DATA_DIR=/tmp/opencode/berlin-join
berlin-wallet join fed11qgqycjnfwfhksw309umnqwpsxsurvve4vvcn2de3v43rgvfhxucnwdrp8qmnyvmyvvmnwvnrvsekgcnxxu6nwdf3v93kxwps8yenqdfnxgunwetyv3skzenp9amrzqqpyqzas0ps93dvl7wgu2xj9nh63vlw4t9lmh4qh5kkqtsewgccqyunv5dewzq
berlin-wallet run <invite> https://mutinynet.com/api
```

### 24.1 First attempt: panic before federation I/O

Exact stderr:

```text
thread 'main' panicked at fedimint-client/src/sm/executor.rs:394:13:
start_executor was called previously
```

The call chain ended at our `berlin-wallet-core/src/client.rs:44` call to
`ClientHandle::start_executor`. `ClientBuilder::join`/`open` already start the executor internally
(`fedimint-client/src/client/builder.rs:582-583` and `:545`). Fix: removed the extra
`client.start_executor()` from `build_client`.

### 24.2 Second attempt: join succeeded

Exact stdout:

```text
federation id: 05d83c302c5acff9c8e28d22cefa8b3eeaacbfddea0bd2d602e1972318013936
chain-id network: signet
config network: signet
module 0: lnv2
module 1: meta
module 2: mintv2
module 3: walletv2
primary BTC module: Some(2)
min withdrawal: 10000 sat
```

So the chain-id derivation points at Signet, the config agrees, and the federation's primary BTC
module is **mintv2**, not v1 mint.

### 24.3 Deposit attempt

`berlin-wallet run <invite> https://mutinynet.com/api`, then:

```text
address
deposit 50000
```

Exact stdout:
```text
bdk address: tb1qzdj0g2cdhj3zemu8sszfcd57tmanhv5wvjy6ak
...
deposit 01a14b81-efbf-7113-9673-efcd0d024fba state=AddressShown address=Some("tb1qen7rhvlway2w3q7w6ktykd5g3wz5780jkp5f67c4zkqfdw6wjj3snydtvm") (address write: Applied)
```

Exact stderr:
```text
error: Insufficient funds: 0 BTC available of 0.00050054 BTC needed
```

Failing source: `bdk_wallet-3.2.0/src/wallet/coin_selection.rs:138` (coin selection for the peg-in
funding transaction). The deposit row remains `AddressShown` for later funding; no on-chain
Mutinynet funds were available to this harness.

### 24.4 Withdrawal attempt before mintv2 was registered

Exact stderr:
```text
error: No module to balance a partial transaction (affected unit: AmountUnit(0)
```

Failing source: the fee-quote dry-run in `fedimint-client/src/client.rs:911` (the same bail exists
in the finalize path at `:768`). The federation exposes `mintv2`, but the client only registered
v1 `MintClientInit`, so there was no primary module for `AmountUnit::BITCOIN`. Fix: replaced v1 `fedimint-mint-client` with `fedimint-mintv2-client` and registered `Mintv2ClientInit`.

### 24.5 Withdrawal attempt after mintv2 registration

The previous failed attempt left a `Quoting` row; added a `cancel-withdraw <id>` CLI command and
cancelled it. Retry:

```text
cancel-withdraw 01a14b83-8506-7656-89c4-ab3a447c447b
withdraw tb1qcw496t7p4cyjg8a96h9wwff3nukn04au7dkqj3 10000
```

Exact stderr:
```text
error: Insufficient funds
```

Failing source: `fedimint-mintv2-client/src/lib.rs:503`
(`.context("Insufficient funds")?` inside `create_final_inputs_and_outputs`). The client has zero
ecash balance, so the fee quote cannot fund the withdrawal. The row remains `Quoting` (quote errors
are left for retry); `cancel-withdraw` releases it. No Mutinynet ecash was available to fund the
withdrawal.

### 24.6 What was fixed by the live run

- Removed the duplicate `start_executor()` call (`build_client`).
- Replaced v1 `fedimint-mint-client` with `fedimint-mintv2-client`; `mintv2` is now the only
  registered mint client and the primary BTC module.
- Added `cancel-withdraw` to recover a `Quoting` row left by a failed quote.
- Added module/primary diagnostics to `join`.

No funds were fabricated or papered over: the deposit and withdrawal both fail for the honest
reason that the harness has no Mutinynet BTC/ecash.


### 24.7 v2 module family decision

The live federation exposes `lnv2`, `meta`, `mintv2`, `walletv2`. The client now registers:

- `Mintv2ClientInit` (`fedimint-mintv2-client`) — **required** primary BTC module.
- `WalletClientInit` — walletv2.
- v1 `fedimint-mint-client` — **removed** from dependencies and registration; it is not part of the
  v2 federation config.
- `fedimint-lnv2-client` — exists and can be registered. It is now in the CLI-first MVP scope but is
  **not registered yet**; the join still succeeds with the lnv2 module present in the config.

`lnv2` client crate source: `modules/fedimint-lnv2-client` at Git `v0.12.0`.

---

## 25. Mutinynet round trip: completed

### 25.1 Funding

Faucet API auth remained blocked (`/api/onchain` → `{"error":"Missing token"}`; GitHub device flow
kept returning `authorization_pending`). The user funded the shared BIP84 BDK address externally:

```text
tb1qnlyj5pq3wfvhh969c5rtpe373tqdmy6stym9vt
```

Funding transaction:

```text
txid: ec47201728e440b04a79f97978213ed31772281d9f87b6e663de04c3edea474b
status: confirmed, block 3514048
vout: 100000 sat to tb1qnlyj5pq3wfvhh969c5rtpe373tqdmy6stym9vt
```

BDK after sync: `confirmed: 0.00100000 BTC`.

### 25.2 Deposit: Done

Commands:

```text
deposit 50000
watch 01a14b81-efbf-7113-9673-efcd0d024fba
ecash-balance
balance
```

Exact key output:

```text
deposit 01a14b81-efbf-7113-9673-efcd0d024fba state=AddressShown address=Some("tb1qen7rhvlway2w3q7w6ktykd5g3wz5780jkp5f67c4zkqfdw6wjj3snydtvm")
funding txid: 6db9c9637416547d209fa4451af68de454eb7dc4fe920f7bf1eb601cacea4aaa
state: Done
next_position: EventLogId(2)
funding_outpoint: 6db9c963...:0
claim_fee: 324 SAT
gross_deposited: 50000 SAT
net_credit: 49676000msat
ecash-balance: 49570304 msat (49570 sat)
BDK: { confirmed: 0.00049847 BTC }
```

Funding transaction vouts:

```text
tb1qen7rhvlway2w3q7w6ktykd5g3wz5780jkp5f67c4zkqfdw6wjj3snydtvm  50000
tb1qtxnylczcl9tflxn6ju42c8ttn075ugck670w5p                     49847
status: confirmed, block 3514050
```

The deposit row reached `Done`. The ecash balance is **105,696 msat lower** than
`net_credit` (49,676,000 vs 49,570,304 msat); this is the observed post-claim balance, recorded
without rounding it into the row.

### 25.3 Withdrawal: Broadcast

A stale `Quoting` row from the pre-funding attempts was cancelled first:

```text
cancel-withdraw 01a14b84-5609-7419-a0e9-213e1743f690
cancel result: Applied
```

Then:

```text
withdraw tb1qav6qxadyxh3pcndq788fzk79njhwvc3eycmgkf 10000
```

Exact key output:

```text
withdrawal 01a14c06-0327-77b5-948a-eed900a8055d
value: 10000 SAT
quote: input 800msat, output 100600msat, dust 360msat
state: Broadcast { txid: 70a3f0c5807264b2877abe703967c975c5d10d32108da009d676fd1e18c0d6b6 }
txid: 70a3f0c5807264b2877abe703967c975c5d10d32108da009d676fd1e18c0d6b6
ecash-balance: 39234560 msat (39234 sat)
```

On-chain status:

```text
txid: 70a3f0c5807264b2877abe703967c975c5d10d32108da009d676fd1e18c0d6b6
confirmed: true, block 3514059
vouts: tb1qav6qxadyxh3pcndq788fzk79njhwvc3eycmgkf 10000
       tb1qancqrg42fqrl62294qp3cydr08ds892lyshff6p2wkjs33m7s5aqu3tnqv 1139104
```

BDK after sync: `confirmed: 0.00059847 BTC` — exactly +10,000 sat from the withdrawal output.

Ecash drop: `49,570,304 - 39,234,560 = 10,335,744 msat` = 10,000 sat value + 233.984 sat
on-chain fee + 101.76 sat federation quote. The on-chain fee is not persisted in the withdrawal
row; the quote is.

### 25.4 Result

Deposit and withdrawal both completed on Mutinynet. No failures occurred after funding; the only
remaining faucet issue is automated OAuth, which was bypassed by the user funding the address
directly.

---

## 26. Net credit now includes operation-level fees

### 26.1 Source of the discrepancy

`ReceiveMeta.fee` only covers the on-chain claim fee. The primary mintv2 module charges an
additional operation fee, reported by:

```rust
// fedimint-client/src/client.rs:1209
pub async fn get_operation_fees(&self, operation_id: OperationId)
    -> anyhow::Result<Option<Amounts>>;
```

For the first Mutinynet deposit:

```text
gross: 50000 sat
ReceiveMeta.fee: 324 sat
Client::get_operation_fees: Some(Amounts({AmountUnit(0): 105696msat}))
old row net_credit: 49676000 msat
actual balance delta: 49570304 msat
```

So the row over-reported by exactly 105,696 msat. The missing fee is the mintv2
issuance/output fee, not a `get_balance_for_unit` omission.

### 26.2 Fix

`Journal::finish_deposit` / `close_deposit_done_if_open` / `claim_adopt_if_absent` now receive an
`operation_fee: Option<fedimint_core::Amount>` and use:

```rust
operation_fee.map(|fee| net_credit(gross, fee).saturating_sub(fee))
```

The deposit flow obtains it with `Client::get_operation_fees(claim.operation_id)` and passes the
BTC amount to the journal (`deposit.rs::operation_fee`). `AdoptedClaim` carries the same field.
`finish_deposit` also persists `operation_id` so a later reconciliation pass can find the fee.

If the fee lookup returns `None`, the journal leaves `net_credit` unset instead of writing zero
(which would over-report). `deposit::reconcile_missing_net_credits` runs from the adoption task and
fills it once `get_operation_fees` returns a BTC amount. The lookup is bounded to 5 attempts with
exponential backoff (500 ms … 2 s).

Rows awaiting a fee are tracked by a dedicated raw index (`TAG_PENDING_NET_CREDIT = 0x07`,
key `[0x07] ++ deposit_id`), written and removed in the same `dbtx` as the row. The reconciliation
lookup is a prefix scan over that index plus one read per pending row — it does **not** scan all
deposit rows each tick. If the operation-log entry has been pruned and the fee never becomes
retrievable, the row stays in the index and `net_credit` stays `NULL`/pending forever; there is no
`gross − claim_fee` fallback because that would over-report spendable credit.

Tests:
`journal::tests::net_credit_includes_operation_fee`,
`journal::tests::deposit_row_net_credit_matches_observed_balance_delta`,
`journal::tests::deposit_net_credit_stays_unset_until_fee_known`,
`deposit::tests::missing_operation_fee_is_not_treated_as_zero`, and
`journal_db::tests::net_credit_reconciles_from_none` pin the observed vector and the None path
(in-memory and DB-backed journals). `crates/berlin-wallet-cli` also has
`pending_net_credit_is_not_rendered_as_zero`, so the CLI displays `pending`, not `0`:

```text
50000 sat − 324 sat − 105696 msat = 49570304 msat
```

Live verification after the fix:

```text
20,000 sat deposit: net_credit=19574784 msat; balance 39234560 -> 58809344 (delta 19574784)
10,000 sat deposit: net_credit=9574912 msat;  balance 58809344 -> 68384256 (delta 9574912)
```

The row now equals the real credited delta. The CLI `status`/`watch` output renders an unset
`net_credit` as `net_credit: pending (operation fee not available yet)`, never as zero.

---

## 27. Crash/restart matrix (Mutinynet)

The deterministic CI matrix is now in §53; these Mutinynet notes remain the
live evidence, and §53 marks the one window that still needs devimint.

Data dir `/tmp/opencode/berlin-join`; all rows/journal/client state in RocksDB. "Kill" means
`SIGKILL`/`abort`, then a fresh process reopens the same DB. `BERLIN_WALLET_CRASH_POINT` is a
debug-only fault injection: `fault.rs` is gated by `#[cfg(debug_assertions)]` and is a no-op in
release builds.

### 27.1 After AddressShown, before funding

- Killed after `request-deposit 10000` reached `AddressShown`.
- Restart: row `01a14c26-986b-74d6-b8c8-a68f65a2ab60` was `AddressShown`, `funding_txid=None`;
  ecash balance unchanged at `58,809,344 msat`.
- No double credit, no stuck row beyond the intended single open slot.

### 27.2 After funding broadcast, before the claim

- Killed immediately after `fund-deposit` printed:
  `22bd63b4198b4771f7a6c572af72dfea87044be8bba0beae122582071b1c1fe2`.
- Restart: row stayed `AddressShown` with that `funding_txid`; calling `fund-deposit` again
  returned the **same txid** and did not broadcast a second transaction.

### 27.3 During Confirming

- Restarted the funded row, started `watch`, killed the process after 5 s.
- Restart `watch` reached `Done` exactly once:
  `net_credit=9574912 msat`; balance `58,809,344 -> 68,384,256` (delta `9,574,912`).
- No duplicate credit.

### 27.4 After Done

- Restart: row `01a14c26-...` remained `Done`, `net_credit=9574912 msat`; balance stayed
  `68,384,256 msat`.
- A new `request-deposit 10000` was accepted as `01a14c2b-3f26-72e5-8b91-128f9a0e9e93`
  (`AddressShown`), proving the single-flight slot was released; it was then cancelled
  (`cancel result: Applied`).

### 27.5 Mid-withdrawal: after `send()` before operation-id persistence

- `BERLIN_WALLET_CRASH_POINT=after_send` aborted the process after `send()` returned:
  `fault injection: aborting at after_send` (exit 134).
- Restart found `01a14c2b-6e56-75b3-b71b-b8ab9e0182ac` still `Quoting`; balance was temporarily
  `55,801,344 msat` (change output not yet finalized).
- `send-withdraw <id>` recovered the operation id from the walletv2 operation log by
  `custom_meta.id` (`find_sent_operation`), awaited the final state, and set:
  `Broadcast { txid: 56decb69a8a935173fb7efd2a2518f4ae2c3a46cf92f06916e3aba2b21d3ca42 }`.
- Final balance `58,048,512 msat`. Operation log contains exactly **one** Send entry for that
  withdrawal id; no second send.

### 27.6 After Broadcast, before the row update

- Pre-fix, `after_broadcast` aborted after `Success(txid)` but before `set_broadcast`; the row was
  `Submitted`, and the change outputs were not yet credited:
  balance `41,941,504 msat`, 5,771,264 msat below the settled value. `await-change` for the send
  operation finalized the change and recovered it to `47,711,232 msat`.
- Fix: `await_change_outputs` now waits for every primary-module change outpoint from
  `SendMeta.change_outpoint_range` before the crash hook and `set_broadcast`. The wait is bounded
  (`CHANGE_OUTPUT_TIMEOUT = 20 s`, inside the 30 s reconcile timeout). On timeout the row remains
  `Submitted`; the next reconciliation pass retries instead of blocking.
  `withdrawal::tests::change_wait_timeout_leaves_reconciliation_headroom` pins the inner/outer
  timeout ordering.
- Post-fix repetition (`01a14c2f-2b9d-70d5-b1a0-41484bdb5500`):
  - crash at `after_broadcast`, restart row `Submitted`, balance `36,402,688 msat`;
  - `send-withdraw` set
    `Broadcast { txid: 7e39097af1bfe34ffa674f717fa8ffb5f8166d93c8c15bdac28a99cc139f0b4e }`;
  - balance stayed `36,402,688 msat` (change already awaited before the crash);
  - exactly one Send entry for that id; no second send.

### 27.7 Faucet-auth note

The faucet API requires a Bearer JWT:

```text
POST https://faucet.mutinynet.com/api/onchain
{"sats":100000,"address":"tb1..."}
HTTP/2 401
{"error":"Missing token"}
```

GitHub device flow remained `authorization_pending` and expired; the round trip was completed by
the user funding the BDK address directly. The CLI currently has **no faucet-token support**.
Decision: it **should** accept `MUTINYNET_FAUCET_TOKEN` (or a `--faucet-token` flag) in a
CLI-only `faucet`/`fund-bdk` command, keeping token handling out of the core wallet. Not
implemented here.

---

## 28. Restore from mnemonic on a fresh data dir (live)

**Policy: the supported recovery artifact is "mnemonic + client data directory", not the mnemonic
alone.** A mnemonic-only restore is balances-only: no operation log/local history, no in-flight
resumption, and possible stranded deposits. See §31 for the app-level backup path.

### 28.1 Exact APIs (fedimint-client 0.12.0)

```rust
// ClientPreview, builder.rs:1519
pub async fn recover(
    self,
    db_no_decoders: Database,
    pre_root_secret: RootSecret,
    backup: Option<ClientBackup>,
) -> anyhow::Result<ClientHandle>;

// Client, client.rs:1966
pub async fn wait_for_all_recoveries(&self) -> anyhow::Result<()>;

// ClientHandle, client/handle.rs:57
pub async fn shutdown(self);

// ClientBuilder, builder.rs:483
pub async fn open(
    self,
    connectors: ConnectorRegistry,
    db_no_decoders: Database,
    pre_root_secret: RootSecret,
) -> anyhow::Result<ClientHandle>;
```

- https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientPreview.html#method.recover
- https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.wait_for_all_recoveries
- https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.ClientHandle.html#method.shutdown
- https://docs.rs/fedimint-client/0.12.0/fedimint_client/struct.Client.html#method.all_modules_usable

### 28.2 Is shutdown-before-reopen required?

**Yes for an in-process restore.** `all_modules_usable` docs state that a module held back during
recovery only becomes available once the client is **reopened**. `ClientBuilder::open` itself does
not check whether a previous handle is still alive, so this is not a compile-time constraint; it is
an operational one. `ClientHandle::shutdown(self)` is the documented explicit stop: it stops the
executor, joins the client task group (30 s bound), and drops the last `Client` handle. Before that
it is not safe to reopen the same RocksDB partition: background tasks and DB handles are still
live. On a fresh process, exiting and reopening has the same effect. Our `recover_client` always
does `recover → wait_for_all_recoveries → shutdown → open`.

### 28.3 Implemented flow

`client::recover_client(db, connectors, root_secret, invite)`:

1. Assert `Client::get_config_from_db(&db).await.is_none()` (fresh partition).
2. Build the same v2 module set (`mintv2`, `walletv2`), `preview(connectors, &invite)`.
3. Assert preview federation id == invite federation id.
4. `preview.recover(db.clone(), root_secret.clone(), None)` — `backup` is `None`; backups are
   deprecated in 0.12.0 and scheduled for removal in 0.13.0.
5. `handle.wait_for_all_recoveries().await`.
6. `handle.shutdown().await`.
7. `configured_builder().open(connectors, db, root_secret)`.
8. Assert the reopened federation id and `all_modules_usable()`.
9. `wait_for_all_active_state_machines()` so the recovered-notes issuance
   state machine settles before the caller reads the balance (§52).

`cli::restore` additionally:

- requires an existing `mnemonic` file (does not generate one);
- requires a fresh dir: no `bdk.sqlite` and no persisted fedimint config;
- derives `client_identity` from the same BIP39 root secret;
- optionally asserts caller-supplied expected federation id and identity;
- runs a **full** BDK scan, not partial sync.

### 28.4 Client identity definition (deviation)

fedimint 0.12.0 does not expose a public one-line "client identity" accessor.
`Client::get_internal_payment_markers()` is **federation-derived only** (fake LN pubkey from the
federation id), so it cannot prove the root secret. We define the printable identity as:

```text
SHA256(get_default_client_secret(global_root_secret, federation_id).to_random_bytes::<32>())
```

where `get_default_client_secret` is mirrored exactly:
`global.child_key(0).federation_key(fed).child_key(0).child_key(0)`. This is the same derivation
fedimint-client uses internally; only the SHA-256 digest is printed, never secret bytes. The
recovered identity matched the original.

### 28.5 BDK full scan (partial-sync gap)

```rust
Wallet::start_full_scan() -> FullScanRequestBuilder<KeychainKind>   // bdk_wallet 3.2.0
EsploraAsyncExt::full_scan(request, stop_gap, parallel_requests)
```

- https://docs.rs/bdk_wallet/3.2.0/bdk_wallet/struct.Wallet.html#method.start_full_scan
- https://docs.rs/bdk_esplora/0.22.1/bdk_esplora/trait.EsploraAsyncExt.html

`start_full_scan()` uses the wallet's unbounded script iterators, so a freshly created wallet
starts at derivation index 0. `sync()`/`start_sync_with_revealed_spks()` only covers already
revealed scripts; on a fresh BDK DB that set is empty, so historical addresses are missed. The
restore uses `stop_gap = 20`, `parallel_requests = 4`, and a 600 s timeout, then
`apply_update(response)` + `persist()`. **`stop_gap = 20` is a floor, not a guarantee:** a wallet
that revealed several unused addresses can have a larger gap and miss older funds. Consider
25–50 or make it configurable before shipping.

### 28.6 Live restore output

Original data dir `/tmp/opencode/berlin-join`; fresh dir `/tmp/opencode/berlin-restore` containing
only a copy of the mnemonic file. Original `join`:

```text
federation id: 05d83c302c5acff9c8e28d22cefa8b3eeaacbfddea0bd2d602e1972318013936
client identity: f306ff2e35e596e172b8c28393164e2a28619c6299096df37562e7dd7d161ace
```

Restore:

```text
restore federation id: 05d83c302c5acff9c8e28d22cefa8b3eeaacbfddea0bd2d602e1972318013936
restore client identity: f306ff2e35e596e172b8c28393164e2a28619c6299096df37562e7dd7d161ace
restore modules usable: true
restore chain-id network: signet
restore ecash balance: 36402688 msat (36402 sat)
restore bdk full scan last active: [(External, 11), (Internal, 2)]
restore bdk balance: { immature: 0 BTC, trusted_pending: 0 BTC, untrusted_pending: 0 BTC, confirmed: 0.00059473 BTC }
restore assertions: ok
```

Original balances were the same: ecash `36,402,688 msat`, BDK `0.00059473 BTC`. The full scan
found activity out to external index 11 / internal index 2 — addresses a fresh partial sync would
never have requested. A 2026-10-31 devimint rerun initially read `0 msat` immediately after
reopen; §52 added the active-state wait and re-verified the recovered balance. Unit coverage:
`seed::tests::client_identity_is_stable_for_the_same_seed`; the restore itself is an end-to-end
live test (it needs the federation).

### 28.7 Deviations and limits

- **Identity is our hash**, not a fedimint built-in; rationale in §28.4.
- **App journal is not restored.** Deposit/withdrawal rows and `custom_meta` history live in the
  local journal, not in the federation. A restored client recovers ecash and BDK funds, but
  application history starts empty; on-chain history is re-derived by the BDK full scan and
  federation operations by operation-log scanners. Withdrawal Send adoption covers a retained
  operation log; in a fresh 0.12.0 recover the operation log itself was empty (§29.4).
- **Post-restore UI:** empty deposit/withdrawal lists are **by design**, not an empty-activity
  state. Show the recovered balances plus a banner such as "History unavailable after restore —
  balances recovered". Old row ids are not recoverable from federation state.
- **Backups are not used** (`Option<ClientBackup> = None`); backup APIs are deprecated in 0.12.0.
- **Only registered modules recover.** `mintv2` and `walletv2` are registered; `lnv2`/`meta` are
  intentionally not, so they are skipped by recovery.
- **Fresh-dir enforcement** is in `recover_client` (config absent) and `cli::restore` (BDK file
  absent). A non-fresh dir must use normal `open`, not recover.

---

## 29. Withdrawal (Send) adoption on restore / journal loss

### 29.1 Invariant

**Every walletv2 `Send` operation in the local operation log is represented by exactly one
`withdrawal_requests` row.** Enforcement is the unique operation-id index:

```text
raw key 0x08 ++ operation_id(32) -> Envelope<WithdrawalId>
```

- `set_withdrawal_operation_id` writes the index immediately after `send()`.
- `insert_adopted_withdrawal` reads it first; if present, the existing row is returned and nothing
  is inserted.
- Dedup is on `operation_id`, not on `custom_meta.id`. A re-scan, or a different generated local
  id for the same Send, cannot duplicate the row.

### 29.2 Implementation

`withdrawal::scan_send_operations` pages the operation log and extracts:

```rust
WalletOperationMeta::Send(SendMeta { address, value, fee, custom_meta, .. })
```

`withdrawal::adopt_orphan_sends` maps the cached final state:

| Final state | Adopted row state |
|---|---|
| `Success(txid)` | `Broadcast { txid }` |
| `Aborted` | `Refunded` |
| `Failure` | `Failed` |
| not final yet | `Submitted` |

It builds `WithdrawalRow::adopted(...)` with `origin = Adopted`, fee from `SendMeta.fee`, value,
address (`assume_checked().to_string()`), `custom_meta`, and `operation_id`.

If `custom_meta.id` matches an existing `Quoting` row (crash after `send()` but before the
operation id was persisted), the existing row is **bound** to the Send and keeps
`origin = Requested`; no duplicate is inserted. A different `Send` with a colliding custom id gets
a fresh local id.

**Schedule:** `sync::adoption_task` calls it at boot and every `ADOPTION_INTERVAL` (60 s), next to
deposit orphan adoption. Adopted `Submitted` rows flow into the existing `reconcile_submitted`
loop; terminal rows do not occupy the single-flight slot.

### 29.3 Tests

- `withdrawal::tests::completed_send_is_adopted_exactly_once` — fresh in-memory journal, one
  completed Send (`Success(txid)`); first scan inserts exactly one `Adopted`/`Broadcast` row;
  re-scan with the same operation but a different `custom_meta.id` leaves the count at one.
- `withdrawal::tests::adopted_send_binds_an_existing_quoting_row` — an existing `Requested`
  `Quoting` row is bound to the Send, keeps `origin = Requested`, and no duplicate is created.
- `journal_db::tests::adopted_send_is_deduplicated_by_operation_id` — DB-backed journal enforces
  the 0x08 operation-id index.
- `journal_db::tests::adopted_send_binds_an_existing_requested_row` — the DB path binds a
  `Quoting` row without overwriting its `Requested` origin or duplicating it.

### 29.4 Live-restore limitation (important)

A truly fresh `recover` on fedimint-client 0.12.0 restored the ecash balance, but the local
**operation log was empty** (`operations` printed nothing). Therefore there were no Send ops to
adopt after that restore. The adoption pass is still correct and required for the case the client
DB retains Send ops while the application journal is lost/recreated (and for versions that restore
the operation log); it is not a substitute for the missing op-log history in a fresh 0.12.0
recover.

Also note: `stop_gap = 20` in the BDK restore is a **floor**. A wallet that revealed unused
addresses can have a larger gap and miss historical funds; consider 25–50 or make it configurable.
See §30 for the live in-flight deposit/withdrawal restore result: neither is resumed.

---

## 30. In-flight operations across a fresh restore (live experiment)

### 30.1 Setup

- Source client data copied from `/tmp/opencode/berlin-join` to
  `/tmp/opencode/berlin-inflight`. Baseline: ecash `36,402,688 msat`, BDK `0.00059473 BTC`.
- The fresh restore used `/tmp/opencode/berlin-inflight-restore` containing only the original
  mnemonic.

### 30.2 Deposit killed after funding broadcast

```text
deposit 01a14ceb-a66b-716d-82e5-313985b40d26
address tb1q44wuvcjuy6s5fkyaljjxshgnmgt04s0dxuzptncxfye3h6khz3yqpeejfu
funding tx 1922343bbf09515b698f3441ed1eb4971d24b728e613c1d03c3c7bde07d2a5e1 (vout 0, 10000 sat)
BERLIN_WALLET_CRASH_POINT=after_deposit_broadcast -> exit 134
```

The funding transaction was confirmed (block `3514519`). After the fresh restore, the deposit
address UTXO was **still unspent** and the restored ecash balance was unchanged at
`36,402,688 msat`. The deposit was **not resumed and not credited**.

### 30.3 Withdrawal killed after `send()` before completion

```text
withdraw tb1qav6qxadyxh3pcndq788fzk79njhwvc3eycmgkf 10000
BERLIN_WALLET_CRASH_POINT=after_send -> exit 134
```

No new transaction appeared at the destination address after >2 minutes. The fresh restore
reported ecash `36,402,688 msat` — unchanged. There was no resumption and no on-chain withdrawal.

### 30.4 Fresh restore result

```text
restore federation id: 05d83c302c5acff9c8e28d22cefa8b3eeaacbfddea0bd2d602e1972318013936
restore client identity: f306ff2e35e596e172b8c28393164e2a28619c6299096df37562e7dd7d161ace
restore modules usable: true
restore ecash balance: 36402688 msat (36402 sat)
restore bdk full scan last active: [(External, 11), (Internal, 3)]
restore bdk balance: { immature: 0 BTC, trusted_pending: 0 BTC, untrusted_pending: 0 BTC, confirmed: 0.00049252 BTC }
restore assertions: ok
```

BDK dropped from `0.00059473` to `0.00049252 BTC` — exactly the 10,000 sat deposit plus the 221 sat
funding fee. The deposit is on-chain at the federation deposit address, but the fresh client has no
operation/row to claim it.

`operations`, `list-withdrawals`, and the fresh journal were empty:

```text
berlin-wallet> berlin-wallet> berlin-wallet> 36402688 msat (36402 sat)
```

### 30.5 Conclusion

**fedimint-client 0.12.0 does not resume in-flight operations on a fresh recover.**

- Deposit funded but not claimed → funding is unspent at the federation address; fresh restore
  does not claim or credit it. Recovering it requires the retained client DB (operation log/module
  state) or a supported history mechanism.
- Withdrawal sent but not completed → no on-chain tx in this test; ecash was unchanged after
  restore. If a withdrawal had already broadcast, recovery would restore the reduced balance but
  still have no local row/history (operation log empty).
- Therefore in-flight funds are **not guaranteed safe across device loss** in 0.12.0. The app must
  either keep the client DB safe (device backup) or prevent/avoid device loss between `send()` and
  settlement; a mnemonic-only restore is insufficient. Requesting a new deposit did not re-claim
  the stranded UTXO (§30.7).

### 30.6 Questions for the fedimint dev

1. Does 0.12.0 recovery intend to resume in-flight operations, or only rebuild settled module
   state? Our live test says it does not resume either a funded-unclaimed deposit or a
   sent-incomplete withdrawal.
2. Is the operation log supposed to be restored? It is empty after a fresh recover, which removes
   all local history and makes orphan adoption impossible unless the client DB is retained.
3. Is there a supported way to recover history now that backup APIs are deprecated for removal in
   0.13.0? If not, what is the recommended app-level strategy for retaining the operation log and
   journal across device loss?

### 30.7 Re-claim attempt (new deposit after restore)

On the fresh-restore data dir, with the 10,000 sat stranded UTXO still unspent at
`tb1q44wuvcjuy6s5fkyaljjxshgnmgt04s0dxuzptncxfye3h6khz3yqpeejfu`:

```text
ecash before: 36402688 msat
request-deposit 10000
  deposit 01a14cf3-f75c-7675-ae0e-b0384e27edf3
  state=AddressShown
  address=tb1qen7rhvlway2w3q7w6ktykd5g3wz5780jkp5f67c4zkqfdw6wjj3snydtvm
```

- `receive()` returned a **different** address (`tb1qen7…`), not the stranded
  `tb1q44…` address.
- After >100 s of a live client (the walletv2 output scanner runs every 60 s),
  ecash was still `36,402,688 msat`; the deposit row remained `AddressShown`,
  `funding_txid=None`, `net_credit=None`.
- The stranded UTXO remained unspent.

**Conclusion:** simply requesting a new deposit does **not** re-derive the old tweak index or
re-claim the stranded UTXO in 0.12.0.

2026-10-31 devimint rerun: the new request returned the **same** walletv2
address as the stranded 50,000 sat UTXO (walletv2 address reuse), unlike the
signet run above. Even then, after ten more blocks the ecash balance was
unchanged, the new row remained `AddressShown`, and the stranded UTXO was still
unspent (11 confirmations). Address equality is not a recovery signal; the new
receive did not re-claim the outpoint.

### 30.8 What the 0.12.0 walletv2 API allows

Public:

```rust
WalletClientModule::receive() -> Address
WalletClientModule::await_receive(position) -> Result<(FinalReceiveOperationState, EventLogId)>
```

Internal/private:

```rust
derive_address(index)          // private
derive_tweak(index)            // private
valid_index()                  // private
next_valid_index(start, handle) // private
receive_output(...)            // private
spawn_output_scanner(...)      // private
```

There is no public method to request a specific tweak index, force a rescan from index 0, enumerate
valid indices, or claim a specific outpoint. The scanner owns an internal `ValidAddressIndexPrefix`
set and `NextOutputIndexKey`; `receive()` only returns the highest tracked valid index. An app can
reimplement the derivation from root secret + `WalletClientConfig` (public `descriptor` /
`is_potential_receive`) and could theoretically manipulate the module DB keys, but that is internal
DB surgery and constructing the claim still requires the private receive state machine. It is not a
supported recovery path.

**UX requirement:** after restore, warn that an earlier in-flight deposit may be stranded and must
be re-claimed; do not discard the original client data, and do not imply that requesting a new
deposit will recover it.

### 30.9 Additional fedimint-dev questions

**Bug report first:** 0.12.0 has no public "rescan deposits from index 0"; after mnemonic-only
restore the scanner's tracked set starts empty, so older funded deposit indices are unreachable.

1. Is a public rescan/re-claim path planned?
2. Is there a supported deposit re-claim/scan path after mnemonic-only restore, or must an app
   re-derive tweak indices itself?
3. Is the deposit-address derivation index recoverable from federation state (or only from local
   client DB state)?
4. Given backups are deprecated, what is the recommended app-level strategy for retaining the
   operation log/journal? Should the app export/import its own journal?

---

## 31. Recovery policy and app-level backup (0.12.0)

### 31.1 Policy

**The recovery artifact is "mnemonic + client data directory", not the mnemonic alone.**

A mnemonic-only restore:

- recovers settled module balances;
- leaves the local operation log empty, so deposit/withdrawal history is empty by design;
- does not resume in-flight operations;
- can strand an in-flight deposit at a federation deposit address (it is not re-claimed by a new
  `receive()`);
- is not sufficient for an in-flight withdrawal.

The UI must make this prominent, not a footnote.

### 31.2 App-level backup decision

Decision: **age-encrypted archive** (see §56). Fedimint backup APIs are deprecated; the app owns
the artifact. The CLI implements:

```text
berlin-wallet backup <destination-file>
berlin-wallet restore-backup <source-file>
```

- `backup` packages the **entire data directory tree**, not a file list, into a tar inside an age
  passphrase-encrypted file, so SQLite `bdk.sqlite-wal` / `-shm` and all `fedimint.rocksdb`
  files/sidecars are included. It refuses an existing destination and must run with the app stopped.
- **Lock check:** before reading, `backup` takes a non-blocking try-lock on the same advisory lock
  fedimint-rocksdb uses (`source/fedimint.rocksdb`.with_extension(`.db.lock`) →
  `source/fedimint.db.lock`). If held, it exits nonzero:
  `database is locked (...); stop the wallet before running backup`.
- `restore-backup` requires an empty data dir and decrypts/validates the archive before renaming the
  tree into place.
- **Encryption:** age passphrase encryption provides an authenticated, versioned container; the
  backup passphrase is separate from the wallet passphrase. The archive is key material.
- After import, start normally with `run` and the wallet passphrase; **do not call `restore`**. The
  retained client DB preserves the walletv2 scanner/index state and the operation log.
- **A journal-only export is not a recovery backup** and does not fix stranded deposits. The
  scanner/address-index state lives in the client DB.

Verified manually: whole-directory backup/import of the live data dir reopened with the same
federation id, client identity, primary module, and minimum withdrawal. CLI unit tests:
`data_dir_backup_round_trips_bdk_state_and_sidecars` (opens real BDK SQLite before/after, asserts
balance and next address match, and asserts WAL/SHM sidecars were copied) and
`backup_refuses_a_locked_database` (held lock → loud failure).

### 31.3 Re-claim limitation (precise)

`WalletClientModule::receive()` returns only the **highest scanner-tracked valid index**. After a
fresh restore the scanner's tracked set starts empty; it derives a new first valid index rather than
the previous set. A previously funded UTXO at an older tweak index is therefore unreachable via the
public API. A manual re-derivation plus claim would need the private `derive_tweak`,
`receive_output`, and receive state machine (or a walletv2 patch). There is no public "rescan
deposits from index 0" in 0.12.0.

**Candidate upstream fix / bug report:** expose a supported rescan/re-claim path that walks tweak
indices from 0 (or a known checkpoint), bounded by the federation's output history, so
mnemonic-only restore can reclaim stranded deposits.

### 31.4 Fedimint-dev questions

Lead with the bug report: **walletv2 0.12.0 has no public rescan-deposits-from-index-0; a
mnemonic-only restore leaves older funded deposit indices unreachable.**

1. Is a public rescan/re-claim path planned? If not, is the intended recovery artifact always the
   client data directory?
2. Is there a supported deposit re-claim/scan path after mnemonic-only restore, or must an app
   re-derive tweak indices itself?
3. Is the deposit-address derivation index recoverable from federation state, or only local client
   DB state?
4. Given backups are deprecated, what is the recommended app-level strategy for retaining the
   operation log/journal? Should the app export/import its own full data directory?

---

## 32. Reorg handling decision

### 32.1 Source findings

**Deposits — walletv2 server/client:**
- `CONFIRMATION_FINALITY_DELAY = 6` (`walletv2-server/src/lib.rs:95`). Block-count votes are
  `tip − 6`; `process_block_count` requires a strictly higher per-peer vote and asserts the
  consensus count never decreases (`old <= new`).
- Potential receive outputs are appended to an append-only `OutputPrefix` log while scanning those
  finalized blocks. There is no rollback path for a reorg that removes/renumbers a block.
- `ReceiveStateMachine` only awaits `await_tx_accepted(txid)` and then enters `Success`
  (`walletv2-client/src/receive_sm.rs`). It does **not** track Bitcoin confirmations or unconfirm.
- The server keeps `UnconfirmedTx` entries and rebroadcasts them in the background. So a claim tx
  can be accepted and later reorged out of the active chain; it is rebroadcast, not reflected as a
  client state transition.
- A reorg deeper than the 6-block finality assumption is outside 0.12.0's server rollback model;
  the append-only output log and monotonic block count cannot repair it.

**Withdrawals — walletv2 `SendStateMachine`:**
- `await_funding` waits for `await_tx_accepted(outpoint.txid)` then calls the federation `tx_id`
  API; that becomes `FinalSendOperationState::Success(txid)`.
- This is **broadcast/acceptance-only**, not confirmation. No confirmation height or reorg watch is
  represented. The recipient chain/BDK owns confirmation tracking; the federation rebroadcasts
  unconfirmed txs.

**BDK:**
- `Wallet::apply_update` / `LocalChain::apply_update` uses `merge_chains`
  (`bdk_chain-0.23.3/src/local_chain.rs:560`). On a competing checkpoint it invalidates blocks above
  the point of agreement and updates the tip; `TxGraph` keeps the tx and updates its anchor. The
  on-chain wallet self-corrects its confirmed/unconfirmed balance.

### 32.2 States required

**None.** There are no `Reorged` variants in `DepositState` or `WithdrawalState`, and none are
required for the 0.12.0 API/behaviour above. No speculative state was added or removed. The
existing `Done`/`Broadcast` states represent federation-accepted settlement/broadcast, not Bitcoin
finality.

### 32.3 What is testable without a controlled chain

- Esplora (`bdk_esplora 0.22.1`) is read-only: no `invalidateblock`, rewind, or chain-reorg API.
  A deterministic reorg cannot be produced against Mutinynet/Esplora.
- There is no client reorg transition to unit-test. The existing state tests already cover the
  terminal `Done`/`Broadcast` paths.
- A local BDK checkpoint invalidate round-trip would require constructing synthetic `CheckPoint` /
  `TxUpdate` anchors; not attempted here because the app has no reorg state and the library
  `merge_chains` path is already source-verified. (The esplora crate cannot drive it.)

### 32.4 Deterministic devimint test (needed)

A devimint/regtest test with bitcoind `invalidateblock` + `generatetoaddress` should cover:

1. Fund a deposit, mine past the 6-block finality window, then invalidate the deposit/claim block
   and mine an alternate chain. Assert how walletv2's monotonic block count and append-only output
   log behave, whether the claim is re-observed/re-broadcast, and whether the ecash balance is
   credited or remains/returns.
2. Broadcast a withdrawal, mine it, invalidate its block, and mine an alternate chain. Assert the
   federation rebroadcasts the `UnconfirmedTx`, the client stays `Broadcast { txid }`, and BDK
   updates the destination/confirmation state.
3. Reorg through BDK's own sync path (checkpoint rollback) and assert confirmed/unconfirmed balance
   self-correction.

This is the only way to test the >6-conf finality assumption; it cannot be reproduced with the
read-only Esplora client.

---

## 33. UniFFI crate (`berlin-wallet-ffi`)

### 33.1 Crate and namespace decision

- `crates/berlin-wallet-ffi`, `crate-type = ["cdylib", "staticlib", "lib"]`.
- `uniffi = "=0.32.0"`, feature `tokio`.
- `uniffi::setup_scaffolding!()` — exactly one namespace (`berlin_wallet_ffi`).
- **Fedimint's own `uniffi` feature is not enabled.** The object stores `ClientHandleArc` directly
  and uses `Deref<Target = Client>` plus `get_first_module_arc`/`get_first_module`.
  `ClientHandle::inner_arc()` is `#[cfg(feature = "uniffi")]` only and is not needed. This avoids
  two UniFFI namespaces and the fedimint-client uniffi dependency.
- `#[derive(uniffi::Object)] PegWallet`; `#[uniffi::export(async_runtime = "tokio")]` impl.
- `request_deposit` persists `AwaitingAddress` and `tokio::spawn`s `run_address_task`, then returns;
  the address is observed via `deposit_status`. No synchronous FFI call blocks on network or
  address derivation. `backup`/`restore_backup` use `spawn_blocking`.

### 33.2 Surface

```rust
PegWallet::new(data_dir: String) -> Arc<PegWallet>;

init(network: String, passphrase: String) -> Result<InitView>; // mnemonic only, local
open(chain_source: Option<String>, passphrase: String) -> Result<WalletInfo>; // existing DB
join(invite_code: String, chain_source: String, passphrase: String) -> Result<WalletInfo>;
restore(invite_code: String, chain_source: String, network: String, passphrase: String)
    -> Result<RestoreView>;                                    // fresh dir
restore_backup(source_file: String, invite_code: String, chain_source: Option<String>,
               backup_passphrase: String, wallet_passphrase: String) -> Result<WalletInfo>;
backup(destination_file: String, backup_passphrase: String) -> Result<()>; // async
shutdown() -> Result<()>;                                     // stop tasks/client, release locks

request_deposit(amount_sat: u64) -> Result<DepositView>;
fund_deposit(deposit_id: String) -> Result<DepositView>;
deposit_status(deposit_id: String) -> Result<DepositView>;
cancel_deposit(deposit_id: String) -> Result<DepositView>;
list_deposits() -> Result<Vec<DepositView>>;

request_withdrawal(address: String, amount_sat: u64) -> Result<WithdrawalView>; // quoted, Quoting
send_withdrawal(withdrawal_id: String) -> Result<WithdrawalView>; // re-quote + drift-check
withdrawal_status(withdrawal_id: String) -> Result<WithdrawalView>;
cancel_withdrawal(withdrawal_id: String) -> Result<WithdrawalView>;
list_withdrawals() -> Result<Vec<WithdrawalView>>;

balance_sat() -> Result<u64>;            // BDK trusted_spendable
ecash_balance_msat() -> Result<u64>;     // mintv2 primary balance
min_withdrawal_sat() -> Result<u64>;
```

Records: `InitView`, `WalletInfo`, `DepositView`, `WithdrawalView`, `RestoreView`.
Enums: `DepositOriginView`, `DepositStateView`, `WithdrawalOriginView`,
`WithdrawalStateView`.
One error enum: `PegWalletError::Message { message }` (converts `anyhow`/I/O; no `anyhow` across
the boundary).

`DepositView.net_credit_msat: Option<u64>`; `None` means pending and the UI must render
**"pending"**, never 0. `DepositView` also carries `origin`, state, amount, gross, claim fee,
address, funding txid, operation id, and timestamps.

`RestoreView` includes balances, min withdrawal, `operation_log_empty`,
`history_unavailable`, journal counts, and BDK last-active external/internal indices.

`WithdrawalView` quote fields:

```text
quoted_fee_sat       on-chain send_fee at quote time
quote_input_msat     FeeQuote.input BTC amount
quote_output_msat    FeeQuote.output BTC amount
quote_dust_msat      FeeQuote.dust BTC amount
total_debit_msat      recipient_amount + quoted_fee + federation quote fees (TOTAL DEBIT, not net)
recipient_amount_sat  amount the recipient gets (withdrawal value)
```

`request_withdrawal` quotes and persists while `Quoting`; the UI must display these and require
explicit confirmation before `send_withdrawal`. `total_debit_msat` is the total debit, not a net
amount; fees Y are `quoted_fee_sat` plus quote components (equivalently
`total_debit_msat − recipient_amount_sat`). `send_withdrawal` re-quotes immediately before send and
compares to the persisted quote. On drift it persists the new quote and returns an error; the UI
must re-display and re-confirm.

### 33.2.1 Open/shutdown contract

- `init(network, passphrase)` only creates/loads the age-encrypted mnemonic.
- `open(chain_source, passphrase)` is the no-invite path for an existing client DB
  (`ClientBuilder::open` via `open_existing_client`); use it on app restart.
- `join(invite, chain_source, passphrase)` is fresh-join only and errors if a client config already
  exists.
- `restore` is mnemonic-only fresh recovery; `restore_backup` decrypts/imports the age archive and
  opens the retained client DB (verifying the invite's federation id).
- `shutdown()` is **terminal for that instance**: it sets a closed flag, takes the state, sends the
  watch shutdown signal, waits up to 5 s per task and aborts stragglers, then consumes `ClientHandle`
  via `ClientHandle::shutdown` to stop the executor/task group. Every other method after shutdown
  returns an error (no panic); construct a new `PegWallet` and call `open()` to resume. Mobile MUST
  call it on backgrounding; otherwise the DB stays locked and `backup` fails with
  `database is locked`.

### 33.3 Smoke test

`scripts/ffi-smoke.sh`:

1. `cargo build -p berlin-wallet-ffi`;
2. generates Kotlin bindings from `target/debug/libberlin_wallet_ffi.so` with the crate's
   `uniffi-bindgen` bin (`--features uniffi-bindgen`);
3. checks the generated `PegWallet` class, `open`/`shutdown`, and
   `netCreditMsat`/`requestDeposit`/`quotedFeeSat`/`totalDebitMsat`/`recipientAmountSat` surface.

Verified: `ffi smoke ok: cdylib + kotlin bindings (PegWallet/DepositView/WithdrawalView surface present)`.
Generated Kotlin methods exactly: `backup`, `balanceSat`, `cancelDeposit`, `cancelWithdrawal`,
`depositStatus`, `ecashBalanceMsat`, `fundDeposit`, `init`, `join`, `listDeposits`,
`listWithdrawals`, `minWithdrawalSat`, `open`, `requestDeposit`, `requestWithdrawal`, `restore`,
`restoreBackup`, `sendWithdrawal`, `shutdown`, `withdrawalStatus`.

`cargo tree -p berlin-wallet-ffi -e features` shows only the top-level `uniffi` crate/features; no
fedimint-client `uniffi` feature is enabled.

### 33.4 Tests

- `cargo check --workspace`: exit 0.
- `cargo test --workspace`: 37 core + 4 CLI + 4 FFI unit tests, exit 0.
- FFI smoke script: exit 0.

---

## 34. BDK sync: height-decreasing reorg and transient errors

### 34.1 Exact no-op mechanism in the pinned BDK

Source verified against `Cargo.lock` pins:

- `bdk_wallet 3.2.0`, `bdk_chain 0.23.3`, `bdk_esplora 0.22.1`, `esplora-client 0.12.3`.
- `Wallet::latest_checkpoint()` returns `self.chain.tip()` (`bdk_wallet-3.2.0/src/wallet/mod.rs:874`).
- `Wallet::apply_update()` calls `LocalChain::apply_update` (`wallet/mod.rs:2459`;
  `bdk_chain-0.23.3/src/local_chain.rs:174`).
- `bdk_esplora::chain_update()` (`async_ext.rs:227`) walks the **local** checkpoint from the tip down
  and calls `fetch_block()` (`async_ext.rs:197`). `fetch_block` returns `None` for a requested height
  above the latest remote tip (`async_ext.rs:206-216`), so when the remote tip is lower, the update
  checkpoint's highest block is the remote tip.
- `LocalChain::merge_chains()` (`local_chain.rs:560`) collects original heights above the update
  into `potentially_invalidated_heights` (`:608`) but only removes them in the **hash-mismatch**
  branch (`:650-657`). When the remote tip is lower and all overlapping hashes agree, it finds a
  point of agreement at the lower tip and returns an **empty changeset**. The local tip above the
  remote tip stays, and its anchors remain confirmed.

This is reproduced in `bdk::tests::lower_tip_update_is_a_noop_in_bdk`: local chain 0..5, update
checkpoint 0..2 with matching hashes → `changeset.blocks.is_empty()` and local tip remains height 5.

### 34.2 Fix

`BdkWallet::sync` (`bdk.rs:291`) now:

1. Reads `wallet.latest_checkpoint()`.
2. Fetches `/blocks/tip/height` and `/blocks/tip/hash` via
   `esplora_client::AsyncClient::get_height()`/`get_tip_hash()`.
3. If `remote_height >= local_height`, fetches `/block-height/{local_height}` via
   `get_block_hash(local_height)` and compares it to the stored tip hash.
4. `reorg_decision(local_height, local_hash, remote_height, remote_at_local)` (`bdk.rs:85`) returns
   `Orphaned` if the remote tip is lower or the hash at the local height differs.
5. If orphaned, **does not run the incremental update**. It calls
   `reset_and_full_scan()` (`bdk.rs:368`): builds a fresh BDK DB from the same seed/descriptors at a
   temporary path, full-scans, then hands it to `swap_in_scanned_db()` (`bdk.rs:398`).
6. The handoff checkpoints both SQLite DBs, **closes the temp connection**, drops the old
   connection, atomically renames the scanned temp file over the final path, and **reopens a new
   `Connection`/`PersistedWallet` against the final path**. The old connection must not be kept: it
   still refers to the unlinked old inode and would return SQLite error 1032 on the next write.
7. The reopened wallet's next derivation index is raised to `max(rebuilt, previous)` for both
   external and internal keychains (using `next_derivation_index` and `reveal_addresses_to`), so a
   revealed-but-unused address is never handed out twice.
8. If ancestry is valid, runs the normal partial `bdk_esplora::sync` and persists.

The temp DB is fully scanned and checkpointed before the atomic rename. A crash before the rename
leaves the old DB intact; a crash after it leaves the new, valid DB. No on-disk format or journal
changes. `BdkWallet` stores its path/seed/network solely to rebuild the same descriptors.

### 34.3 Transient error handling

`sync` (`bdk.rs:291`) retries **3 attempts** with 250 ms exponential backoff for:

- `esplora_client::Error::Reqwest` where `is_connect() || is_timeout() || is_request()` (connection
  refused, DNS/request failures, timeouts);
- our own `tokio::time::timeout` expiry.

Malformed/parsing/encoding errors and `HttpResponse` 4xx are returned immediately as normal
surfaced errors (`is_transient_esplora`, `bdk.rs:40`; tests `http_4xx_is_a_permanent_sync_error`,
`malformed_body_is_a_permanent_sync_error`). After 3 failed attempts the last error is returned.

The CLI `sync` arm (`main.rs:292`) no longer uses `?` outside the REPL command error handling; it
returns the error to the loop, which prints it and returns to the prompt. Background
`bdk_chain_task` already catches/logs and retries on its next tick.

### 34.4 Tests

- `bdk::tests::ancestry_decision_accepts_same_hash`
- `bdk::tests::ancestry_decision_rejects_lower_remote_tip`
- `bdk::tests::ancestry_decision_rejects_conflicting_hash`
- `bdk::tests::lower_tip_update_is_a_noop_in_bdk`
- `bdk::tests::http_4xx_is_a_permanent_sync_error`
- `bdk::tests::malformed_body_is_a_permanent_sync_error`
- `bdk::tests::swap_reopens_connection_and_preserves_next_index` (unit regression: open DB, swap
  in a scanned temp DB, write on the same `BdkWallet`; asserts no SQLite 1032 and next index does
  not regress)

`cargo check --workspace`: exit 0. `cargo test --workspace`: 37 core + 4 CLI + 4 FFI tests pass.

---

## 35. Reorg safety — BDK does not reconcile a height-decreasing reorg

Status: upstream finding; worked around in this repo (b91ebca, 9eebd19).
Verified live 2026-10-19.

### Symptom
After a reorg that *shortens* the chain, the wallet keeps anchors to orphaned
blocks and reports the orphaned tx inside `confirmed`. The confirmed balance is
inflated by the full orphaned amount and stays stale indefinitely.

### Repro (devimint, regtest)

```text
address                      # note the address
devimint

btc sendtoaddress <addr> 0.001
btc generatetoaddress 1 <mining_address>
H=$(btc getblockcount)
REPL

sync ; balance              # confirmed +0.001

devimint

btc invalidateblock $(btc getblockhash $H)
REPL

sync ; balance              # STALE: still inside confirmed
```

### Evidence
- `btc getrawmempool` — tx back in the mempool
- `btc gettransaction <txid>` — `confirmations: 0`
- esplora `GET /tx/<txid>` — `"status":{"confirmed":false}`
- wallet `balance` — tx still counted in `confirmed`
- Ruled out mempool fallback: RBF-replacing the tx so it is in neither the chain
  nor the mempool leaves the balance unchanged.
- Ruled out "just needs a nudge": mining one block at the *same* height as the
  orphaned block leaves it stale; only when the tip advances past the stored
  checkpoint height does it self-correct.

### Mechanism (verified against pinned sources, not docs)
- `bdk_wallet 3.2.0`, `bdk_chain 0.23.3`, `bdk_esplora 0.22.1`, `esplora-client 0.12.3`
- `Wallet::latest_checkpoint()` -> `self.chain.tip()` — bdk_wallet/src/wallet/mod.rs:874
- `Wallet::apply_update()` -> `LocalChain::apply_update` — wallet/mod.rs:2459,
  bdk_chain/src/local_chain.rs:174
- `bdk_esplora::chain_update` — bdk_esplora/src/async_ext.rs:227; `fetch_block`
  returns `None` above the remote tip (:197, :206-216), so the update's
  `CheckPoint` cannot contain a block at the local tip height when the remote tip
  is lower.
- `LocalChain::merge_chains` — local_chain.rs:560 — collects
  `potentially_invalidated_heights` (:608) but purges them only in the
  hash-mismatch branch (:650-657). With a lower remote tip and agreeing overlap
  it reaches agreement and returns an empty changeset; local blocks above the
  remote tip survive with their anchors.
- Unit proof: `bdk::tests::lower_tip_update_is_a_noop_in_bdk` — local chain 0..5
  plus an update for 0..2 with matching hashes yields an empty changeset and
  leaves the local tip at 5.

### Impact
Any balance or funding decision in the window after a reorg uses coins that no
longer exist. This contradicts the earlier PLAN/COMPILE claim that "reorg is
handled by walletv2/BDK, no journal state needed".

### Fix (ours, in the wallet)
`BdkWallet::sync` verifies ancestry before trusting the incremental update: read
the stored checkpoint tip, fetch `/blocks/tip/hash` and `/blocks/tip/height`, and
if the stored tip is not an ancestor of the remote tip, rebuild the wallet DB and
full-scan. Detects both a lower remote tip and a hash mismatch at equal height.
`reorg_decision()` is unit-tested for all three cases.

DB swap hand-off: the scanned temp DB is checkpointed, the old connection
dropped, the file renamed, and a new connection opened against the final path —
otherwise the first write fails with `SQLITE_READONLY_DBMOVED` (1032). Keychain
next-index is raised to `max(rebuilt, previous)` so a revealed-but-unused address
is never handed out twice.

### Verification
Live: fund, mine, `sync`, `invalidateblock`, `sync` -> balance corrects
immediately with no tip advance and no 1032; `address` returns a fresh value.
Rescan is announced at INFO.

## 36. devimint's esplora panics on a live reorg

Status: upstream finding; worked around by manual restart.

`electrs-esplora 0.4.1`, `src/new_index/schema.rs:298`: thread 'main' panicked at src/new_index/schema.rs:298:9:
assertion left == right failed
left: 0x066563c1...
right: 0x76baa724... It asserts the block hash at an already-indexed height instead of rolling back.
Dies on every `btc invalidateblock` while running. Reconciles fine if it discovers
the reorg at boot — the assert is only in the live update path.

Impact: `invalidateblock` is the standard reorg-testing tool and devimint's own
esplora cannot survive it, so every reorg test needs a manual restart.

Restart recipe in §40: `--jsonrpc-import`; `--daemon-dir` and `--blocks-dir` must
include the `regtest` component; `--cookie user:pass` (not `--daemon-auth`).

## 37. walletv2 change_outpoint_range is federated ecash change (corrected)

Status: resolved; the earlier "phantom Bitcoin txid" interpretation was a
category error.

`SendMeta`/`ReceiveMeta::change_outpoint_range` is a `fedimint_core::OutPointRange`
(`fedimint-core/src/lib.rs:474-550`):

```rust
pub struct OutPointRange {
    pub txid: TransactionId, // fedimint_core::TransactionId (SHA-256), not bitcoin::Txid
    idx_range: IdxRange,
}
```

Its iterator yields `fedimint_core::OutPoint` values. The matching wait,
`Client::await_primary_bitcoin_module_output(operation_id, out_point)`
(`fedimint-client/src/client.rs:1253`) waits for the primary Bitcoin module's
**ecash output**, not a Bitcoin transaction confirmation.

Therefore these txids are not expected in the Bitcoin mempool or blocks. The
range indexes the mintv2 ecash outputs created as change by the federation
transaction. The on-chain withdrawal txid is a different value.

Live 2026-10-31 withdrawal: the `Send` meta range was
`txid=8f5d99cba150aac1c2c10805eede233c1a8f345408a21357bb480821213caea1`,
indices 1..5; the actual on-chain withdrawal txid was
`14c96662e8bf72c7df18264f9a3c9cc1a991182fd00d585b8af95b929c83c4eb`.
That difference is expected, not a phantom transaction.

The `await-change` wait is correct for its purpose (ensuring the ecash change
outputs are journaled before `Broadcast`). It is not an on-chain verification
step, and no downgrade is needed. The test
`withdrawal::tests::change_outpoint_range_is_federated_ecash_change` pins the
type relationship. Source links:
<https://github.com/fedimint/fedimint/blob/v0.12.0/fedimint-core/src/lib.rs#L474-L550>
and
<https://github.com/fedimint/fedimint/blob/v0.12.0/fedimint-client/src/client.rs#L1253>.

## 38. Withdrawal quote vs charged fee mismatch (resolved)

Root cause: `WalletClientModule::send_fee_quote(amount)` expects the **wallet
output amount including the on-chain miner fee**, because `send` creates a
wallet output worth `value + fee` (`fedimint-walletv2-client/src/lib.rs:270-297,
399-405`). The withdrawal code quoted only `value`, so the wallet-output fee and
the mintv2 change/dust were under-estimated.

Fix: both `request_withdrawal` and the pre-send re-quote use
`funded_withdrawal_amount(value, chain_fee) = value + chain_fee`. Test:
`withdrawal::tests::quote_uses_the_wallet_output_amount_including_the_chain_fee`.

Live 2026-10-31 (devimint bitcoind backend):

- value `50,000 sat`, on-chain `send_fee` `228 sat`;
- federation `FeeQuote`: input `900 msat`, output `100,400 msat`, dust `300 msat`
  = `101,600 msat`;
- quoted total debit: `50,000,000 + 228,000 + 101,600 = 50,329,600 msat`;
- ecash balance before/after: `556,707,840 -> 506,378,240 msat`, delta
  `50,329,600 msat`;
- `operation-fees` for the same operation returned `101,600 msat`.

The quote now reconstructs the observed debit exactly. The prior 224 msat
mismatch was this missing gross-up.

## 39. Observability gaps

- `watch` polled silently for its whole duration. FIXED (§54): the CLI prints an
  initial line and a 10 s stderr heartbeat with state, event-log positions,
  funding txid, operation id, and elapsed time. stdout remains the final row.
- Shutdown task-cancellation noise (`fm::task` WARN/ERROR) is suppressed by a
  reloadable filter only for the shutdown phase. FIXED (§55).
- `sync` treated a connection refusal as fatal and killed the REPL. FIXED
  (b91ebca): connect/timeout/request errors retry with bounded backoff (3 attempts,
  250/500 ms), parse/4xx are permanent; the CLI returns to the prompt.
- The CLI installed no tracing subscriber, so every `tracing::` line in core was
  silently dropped — including the adoption-pass warnings. FIXED (890a24d):
  subscriber on stderr, `RUST_LOG`-controlled, `try_init()` so it is idempotent;
  FFI deliberately does not install one so the host owns logging.
- Cosmetic: the REPL prompt was on stdout while logs go to stderr, so log records
  landed mid-prompt and needed an Enter to recover. FIXED (eba79a6): prompt now on
  stderr, so prompts and logs share one channel.
- The mintv2 operation fee (~0.106% on top of the claim fee; ~0.42% round trip at
  100k sat) is not previewed at `request-deposit`. It is accounted for in
  `net_credit`, but a user should see it before committing.

## 40. Build / environment notes (Fedora 44)

- **Preferred path:** run `nix develop` from the repository root. The checked-in
  `flake.nix` devShell supplies Rust 1.91.1 via rust-overlay (with `rustfmt` and
  `clippy`), the native dependencies, `LIBCLANG_PATH`, and a
  `BINDGEN_EXTRA_CLANG_ARGS` derived with `clang -print-resource-dir`. It is a
  devShell only, not a package build. Run cargo and the resulting binary inside
  that shell.
- The manual derivation below is for non-Nix users.
- The toolchain is pinned to `1.91.1` in `rust-toolchain.toml`; the default
  1.89 stable cannot build the dependency graph (`fedimint-connectors` → iroh
  requires Rust 1.91).
- `BINDGEN_EXTRA_CLANG_ARGS` must include clang's own resource dir
  (`-isystem /usr/lib/clang/22/include` on the current Fedora 44 toolchain) plus
  the standard search paths from `clang -E -v`. `22` is the current clang major
  version; re-derive it on any clang upgrade — don't copy it.
- `LIBCLANG_PATH=/usr/lib64`. Set both variables in the shell; they are
  intentionally not in `.cargo/config.toml`, because they are machine-specific.
- `aws-lc-sys` 0.41.0 and `librocksdb-sys` need a C++17 compiler; `gcc-c++` and
  `clang` are required on Fedora.
- `~/.local/bin/cmake` (4.4.4) shadows `/usr/bin/cmake`; both work, but note it.
- devimint esplora restart (see §36):

```text
nohup esplora --network regtest
--daemon-dir "$FM_BTC_DIR/regtest"
--daemon-rpc-addr 127.0.0.1:10016
--cookie bitcoin:bitcoin
--jsonrpc-import
--db-dir /tmp/esplora-rr
--http-addr 127.0.0.1:10025
--monitoring-addr 127.0.0.1:10026
--timestamp -v \

    /tmp/esplora-rr.log 2>&1 &
```

The `blk*.dat` path fails even with the correct `regtest` subdirectory;
`--jsonrpc-import` works.

## 41. Known limitation — restore strands in-flight operations

Restore recovers client identity and balances exactly, but 0.12.0 does not resume
in-flight operations and there is no public re-claim path: a fresh restore produces
an empty operation log and an in-flight deposit is stranded. Hence the recovery
story is "mnemonic + client data directory"; the mnemonic and full-directory backup are
age-encrypted (the live databases rely on full-disk encryption).

Still open: a reorg that orphans a *credited deposit* — whether the watermark or
`net_credit` can double-count. The BDK side is now proven correct, so this test is
no longer confounded by the §35 bug.

---

## 42. Logging and the reorg-rescan notice

### 42.1 CLI subscriber

`crates/berlin-wallet-cli/src/main.rs:init_tracing()` installs one stderr subscriber at the very
start of `main()`:

```rust
let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
let _ = tracing_subscriber::fmt()
    .with_env_filter(filter)
    .with_writer(std::io::stderr)
    .try_init();
```

- `RUST_LOG` controls filtering; unset/invalid falls back to `info`.
- Output goes to **stderr**, leaving stdout for the REPL prompt and command output.
- `try_init()` is idempotent; CLI test `tracing_init_is_idempotent` calls it twice and does not
  panic.

`tracing-subscriber = "=0.3.23"` with the `env-filter` feature is a CLI dependency.

### 42.2 FFI subscriber ownership

`crates/berlin-wallet-ffi` deliberately **does not install a tracing subscriber**. The embedding
mobile app owns logging. If the host wants core logs it should install its own subscriber using
`try_init()` (or a `Once` guard); there is no second initialization inside the FFI library, so
double-init cannot panic. Core log calls do not force a subscriber.

### 42.3 Reorg rescan log

The orphaned path in `BdkWallet::sync` now emits (`bdk.rs:344-349`):

```rust
tracing::info!(
    local_height,
    remote_height,
    remote_tip = ?remote_tip_hash,
    "BDK checkpoint is orphaned; rebuilding wallet DB and running full rescan"
);
```

Level: **INFO** — a full DB rebuild/rescan is operationally significant.

Exact invocations:

```bash
# all info from the wallet/core
RUST_LOG=info ./target/debug/berlin-wallet run <invite-code> <esplora-url>

# only the BDK sync module (the rescan line)
RUST_LOG=berlin_wallet_core::bdk=info ./target/debug/berlin-wallet run <invite-code> <esplora-url>
```

Text rendered:

```text
INFO berlin_wallet_core::bdk: BDK checkpoint is orphaned; rebuilding wallet DB and running full rescan local_height=... remote_height=... remote_tip=...
```

Verified with `RUST_LOG=info` against an invalid Esplora URL: core `tracing` lines (including
`berlin_wallet_core::sync: BDK chain sync failed; will retry`) appeared on stderr while stdout stayed
clean. The rescan line is in the `ReorgDecision::Orphaned` branch immediately before
`reset_and_full_scan()`.

## 43. Chain backend APIs — Electrum and bitcoind RPC (source-verified)

The chain source is now a trait (`chain::ChainBackend`). The contract is
symbol-verified against the **vendored sources resolved by `Cargo.lock`**, not
against docs.rs and not against a plan:

| Symbol | Version / file |
|---|---|
| `BdkElectrumClient::new(client)` | `bdk_electrum-0.23.2/src/bdk_electrum_client.rs:30` |
| `BdkElectrumClient::sync(request, batch_size, fetch_prev_txouts)` | `:198` |
| `BdkElectrumClient::full_scan(request, stop_gap, batch_size, fetch_prev_txouts)` | `:114` |
| `BdkElectrumClient::transaction_broadcast(&tx)` | `:90` |
| `electrum_client::Client::{new, from_config}` | `electrum-client-0.24.1/src/client.rs:151,157` |
| `ConfigBuilder::{timeout, retry, build}` | `electrum-client-0.24.1/src/config.rs:58,64,76` |
| internal reconnect loop with `thread::sleep` | `electrum-client-0.24.1/src/client.rs:32-100` |
| `ElectrumApi::block_headers_subscribe` / `block_header(h)` | `electrum-client-0.24.1/src/api.rs:193,188` |
| `Emitter::new(client, last_cp, start_height, expected_mempool_txs)` | `bdk_bitcoind_rpc-0.22.0/src/lib.rs:81` |
| `Emitter::{next_block, mempool}`, `NO_EXPECTED_MEMPOOL_TXS` | `:202,111,63` |
| `BlockEvent::{block, block_height, block_hash, connected_to}` | `:226-264` |
| emitter rewind on an orphaned start checkpoint | `:308-329,360-377` |
| `bitcoincore_rpc::Auth::{None,UserPass,CookieFile}` | `bitcoincore-rpc-0.19.0/src/client.rs:197` |
| `Client::{new, from_jsonrpc}`; `new` cannot set a timeout | `:1288,1297` |
| `jsonrpc::simple_http::Builder::{url, auth, cookie_auth, timeout, build}` | `jsonrpc-0.18.0/src/http/simple_http.rs:378,384,395,372,417` |
| `jsonrpc::Client::with_transport` | `jsonrpc-0.18.0/src/client.rs:41` |
| `jsonrpc::error::RpcError { code: i32, message, .. }` | `jsonrpc-0.18.0/src/error.rs:124-131` |
| `Wallet::apply_block_connected_to` / `apply_unconfirmed_txs` / `apply_evicted_txs` | `bdk_wallet-3.2.0/src/wallet/mod.rs:2702,2749,2816` |
| `Wallet::spk_index()` → `KeychainTxOutIndex::last_used_index` | `bdk_wallet-3.2.0/src/wallet/mod.rs:2590`; `bdk_chain-0.23.3/src/indexer/keychain_txout.rs:945` |
| `CreateParams::create_wallet_no_persist` | `bdk_wallet-3.2.0/src/wallet/params.rs:212` |

Compile-checked properties (a scratch crate against the same pinned versions):

- `SyncRequest<I>` and `FullScanRequest<K>` are `Send` (`Box<dyn Iterator + Send>`,
  `Box<dyn FnMut + Send + 'static>` in `bdk_core-0.6.4/src/spk_client.rs:9,11,241,477`),
  so a request can cross `spawn_blocking` while the wallet stays on the async task.
- `electrum_client::Client`, `BdkElectrumClient<Client>`, and
  `bitcoincore_rpc::Client` are `Send + Sync`.
- `Emitter<Arc<Client>>` type-checks (`Arc<C>: Deref<Target = C>`), which is what
  lets the emitter be driven from a blocking task.

Behavioural facts that the code depends on:

- `bdk_electrum` **also** no-ops when the remote tip is lower than the local
  checkpoint (`bdk_electrum_client.rs:612-619`, returning `prev_tip`). The reorg
  anchor check is therefore required for Electrum exactly as it is for Esplora.
- `bdk_bitcoind_rpc`'s `Emitter` searches `last_cp` for an agreement point and
  rewinds, but the wallet's own DB swap stays the authoritative repair path.
- Regtest devimint esplora ships electrs; its regtest Electrum RPC defaults to
  `127.0.0.1:60401`.

Not verified here (no live node in this environment):

- An Electrum sync/full scan/restore against a live electrs.
- A bitcoind block-walk sync, mempool eviction handling, and a birthday-bounded
  restore against a live node.
- `invalidateblock` on the bitcoind backend (expected to work, unlike the
  devimint Esplora binary; §36/§40).
- `bip158::FilterIter` is **not** used. `getcfilters` and the block-filter-index
  requirement were not tested, so no code depends on them.

Update 2026-10-31: §51 live-ran Electrum sync, funded deposit, withdrawal,
restart, and `chain.json` reuse. The Electrum full scan/restore bullet above is
still not live-verified.

## 44. Network selection is explicit and fail-closed

`network_from_chain_id` used to return `Network::Regtest` for any chain-id it did
not recognise. That is the upstream server behaviour (`fedimint-server-core`),
and it is wrong for a wallet: a wrong network means wrong address prefixes and
wrong BIP84 descriptors, with no warning. It now returns
`anyhow::Result<Network>`; the four well-known block-1 hashes still map to
mainnet, testnet, signet, and signet (Mutinynet), and anything else is an error.

The error is resolvable without reintroducing a silent fallback:

- `init` requires `--network <mainnet|testnet|signet|regtest>` and records it in
  `<data-dir>/network.json` (version 1, mode 0600, atomic temp+rename).
- `run`/`open` read that record, derive the chain-id network, and refuse a
  mismatch.
- An unknown chain-id is accepted **only** when the recorded choice is
  `regtest`. That is the explicit opt-in for devimint and private chains; it is
  not a default.
- `init_network` refuses to change the recorded network of an initialized data
  directory. A different network needs a fresh directory.

The FFI follows the same rule and is a breaking signature change, as agreed:
`PegWallet::init(network: String, passphrase: String)`, `PegWallet::restore(invite_code,
chain_source, network: String, passphrase: String)`, and `InitView` gains `network`; the
passphrase is the at-rest mnemonic passphrase (§56). `join` and
`open` do not take a network; they require the recorded file and verify it, so
an app cannot skip the choice. `WalletState` carries the verified
`bitcoin::Network`.

Tests in `crates/berlin-wallet-core/src/network.rs`:

- known chain-ids map to their networks;
- an unknown chain-id fails with the chain-id and the `--network regtest` hint;
- a known chain-id must match the recorded network;
- an unknown chain-id is accepted only with an explicit `regtest` record;
- `network.json` round-trips, a missing record has a clear error, a corrupt file
  is reported, and `init_network` refuses a change.

The CLI argument split (`--network` is removed before positional parsing) is
covered by `network_flag_is_removed_before_positional_parsing`. This also fixes
the old `init` argument bug: the chain source is `args[2]`, not `args[3]`.

## 45. Adoption/watcher race on a successful deposit claim

Live observation (2026-10-31, devimint v0.12.0, bitcoind backend on the same
node the federation used):

- `deposit 100000` broadcast funding tx `0d801bad…`; the wallet journal row was
  `AddressShown` with `funding_txid` set and `funding_outpoint`/`operation_id`
  unset.
- The operation log contained a walletv2 `Receive` operation with
  `outpoint = 0d801bad…:0`, `value = 100000`, `fee = 316`, and
  `outcome = Success`.
- `ecash-balance` was `99,577 sat`, i.e. 100,000 minus the claim fee and the
  mintv2 operation fee. The federation had observed and claimed the deposit.

Cause: `adopt_orphan_claims` and the per-deposit observer race on the same
claim. Adoption checked only the claim-ownership map; when the requested row
had not bound yet it inserted an `Adopted` row instead of binding the open one.
That marked the outpoint bound, and `scan_receive_claims` filtered bound
outpoints, so the observer could no longer close the requested row. The journal
ended with two rows for one deposit.

Fix:

- `adopt_orphan_claims` matches an open requested row by address script and
  amount before adopting. It binds or closes that row with the claim's figures
  and only inserts an `Adopted` row when no open row matches.
- `watch_deposit` scans all successful claims, not only unbound ones, so it can
  close its row even when adoption won the bind.
- `close_deposit_done_if_open` now records the operation id with the gross and
  claim fee; a repaired row is indistinguishable from a normally bound one.
- `list-deposits` exposes every row, including `Adopted`, so the duplicate is
  visible instead of hidden behind a known id.

Tests: `an_open_row_matches_a_claim_by_script_and_amount` and
`closing_an_open_row_from_an_adopted_claim_records_the_operation`.

Resolution (2026-10-31, after the fix in #35): on restart the requested row was
`Done` with `operation_id = d941d2bb_bb6658d0`, `funding_outpoint =
0d801bad…:0`, `gross_deposited = 100000 SAT`, `claim_fee = 316 SAT`, and
`net_credit = 99,577,856 msat`; `ecash-balance` matched exactly. `list-deposits`
shows the `Requested` and `Adopted` rows for the same txid and operation id,
which is one deposit with a historical duplicate claim owner, not two deposits.
A restart did not change the balance.

Display note: `list-deposits` (CLI) and `PegWallet::list_deposits` (FFI) collapse
rows by funding txid and show one row per deposit. The `Requested` row is shown
when both it and the `Adopted` duplicate are `Done`; the `Adopted` row remains
addressable with `status <id>` for audit. A genuine adopted-only deposit is not
hidden.

## 46. CLI shutdown and signal handling

- `quit` sends the shutdown flag, drains the deposit observers, waits up to five
  seconds for the BDK/fedimint/adoption tasks, then calls
  `ClientHandle::shutdown` before returning. A second `run` can open the same
  data directory immediately; the RocksDB lock is not left behind.
- SIGINT is trapped. At the prompt it prints a hint and keeps the process alive.
  During `watch` it cancels the foreground wait and returns to the prompt; the
  background observer and the 60 s adoption pass continue.
- SIGTSTP (Ctrl+Z) is trapped and refused with a message. Suspending the process
  would hold the database lock, so the wallet tells the user to `quit`.
- `list-deposits` and the FFI `list_deposits` collapse duplicate rows by funding
  txid (preferring `Done`, then `Requested`), so one deposit displays once.

Gate (PTY): Ctrl+C printed the interrupt hint, Ctrl+Z printed the suspend
warning, `quit` exited 0, and an immediate second `run` opened the same data
directory. A `RUST_LOG=debug` quit produced no WARN or ERROR lines.

## 47. First-class on-chain send

- `BdkWallet::quote_send(destination, amount, fee_rate)` builds the PSBT with
  `build_tx().add_recipient(..).fee_rate(..).finish()`, then reports the exact
  fee (`Wallet::calculate_fee`), total debit, change, input count, and txid.
  Nothing is signed or broadcast at quote time.
- `BdkWallet::execute_send(plan)` signs with the BIP84 signers, extracts the
  transaction, verifies the txid did not change, broadcasts through
  `ChainBackend::broadcast`, applies the unconfirmed tx to the wallet graph, and
  persists.
- `OnchainSendRow` is stored under tag `0x09` with state `Broadcast`; it records
  address, amount, fee rate, fee, vsize, change, and txid. `list-onchain` reads
  both the journal rows and `Wallet::history()` (confirmation state and
  sent/received/fee from the BDK graph).
- CLI: `send <address> <sat> <sat/vb>` prints the quote before signing;
  `list-onchain` prints journalled sends and wallet transactions.

Live gate (2026-10-31, devimint v0.12.0, bitcoind backend on the same node):

- 10,000 sat at 2 sat/vB: fee 281 sat, total debit 10,281 sat, change
  9,989,719 sat, 1 input, txid `65ab176d…`.
- Broadcast succeeded; the journal row and the unconfirmed wallet transaction
  appeared immediately; after one block the transaction showed `height 156
  (1 conf)`.
- Restart preserved the journal row and history.
- `invalidateblock` at height 156 reduced the remote tip to 155. The wallet
  logged the orphaned-checkpoint rebuild (`scan_floor=0`,
  `floor_source=configured_birthday`), full-scanned, and reported the send
  unconfirmed. Re-mining the mempool returned it to `height 156 (1 conf)`.
- Signet is not available in this repository's harness; #38 tracks that run.

Known window: the journal row is inserted after broadcast. A crash between the
broadcast and the insert leaves the transaction in the BDK graph but without the
audit row; the crash matrix (#25) owns that window.

## 48. lnv2 client API — source-verified

Verified against the pinned Fedimint tag `v0.12.0` (`2ead392`) in the
`modules/fedimint-lnv2-client` and `modules/fedimint-lnv2-common` crates. No
lnv2 code is registered in this workspace yet; this section is the gate for
#23.

### 48.1 Identity and registration

- Crate: `fedimint-lnv2-client = "0.12.0"` (path `modules/fedimint-lnv2-client`),
  with `fedimint-lnv2-common` as its common types. The workspace
  `[patch.crates-io]` already pins both to the `v0.12.0` tag.
- Module kind is `lnv2` (`modules/fedimint-lnv2-common/src/lib.rs:47`),
  consensus version 1.0. It is distinct from the v1 `ln` module.
- `LightningClientInit` is at `modules/fedimint-lnv2-client/src/lib.rs:222`.
  `Default` (`:236`) sets `gateway_conn = None` and `custom_meta_fn = Null`.
  `ClientModuleInit::init` (`:258`) then builds
  `RealGatewayConnection { api: GatewayApi::new(None, connector_registry) }`
  (`:282-290`). **There is no client-side gateway URL to configure**; gateways
  come from the federation's registered list.
- Register with `ClientBuilder::with_module(LightningClientInit::default())` and
  read the module with
  `Client::get_first_module::<LightningClientModule>()`. Alias the import
  (`Lnv2ClientInit`) because the v1 crate exports the same type name;
  `fedimint-cli` registers both (`fedimint-cli/src/lib.rs:836,841`), while this
  wallet only needs v2.
- Live check: the devimint v0.12.0 federation exposes `lnv2` as module 0
  (`join` output: `module 0: lnv2`, `primary BTC module: Some(2)`).

### 48.2 Send

```rust
LightningClientModule::send(
    invoice: Bolt11Invoice,
    gateway: Option<SafeUrl>,
    custom_meta: serde_json::Value,
) -> Result<OperationId, SendPaymentError>
```

`modules/fedimint-lnv2-client/src/lib.rs:539`. It validates the invoice amount,
expiry, and currency, derives a deterministic operation id from
`(invoice, 0u64)`, rejects `operation_exists` as
`SendPaymentError::DuplicatePaymentAttempt`, selects a gateway when `None`,
checks the gateway fee/expiry limits, builds an `OutgoingContract`, submits a
transaction with one Lightning output, and logs `SendPaymentEvent`.

- `await_final_send_operation_state(operation_id)
  -> anyhow::Result<FinalSendOperationState>` (`:797`);
  `FinalSendOperationState::{Success([u8; 32]), Refunded, Failure}` (`:170`).
- `subscribe_send_operation_state_updates(operation_id)` (`:732`) yields
  `SendOperationState::{Funding, Funded, Success, Refunding, Refunded, Failure}`
  (`:153`).
- `get_invoice_send_status(&Bolt11Invoice) -> anyhow::Result<InvoiceSendStatus>`
  (`:687`); `InvoiceSendStatus::{NotAttempted, InFlight, Succeeded, Failed}`
  (`:1365`). Call this before paying an invoice through any other path; only
  `NotAttempted` is safe to treat as unpaid.
- `SendOperationMeta` (`:82`) stores `change_outpoint_range`, `gateway`,
  `contract`, `invoice`, and `custom_meta`; `gateway_fee()` (`:99`) returns the
  absolute gateway fee from the invoice/contract.
- Errors: `SendPaymentError` (`:1378`) covers missing amount, expiry, duplicate
  attempt, gateway selection, unsupported federation, fee/expiry limits,
  currency mismatch, and funding failure.

### 48.3 Receive

```rust
LightningClientModule::receive(
    amount: Amount,
    expiry_secs: u32,
    description: Bolt11InvoiceDescription,
    gateway: Option<SafeUrl>,
    custom_meta: serde_json::Value,
) -> Result<(Bolt11Invoice, OperationId), ReceiveError>
```

`modules/fedimint-lnv2-client/src/lib.rs:835`. The gateway creates the Bolt11
invoice; the client creates the `IncomingContract` and returns both the invoice
and the operation id.

- `await_final_receive_operation_state(operation_id)
  -> anyhow::Result<FinalReceiveOperationState>` (`:1204`);
  `FinalReceiveOperationState::{Expired, Claimed, Failure}` (`:210`).
- `subscribe_receive_operation_state_updates(operation_id)` (`:1163`) yields
  `ReceiveOperationState::{Pending, Expired, Claiming, Claimed, Failure}`
  (`:195`).
- `ReceiveOperationMeta` (`:105`) stores `gateway`, `contract`, `invoice`, and
  `custom_meta`; `gateway_fee()` (`:118`).
- Limits: `MAX_INVOICE_EXPIRY_SECS = 86400`
  (`gateway_api.rs:128`), and `MINIMUM_INCOMING_CONTRACT_AMOUNT = 5 sat`
  (`common/lib.rs:52`). `ReceiveError` (`:1407`) covers gateway selection,
  unsupported federation, fee, small amount, invalid/incorrect invoice, and
  expiry.

### 48.4 Fees

- `send_fee_quote(amount: Amount) -> anyhow::Result<FeeQuote>` (`:913`) and
  `receive_fee_quote(amount: Amount) -> anyhow::Result<FeeQuote>` (`:884`)
  quote the **federation transaction** fee: Lightning input/output fees, mint
  fees on the funding notes/change, and sub-denomination dust. The gateway's
  off-chain fee is **not** included.
- The gateway fee comes from `RoutingInfo` (`gateway_api.rs:149`):
  `send_fee_minimum` (direct swap), `send_fee_default` (Lightning swap),
  `send_fee` limits, `receive_fee`, and `expiration_delta_{minimum,default}`.
  It is also recoverable from `SendOperationMeta::gateway_fee()` /
  `ReceiveOperationMeta::gateway_fee()`.
- `PaymentFee` (`gateway_api.rs:194`) is `{ base: Amount, parts_per_million }`.
  Client-enforced limits: send `1.5% + 100 sat` (`SEND_FEE_LIMIT`, `:203`),
  receive `0.5% + 50 sat` (`RECEIVE_FEE_LIMIT`, `:217`). The outgoing contract
  expiration delta is capped at 1440 blocks.
- `spendable_amount(balance: Amount, gateway: Option<SafeUrl>) -> anyhow::Result<Amount>`
  (`:959`) binary-searches the largest invoice amount that fits the balance,
  using the default send fee. It is point-in-time; `send` remains authoritative.
- `FeeQuote` is `fedimint_client_module::transaction::FeeQuote`, the same
  non-serializable type the withdrawal path already wraps in `FeeQuoteWire`.
  Reuse that wire form or add an lnv2-specific one.
- `LightningClientConfig` (`common/config.rs:39`) exposes `tpe_agg_pk`,
  `tpe_pks`, `fee_consensus`, and `network`. `FeeConsensus` (`:62`) is the
  federation's per-input/output fee.

### 48.5 Gateways

- `select_gateway(invoice: Option<Bolt11Invoice>)
  -> Result<(SafeUrl, RoutingInfo), SelectGatewayError>` (`:454`): queries the
  federation's vetted gateways, prefers the invoice's payee key, checks
  `routing_info`, and falls back to a random online gateway.
- `list_gateways(peer: Option<PeerId>)` (`:493`) and
  `routing_info(&SafeUrl)` (`:512`).
- Federation API (`api.rs:21-40`): `gateways()`, `gateways_from_peer(peer)`,
  `add_gateway(auth, gateway)`, `remove_gateway(auth, gateway)`,
  `consensus_block_count()`, `await_preimage(outpoint, expiration)`,
  `await_incoming_contract(contract_id, expiration)`,
  `await_incoming_contracts(start, n)`.
- Adding/removing a gateway is an **admin/guardian** action (`ApiAuth`); a
  normal client only lists and selects. `fedimint-cli` exposes
  `lnv2 gateways add/remove` for that admin path
  (`modules/fedimint-lnv2-client/src/cli.rs`).
- Devimint's `dev-fed` starts LND/LDK gateways and registers them; the client's
  `list_gateways` is the runtime check that a gateway is reachable.

### 48.6 Events and journal hooks

- `SendPaymentEvent { operation_id, amount, fee }`, kind `payment-send`,
  persistent (`modules/fedimint-lnv2-client/src/events.rs:8,16`).
- `SendPaymentUpdateEvent { operation_id, status }`, kind
  `payment-send-update` (`:31,38`); status is `Success([u8; 32])` or `Refunded`.
- `ReceivePaymentEvent { operation_id, amount, fee }`, kind `payment-receive`
  (`:45,56`).
- `custom_meta: Value` is carried in both operations and in
  `SendOperationMeta`/`ReceiveOperationMeta`. Use the journal id there, as the
  withdrawal flow already does, so restart adoption can match an operation to a
  row without re-sending.
- Operation meta is `LightningOperationMeta::{Send, Receive, LnurlReceive}`
  (`:78`). `SendOperationMeta.change_outpoint_range` is the handle for
  `await_primary_module_outputs`, mirroring the withdrawal change wait.

### 48.7 Trust and liquidity boundary

This is **not** a trustless Lightning bridge. The gateway is a Lightning node
that holds a channel balance and federation ecash; the federation is custodial
for the ecash and can halt, censor, or rug; Lightning routing nodes see payment
metadata. The protocol limits what each party can do, but it does not remove
the counterparties.

- **Send:** the client locks an `OutgoingContract` (payment hash, amount,
  expiry, gateway claim key, client refund key). The gateway pays the invoice
  and claims the contract by revealing the preimage; on failure it returns a
  forfeit signature and the client refunds after expiry. The gateway can
  censor/delay and sees the invoice, amount, and payment hash. The federation
  enforces the contract but sees the same contract.
- **Receive:** the client creates an `IncomingContract`; the gateway creates a
  hold invoice. The payer pays the gateway's LN node; the federation provides
  decryption shares; the gateway decrypts the preimage and claims, while the
  client claims the incoming contract. The gateway can fail to route or stay
  offline; the contract/expiry protects the client's funds, but the gateway
  sees the payment and controls the invoice.
- **Liquidity:** the gateway needs **outbound** liquidity to pay invoices for
  clients and **inbound** liquidity to receive for them, plus on-chain funds for
  channel management. A gateway without liquidity is online but cannot route;
  `routing_info` succeeds while `send` can still fail.
- **Fees:** the gateway's fee schedule is in `RoutingInfo`; the federation's
  transaction fee is in `FeeQuote`. The client enforces the limits above and
  rejects a gateway that exceeds them.

### 48.8 Not verified / implementation notes for #23

- The crate is not a workspace dependency yet; no lnv2 client is registered and
  no live send/receive has been attempted.
- `LightningClientInit::default()` needs no gateway configuration; the
  federation's gateway list is the source. A test that needs a specific gateway
  can pass `Some(SafeUrl)` to `send`/`receive`.
- Register only v2 (`lnv2`); the live federation has no v1 `ln` module.
- Reuse `FeeQuoteWire` for the two quote calls, and record both the gateway fee
  and the federation fee in the journal row.
- Call `get_invoice_send_status` before any retry; `send` itself rejects a
  duplicate operation id.
- The event names and `custom_meta` support the same adoption/journal pattern
  as deposits and withdrawals.
- Still unverified at runtime: gateway selection against a real federation,
  paid/failed/expired/interrupted invoices, and refund/claim timing on a
  regtest federation. Those are #23's live gates.

## 49. lnv2 send/receive implementation and live gate

Implementation:

- `client::configured_builder` registers
  `fedimint_lnv2_client::LightningClientInit::default()` alongside mintv2 and
  walletv2. The live federation exposes `lnv2` at module 0.
- `crates/berlin-wallet-core/src/lightning.rs`:
  - `prepare_send` checks `get_invoice_send_status` first, selects the gateway,
    quotes the gateway fee and the federation `send_fee_quote`, writes a
    `Pending` row, and returns the quote. The row id is the `custom_meta.id`.
  - `execute_send` calls `send` once, persists the operation id, and waits up to
    300 s for `await_final_send_operation_state`; the adoption tick resumes a row
    that outlives that wait.
  - `request_receive` selects/quotes the gateway, writes a `Pending` row, calls
    `receive`, and persists the invoice, payment hash, and operation id.
  - `refresh_row`/`refresh_pending` update final states; `ln-status` and
    `ln-list` refresh pending rows on demand.
  - `adopt_lightning_operations` scans the operation log for
    `LightningOperationMeta::{Send,Receive}`, matches `custom_meta.id` or the
    operation id, and updates/creates a row. It never calls `send`/`receive`.
  - Receive rows have a local expiry fallback: if the invoice is expired and
    the upstream final stream has not returned, the row is marked `Expired`.
- Journal: `LightningRow` under tag `0x0A` with kind (Send/Receive), origin,
  invoice, payment hash, amount, gateway, gateway fee, federation fee quote,
  operation id, state, preimage, expiry, and last error. States are `Pending`,
  `Succeeded`, `Refunded`, `Claimed`, `Expired`, `Failed`.
- CLI: `ln-send <invoice> [gateway]`, `ln-receive <sat> [expiry] [gateway]
  [description...]`, `ln-status <id>`, `ln-list`, `ln-gateways`.

Live gate (2026-10-31, devimint v0.12.0, fresh data directory):

- **Paid send**: 1,000,000 msat invoice via the LDK gateway. Quote: gateway
  5,000 msat, federation 2,104 msat, total 1,007,104 msat. Row `Succeeded` with
  preimage; the LND invoice was `SETTLED` for 1,000 sat; the ecash balance fell
  by exactly 1,007,104 msat.
- **Paid receive (online)**: 50,000,000 msat invoice via the LDK gateway. Quote:
  gateway 152,000 msat, federation 2,688 msat, net 49,845,312 msat. LND paid it;
  the row became `Claimed`; the balance rose by 49,845,760 msat (the quote is
  conservative by 448 msat).
- **Receive across restart**: a 10,000,000 msat invoice was paid while the client
  was offline. On restart the row was still `Pending`; after the client stayed
  online for 30 s it became `Claimed` and the balance rose by 9,966,080 msat.
  This exercises resume/adoption, not a re-send.
- **Expired receive**: a 1 s invoice was not paid; the row became `Expired` via
  the local invoice-expiry fallback (the upstream final stream lagged).
- **Expired send**: an invoice with a 1 s expiry was refused by `send`; the row
  became `Failed` with "Invoice has expired". No funds moved.
- **Duplicate send**: re-sending an already-`Succeeded` invoice was refused with
  the existing row id and state; no second operation.
- **Gateways**: the federation listed LDK, LND, and a second LDK gateway; the
  send/receive tests used the LDK gateway at `127.0.0.1:10146/v1`.

Upgrade note: a client data directory created **before** lnv2 was registered
does not gain the module. The client stores only the module configs it
registered at join; `get_first_module::<LightningClientModule>()` then reports
"No modules found of kind lnv2". A fresh join or restore is required. The live
gate used a fresh directory.

Not exercised: a gateway that fails after funding (refund/forfeit path), a
gateway with exhausted inbound/outbound liquidity, multiple concurrent invoices,
LNURL, and signet. The expired paths exercise failure handling but not a
gateway-side routing failure.

## 50. Deposit fee preview (2026-10-31)

The preview is computed from the federation's client configs, not guessed:

- `fedimint_walletv2_common::config::WalletClientConfig` has a public
  `fee_consensus: FeeConsensus` (`modules/fedimint-walletv2-common/src/config.rs:215`).
  Its `FeeConsensus` (`:136`) has a public `fee(Amount) -> Amount`; the base is
  100 sat plus `parts_per_million`.
- `fedimint_mintv2_common::config::MintClientConfig` has a public
  `fee_consensus: FeeConsensus` (`modules/fedimint-mintv2-common/src/config.rs:45`);
  its `FeeConsensus` (`:70`) has `fee(Amount) -> Amount`, base 100 msat plus
  `parts_per_million`. `fedimint-mintv2-common` is now a direct dependency of
  `berlin-wallet-core`.
- `client::wallet_client_config` and `client::mint_client_config` read them with
  `ClientConfig::get_first_module_by_kind::<T>(KIND)`. In 0.12.0 that returns
  `(ModuleInstanceId, &T)`; the helpers clone the config.
- `client::deposit_operation_fee_estimate` applies both consensuses to the gross
  deposit and sums them. This models the claim transaction's walletv2 input and
  mintv2 output. It remains labelled an estimate: the live federation's actual
  operation fee also includes change/dust, which the fee consensus does not
  expose.

`request_deposit` calls `WalletClientModule::receive_fee()` for the on-chain
claim fee and stores it with the operation estimate before inserting the row.
`fund_deposit` re-quotes the claim fee at broadcast time. After the claim,
`Client::get_operation_fees` supplies the actual BTC fee; it is stored in
`DepositRow.operation_fee` instead of only being used transiently for
`net_credit`. Both new row fields (`quoted_operation_fee`, `operation_fee`) are
`#[serde(default)]`, so pre-#21 rows load with `None` and display the quote as
unavailable.

CLI behaviour:

- `request-deposit` and `deposit` print the quote before funding:
  `deposit preview:`, amount, on-chain claim fee, operation-fee estimate, and
  estimated net.
- If the operation quote is unavailable the line says
  `operation fee: unavailable (reconciled after claim)` rather than showing zero.
- `status`/`watch` print the quoted estimate, the actual fee, and a mismatch line
  when they differ. The actual fee is authoritative.

Live gate (2026-10-31, devimint v0.12.0, bitcoind backend, `/tmp/bw-lnv2-gate`):

- `deposit 100000` previewed claim fee `316 sat`, operation-fee estimate
  `100100 msat`, and estimated net `99583900 msat`.
- The claim completed with claim fee `316 sat`, actual operation fee
  `103072 msat`, net credit `99580928 msat`; `status` printed
  `operation fee mismatch: quoted 100100 msat, actual 103072 msat`.
- A first attempt that summed only the mintv2 output consensus quoted
  `100 msat` against a `104096 msat` actual. That is why the preview now includes
  the walletv2 input consensus; the remaining few-thousand-msat gap is the
  federation's change/dust, and the mismatch is visible rather than hidden.

Tests: 99 core, 5 CLI, 4 FFI.

## 51. Electrum live gate (2026-10-31)

Environment: fresh devimint v0.12.0 (`dev-fed`), electrs regtest Electrum RPC on
`127.0.0.1:60401`, federation on bitcoind. Wallet at `7ba02b5`; data directory
`/tmp/bw-electrum-gate`; the run was one fresh devimint session.

Sequence and observations:

- `init --network regtest electrum:tcp://127.0.0.1:60401` printed
  `chain source: electrum: tcp://127.0.0.1:60401` and created
  `chain.json` with `{"version":1,"source":"electrum:tcp://127.0.0.1:60401"}`.
- The BDK address was funded with 0.1 BTC and one block was mined.
- In the same REPL: `sync` printed `bdk synced`; `balance` reported
  `confirmed: 0.10000000 BTC`; `deposit 100000` funded the walletv2 address and
  returned after broadcast (`funding_txid=22d6d58e…`).
- Ten blocks later a restart with **no** chain-source argument still printed
  `chain source: electrum: tcp://127.0.0.1:60401` (the stored `chain.json`), and
  `watch` completed the claim: deposit state `Done`, claim fee `316 sat`,
  actual operation fee `106144 msat`, `net_credit=99577856 msat`,
  `ecash-balance=99577856 msat`.
- `withdraw <external-regtest-address> 50000` reached
  `WithdrawalState::Broadcast { txid: bf1c3334… }`; one block later a restart's
  `list-withdrawals`/`status` still showed `Broadcast` with the same txid, and
  `bitcoin-cli gettransaction` reported `confirmations: 1`. This is the
  federation txid reconciliation path, not a BDK scan.
- A bare `init --network regtest` (no chain source) reopened the data directory
  and printed the same Electrum source, proving the `chain.json` round trip.

Covered by this gate: Electrum sync, funded deposit/claim, withdrawal
broadcast + restart reconciliation, wallet restart, and chain-source
persistence/reuse.

Not covered: a reorg observed through Electrum, Electrum full scan/restore, and
injected transient-failure behaviour. Electrum cannot force `invalidateblock`
from the wallet side; the deterministic reorg path is the bitcoind-backend gate
in §47 (`invalidateblock`, unconfirmed, re-mine, one confirmation), and the
shared `BdkWallet` ancestry check is the same code path. Full scan/restore on
Electrum remains unit/partial.

One non-fatal `fm::client` warning occurred while setting the claim outcome
(`Write-write conflict detected`); the operation still reached `Done` with the
expected net credit. It is a client journal concurrency warning, not an Electrum
error.

## 52. Restore assertions, warnings, and balance settlement (2026-10-31)

The restore contract is now executable instead of only documented:

- `restore_policy::RestoreSummary` turns `(operation_log_empty, deposit_count,
  withdrawal_count)` into `history_unavailable`, the exact CLI/FFI warning
  strings, and `is_balances_only()`. The warning strings are:
  - `History unavailable after restore — balances recovered`
  - `A previous deposit may still need to be claimed; keep the original client
    data. A new deposit request does not recover it.`
  - `Recovery requires your mnemonic plus the original client data backup.`
- `recover_client` now calls `Client::wait_for_all_active_state_machines()`
  (`fedimint-client/src/client.rs:2070`) after reopening. mintv2 recovery
  reconstructs notes by adding a pending output state machine; without this
  wait, the reopened client reported `0 msat` immediately while the issuance
  was still settling.
- `cli::restore` prints the operation-log/history flags, journal counts, and
  every warning, and fails unless the result is balances-only.
- FFI `RestoreView` adds `warnings: Vec<String>` and keeps
  `history_unavailable`; `restore` fails if the fresh restore is not
  balances-only. The exact warning text is unit-tested in core, CLI, and FFI.

Live gate (2026-10-31, devimint v0.12.0, bitcoind backend):

- Source `/tmp/bw-restore-source` settled a 100,000 sat deposit:
  `net_credit=99577856 msat`. It then requested and funded a 50,000 sat deposit
  and mined one block, leaving it unclaimed (`state=AddressShown`,
  `funding_txid=e385adcf…`, UTXO value 0.00050000 BTC).
- Fresh mnemonic restore in `/tmp/bw-restore-fresh`:
  - `restore ecash balance: 99577856 msat (99577 sat)` — recovered.
  - `restore bdk balance: { …, confirmed: 0.09849694 BTC }` — matched the
    source balance.
  - `restore operation log empty: true`
  - `restore history unavailable: true`
  - `restore journal deposits: 0`
  - `restore journal withdrawals: 0`
  - all three warnings were printed.
- The new `request-deposit 10000` after restore returned the **same** walletv2
  address as the stranded 50,000 sat UTXO (`bcrt1qf64…`) in this run. After ten
  more blocks the new row was still `AddressShown`, ecash stayed
  `99577856 msat`, and the stranded UTXO was still unspent (11 confirmations).
  Address reuse is therefore not a recovery signal: a new receive did not
  re-claim the old outpoint.

An earlier run of the same gate exposed the settlement bug: recovery logged
`amount=Some(99577856msat)`, but the CLI printed `0 msat` because it read the
balance before the recovered-notes output state machine ran. The active-state
wait in `recover_client` fixed it; the final run above is the post-fix result.

Tests: 103 core, 6 CLI, 5 FFI.


## 53. Automated crash/restart matrix (2026-10-31)

The deterministic matrix lives in `crates/berlin-wallet-core/src/crash_matrix.rs`
and runs as part of `cargo test --workspace` (no federation required). Each test
performs a partial operation, drops the `Journal` (the process crash), reopens
the same `Database`, and asserts the exactly-once invariant.

| Window | Deterministic test | Assertion after restart |
|---|---|---|
| Address shown before funding | `address_shown_before_funding_survives_restart` | `AddressShown`, no funding txid, one open row; a retry reuses it; cancel releases the slot. |
| Funding broadcast before claim | `funding_broadcast_before_claim_survives_restart_without_rebroadcast` | The recorded funding txid is still present; the retry path returns it rather than rebuilding. |
| Crash during confirmation | `confirmation_restart_claims_exactly_once` | `claim_bind` is idempotent for the owner; a replay of `finish_deposit` leaves one `Done` row, one `net_credit`, and one bound outpoint; a later row cannot steal the outpoint. |
| Crash after `Done` | `crash_after_done_keeps_one_credit_and_releases_the_slot` | One `Done` row and credit; no open row; a new deposit is accepted. |
| After `send()` before operation id | `send_after_send_before_operation_id_is_adopted_without_resend` | The boot adoption binds the existing `Quoting` row by `custom_meta.id`/operation id; one row, no second send; repeated adoption dedups. |
| Final send state before `Broadcast` | `final_send_state_before_broadcast_restart_sets_broadcast_once` | The row is `Submitted` with the operation id and is retried; `set_broadcast` is idempotent and releases the slot. |
| Change-output finalization timeout | `change_output_timeout_keeps_submitted_for_retry` | A timed-out change wait leaves `Submitted`; the reconciliation query still finds it; a later retry writes `Broadcast` once. |
| Broadcast before BDK/journal persistence | `after_broadcast_before_bdk_and_journal_persistence` (`#[ignore]`) | **Devimint-only and currently uncovered.** The network broadcast precedes both BDK persistence and the journal's `funding_txid` write; the in-memory journal cannot reproduce that boundary. The deterministic suite starts once the txid exists. The adjacent post-persistence live window is §27.2; this narrower boundary has no live test yet. |

`fault::crash_point` hooks remain the live-process counterpart:
`after_deposit_address`, `after_deposit_broadcast`, `after_deposit_done`,
`after_send`, and `after_broadcast`. The ignored test is listed in
`crash_matrix::DEVIMINT_ONLY_WINDOWS`; the CI matrix asserts that the list is
explicit so a future uncovered window cannot be added silently. The narrow
broadcast-before-persistence window remains a documented live gap, not a
CI-proven guarantee.

The journal tests prove the app-level exactly-once transitions. They do not
replace the live evidence that the federation/walletv2 issued or broadcast the
underlying operation once (§27); they prove the recovery path cannot create a
second row, second claim owner, or second broadcast transition.

Tests: 111 core pass + 1 devimint-only ignored, 6 CLI, 5 FFI.

## 54. `watch` heartbeat and cancellation (2026-10-31)

The CLI `watch <deposit-id>` now emits progress to **stderr** while stdout keeps
the final machine-readable row. It prints an initial line and a heartbeat every
10 seconds:

```text
watch <id> (deposit): elapsed=0s state=AddressShown event_position=EventLogId(30) federation_event_position=EventLogId(30) last_observed_position=EventLogId(30) funding_txid=7ac2e975… operation_id=<none>
watch <id> (deposit): elapsed=10s state=AddressShown event_position=EventLogId(30) federation_event_position=EventLogId(30) last_observed_position=EventLogId(30) funding_txid=7ac2e975… operation_id=<none>
```

- `state` is the current journal row state.
- `event_position` is the starting federation event-log cursor.
- `federation_event_position` / `last_observed_position` is the last persisted
  cursor (`next_position` or `position`); it advances when an Aborted event is
  consumed.
- `funding_txid` and `operation_id` identify the deposit and claim when known.
- The final line is also on stderr; `print_deposit` remains on stdout.

Ctrl+C already cancels the foreground `watch` from #10. Live smoke on devimint
(bitcoind backend):

- Initial line at `elapsed=0s` and a second at `elapsed=10s`, both naming the
  deposit, state, positions, and funding txid.
- SIGINT printed the interrupt hint and
  `watch cancelled; the background observer (or the 60 s adoption pass) keeps the row updated`,
  then returned to the `berlin-wallet>` prompt without killing the process.

`watch` remains the deposit-claim wait; withdrawal send/status use
`send-withdraw`/`status`. The heartbeat infrastructure is not a new withdrawal
watch.

Tests: 111 core pass + 1 devimint-only ignored, 7 CLI, 5 FFI. The CLI test
`watch_progress_names_state_positions_and_funding` pins the field names.

## 55. Quiet shutdown without hiding normal task errors (2026-10-31)

The CLI tracing subscriber is now built as a `tracing_subscriber::registry()`
with a reloadable `EnvFilter` and the stderr `fmt` layer. `init_tracing` keeps a
`reload::Handle<EnvFilter, Registry>` in a `OnceLock`.

After the REPL stops reading commands, `quiet_fedimint_task_logs()` appends the
`fm::task=off` directive through that handle **before** `shutdown_tx` and
`ClientHandle::shutdown`. This suppresses fedimint's expected
task-cancellation lifecycle messages (`Task shut down uncleanly` and the
output-scanner cancellation reported as a panic) without disabling `fm::task`
while the wallet is running, so genuine task panics remain visible.

Live check (devimint bitcoind data directory with a funded/open deposit, so the
walletv2 output scanner is active):

```text
RUST_LOG=info berlin-wallet run <invite>
> quit
exit 0
```

No `fm::task` WARN or ERROR appeared on stderr after the prompt. The previous
`fm::task` lines are gone; the process still exits 0 and releases the database
lock. The existing `tracing_init_is_idempotent` test covers repeated
initialization; the filter is only narrowed at shutdown.

Tests remain 111 core pass + 1 devimint-only ignored, 7 CLI, 5 FFI.

## 56. Encrypted mnemonic and encrypted backups (2026-10-31)

### Mnemonic at rest

- `seed::load_or_create_mnemonic(path, passphrase)` writes an **age** file using
  `age::Encryptor::with_user_passphrase` (age 0.12.1, passphrase mode; scrypt +
  ChaCha20-Poly1305, authenticated). The file starts with
  `age-encryption.org/v1`; mode `0600`; writes are atomic (sibling temp + rename).
- The wallet passphrase is **not** a BIP39 passphrase. The BIP39 seed still uses
  the empty passphrase; this passphrase only protects the mnemonic at rest.
- A legacy plaintext mnemonic is parsed once, re-written encrypted with the
  supplied passphrase, and a WARN is emitted. `backup` refuses a source that is
  still legacy plaintext until the wallet is run/init once with a passphrase.
- Wrong passphrases and corrupt files fail with a clear decryption error.
- The CLI prompts for a new/confirmed passphrase on `init`, and for the existing
  passphrase on `run`/`join`/`restore`. Automation can set
  `BERLIN_WALLET_PASSPHRASE` (`BERLIN_WALLET_BACKUP_PASSPHRASE` for archives).
- The FFI requires the passphrase on `init`, `join`, `open`, `restore`,
  `restore_backup`, and `backup`; it does not prompt.

### Encrypted backup format

`backup_data_dir(source, destination_file, passphrase)` produces a single age
passphrase-encrypted tar. Inside the authenticated archive is
`.berlin-wallet-backup.json`:

```json
{"format":"berlin-wallet-backup","version":1,"created_at":...}
```

The tar contains the whole directory, including the encrypted mnemonic,
`chain.json`, `bdk.sqlite` + `-wal`/`-shm`, and `fedimint.rocksdb`. The command
holds the fedimint DB lock and writes a uniquely named sibling tar/age temp; it
renames only the complete archive into place. It refuses an existing
destination. `import_data_dir` decrypts to a sibling temporary tar, validates
every tar path (no absolute paths, `..`, links, or special files), verifies the
manifest version/format and required files, extracts to a sibling temporary
directory, and only then renames it into an empty target. Wrong passphrases,
tampering, bad manifests, and partial imports leave the target untouched; stale
temp files/directories are ignored.

The live `fedimint.rocksdb` and `bdk.sqlite` files are **not**
application-encrypted. Full-disk encryption (LUKS/FileVault/BitLocker) is the
supported boundary for the live data directory; the age backup protects the
archive itself. This is a deliberate trade-off, not a claim of DB-level
encryption.

### Live CLI check

Source copied from the existing devimint data directory (`/tmp/bw-lnv2-gate` to
`/tmp/bw-encryption-source`):

- `init --network regtest` with `BERLIN_WALLET_PASSPHRASE=wallet-pass` migrated
  the legacy plaintext mnemonic; the file then began `age-encryption.org/v1`.
- The same command with the wrong passphrase failed:
  `could not decrypt mnemonic ... Decryption failed`.
- `backup /tmp/bw-encryption-backup.age` with
  `BERLIN_WALLET_BACKUP_PASSPHRASE=backup-pass` produced a 1.5 MB mode-0600 age
  file.
- `restore-backup` into `/tmp/bw-encryption-restored` with the backup passphrase
  restored the tree; the mnemonic was still the 252-byte age file.
- `init --network regtest` on the restored directory with the wallet passphrase
  opened successfully.

Tests: 119 core pass + 1 devimint-only ignored, 7 CLI, 6 FFI. Core coverage
includes encrypted mnemonic round trip/wrong passphrase/plaintext migration,
backup/import with WAL/SHM and RocksDB sidecars, tampering, wrong passphrase,
stale temp files/dirs, existing-archive refusal, plaintext-source refusal, and
locked-DB refusal.

## 57. REPL input handling (2026-10-31)

The development REPL now has explicit input normalization and prompt handling:

- `normalize_command_line` strips trailing `\r` and terminal bracketed-paste
  markers (`\x1b[200~`/`\x1b[201~`). PTY test: a single paste of
  `balance\nbalance\nquit\n` executed both `balance` commands and quit.
- A single-character unknown command (for example a stray `s`) is absorbed with
  no error.
- `!<command>` and `shell <command>` run through `sh -c` while the REPL keeps
  its client and database open. PTY test: `!echo shell-ok` and
  `shell echo shell2` both produced their output.
- Other unknown multi-character input prints one concise hint and returns to the
  prompt instead of emitting an `error:` chain.
- The tracing writer is prompt-aware: while the REPL is waiting for input, a
  background log line reprints `berlin-wallet> ` after the record. PTY test with
  an unreachable BDK chain backend showed a `BDK chain sync failed` WARN followed
  immediately by a fresh prompt; the next `quit` was consumed without an extra
  Enter. Logs during commands (progress/watch/command execution) do not redraw an
  extra prompt.
- Startup and `init` use `BdkWallet::peek_destination()`, which prints the current
  unused external address without revealing/advancing the derivation index. Two
  consecutive `init` runs in the same data directory printed the same
  `bdk address`; the explicit `address` command still calls
  `reveal_destination()`.

Tests: `repl_normalises_paste_markers_and_absorbs_stray_keys` and
`bdk::tests::peek_destination_is_stable_until_the_explicit_reveal`.

## 58. Local OP_TRUE signet on-chain send gate (2026-10-31)

Environment: Bitcoin Core 31.0, `-chain=signet -signetchallenge=51`, local
bitcoind RPC; wallet data `/tmp/bw-signet-gate`; `berlin-wallet` at the current
tree. The node was a trivially-true custom signet, not public signet-4 or
Mutinynet.

The harness (`scripts/signet-gate.sh` + `scripts/signet-mine.py`) does:

- `init --network signet "bitcoind:http://u:p@127.0.0.1:18443?birthday=1"`;
- mines 101 blocks to the BDK address (coinbase maturity);
- runs the headless `send <external> 50000 2`;
- mines the send transaction and reads `list-onchain` / `gettransaction`;
- invalidates the block and re-mines to exercise the BDK reorg rebuild.

Observed on the final alpha harness run:

```text
send quote:
  to: tb1qkkprzymnjmea954pzlaekt5svsu4kddmtlf0n3
  amount: 50000 sat
  fee rate: 2 sat/vB
  fee: 281 sat
  total debit: 50281 sat
  change: 4999949719 sat
  inputs: 1
  txid: cc748ba8d29d7800bb79d8e0d33f3d600b96d3c65adfaf57ac7714fecfbecc03
broadcast: cc748ba8d29d7800bb79d8e0d33f3d600b96d3c65adfaf57ac7714fecfbecc03 (141 vB, fee 281 sat)
```

The send confirmed in block 102
(`00000132935f55d663bb59cb415f69e08ca94b0c87e7da1d7d78f611f68d13d0`):

```text
journal: 01a1955c-bd16-7023-a90d-9b3e26ced4ff cc748ba8... tb1qkkp... fee=281 sat
         vsize=141 vB change=4999949719 sat state=Broadcast
gettransaction: confirmations=1 blockhash=0000013293...
```

Reorg: `invalidateblock 00000132935f55d663bb59cb415f69e08ca94b0c87e7da1d7d78f611f68d13d0`
was accepted; `list-onchain` logged
`BDK checkpoint is orphaned; rebuilding wallet DB and running full rescan` and
rebuilt from `birthday=1`. The harness re-mined a new block 102
(`000002833c603a3f72cb68faca717dd433a1ce31587bfb79b5e01f3ae5baa4`) including
the send transaction; `list-onchain` then reported the same txid at height 102
with 1 confirmation.

Coverage boundary: this proves the signet network's transaction path, address
format, fee quote, broadcast, journal row, confirmation state, and BDK reorg
rebuild against local Bitcoin Core. It does **not** prove a public signet
federation/faucet path or public Esplora availability. The local signet node is
started with OP_TRUE, so it is a test chain, not a consensus-representative
public signet.

The harness note `birthday=0` is not usable for a reorg rebuild on a non-regtest
chain: the bitcoind backend refuses a genesis scan and the harness uses
`birthday=1`. Public signet should use a recent birthday height.

Tests: no new unit tests; the top-level headless path is exercised live.
