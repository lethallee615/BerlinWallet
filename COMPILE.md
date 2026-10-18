# Berlin Wallet

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
`BINDGEN_EXTRA_CLANG_ARGS` are already in `.cargo/config.toml [env]`.

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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (30 tests).

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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (30 tests).

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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (30 tests).

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

`cargo check --workspace` + `cargo test --workspace` → **EXIT 0** (30 core tests + 3 CLI tests + 3 FFI tests).
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

- `init` — create/open the BIP39 mnemonic and SQLite BDK wallet, print a regtest address/balance.
- `join <invite>` — build a client, print federation id, client identity, wallet network, and minimum withdrawal.
- `backup <destination-dir>` / `restore-backup <source-dir>` — full data-directory tree copy
  (`mnemonic`, `bdk.sqlite` + `-wal`/`-shm`, `fedimint.rocksdb` and sidecars); non-blocking DB lock
  check; app must be stopped; encryption is external.
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
- `fedimint-lnv2-client` — exists and can be registered, but **not registered**: lightning is out of
  scope for this wallet. The join still succeeds with the lnv2 module present in the config.

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
never have requested. Unit coverage:
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

Decision: **documented full-directory copy** (not an in-app encrypted archive yet). Fedimint backup
APIs are deprecated; the app owns the artifact. The CLI implements:

```text
berlin-wallet backup <destination-dir>
berlin-wallet restore-backup <source-dir>
```

- `backup` copies the **entire data directory tree**, not a file list, so SQLite `bdk.sqlite-wal` /
  `-shm` and all `fedimint.rocksdb` files/sidecars are included. It physically cannot lose recent
  BDK WAL commits. It refuses a non-empty destination and must run with the app stopped.
- **Lock check:** before copying, `backup` takes a non-blocking try-lock on the same advisory lock
  fedimint-rocksdb uses (`source/fedimint.rocksdb`.with_extension(`.db.lock`) →
  `source/fedimint.db.lock`). If held, it exits nonzero:
  `database is locked (...); stop the wallet before running backup`. This was verified against a
  live `run` process.
- `restore-backup` requires an empty data dir and copies the whole tree back.
- **Encryption:** the backup is **unencrypted by design**; the operator must use `age`/`gpg` or
  equivalent. The mnemonic is inside the archive — treat the archive as key material.
- After import, start normally with `run`; **do not call `restore`**. The retained client DB is what
  preserves the walletv2 scanner/index state and the operation log.
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

init() -> Result<InitView>;                                  // mnemonic only, local
open(esplora_url: String) -> Result<WalletInfo>;              // existing DB, no invite
join(invite_code: String, esplora_url: String) -> Result<WalletInfo>; // fresh join only
restore(invite_code: String, esplora_url: String) -> Result<RestoreView>; // fresh dir
restore_backup(source_dir: String, invite_code: String, esplora_url: String)
    -> Result<WalletInfo>;                                    // retained client DB
backup(destination_dir: String) -> Result<()>;                // async
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
net_msat             value + quoted_fee + federation quote fees
```

`request_withdrawal` quotes and persists while `Quoting`; the UI must display these and require
explicit confirmation before `send_withdrawal`. `send_withdrawal` re-quotes immediately before
send and compares to the persisted quote. On drift it persists the new quote and returns an error;
the UI must re-display and re-confirm.

### 33.2.1 Open/shutdown contract

- `init()` only creates/loads the mnemonic.
- `open(esplora_url)` is the no-invite path for an existing client DB (`ClientBuilder::open` via
  `open_existing_client`); use it on app restart.
- `join(invite, esplora_url)` is fresh-join only and errors if a client config already exists.
- `restore` is mnemonic-only fresh recovery; `restore_backup` imports the full directory and opens
  the retained client DB (verifying the invite's federation id).
- `shutdown()` takes the state, sends the watch shutdown signal, waits up to 5 s per task and aborts
  stragglers, then consumes `ClientHandle` via `ClientHandle::shutdown` to stop the executor/task
  group. Mobile MUST call it on backgrounding; otherwise the DB stays locked and `backup` fails
  with `database is locked`.

### 33.3 Smoke test

`scripts/ffi-smoke.sh`:

1. `cargo build -p berlin-wallet-ffi`;
2. generates Kotlin bindings from `target/debug/libberlin_wallet_ffi.so` with the crate's
   `uniffi-bindgen` bin (`--features uniffi-bindgen`);
3. checks the generated `PegWallet` class and `netCreditMsat`/`requestDeposit` surface.

Verified: `ffi smoke ok: cdylib + kotlin bindings (PegWallet/DepositView surface present)`.
Generated Kotlin methods exactly: `backup`, `balanceSat`, `cancelDeposit`, `cancelWithdrawal`,
`depositStatus`, `ecashBalanceMsat`, `fundDeposit`, `init`, `join`, `listDeposits`,
`listWithdrawals`, `minWithdrawalSat`, `open`, `requestDeposit`, `requestWithdrawal`, `restore`,
`restoreBackup`, `sendWithdrawal`, `shutdown`, `withdrawalStatus`.

`cargo tree -p berlin-wallet-ffi -e features` shows only the top-level `uniffi` crate/features; no
fedimint-client `uniffi` feature is enabled.

### 33.4 Tests

- `cargo check --workspace`: exit 0.
- `cargo test --workspace`: 30 core + 3 CLI + 3 FFI unit tests, exit 0.
- FFI smoke script: exit 0.
