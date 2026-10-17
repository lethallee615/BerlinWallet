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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (19 tests).

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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (19 tests).

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

`cargo check` + `cargo test -p berlin-wallet-core --lib` → **EXIT 0** (19 tests).

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

`cargo check --workspace` + `cargo test --workspace` → **EXIT 0** (19 core tests + CLI 0 tests).
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
builder.with_module(MintClientInit);
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
- `join <invite>` — build a client, print federation id, wallet network, and minimum withdrawal.
- `run <invite> [esplora-url]` — interactive session with `balance`, `address`, `min`,
  `request-deposit <sat>`, `fund-deposit <id>`, `deposit <sat>` (both steps), `watch <id>`,
  `request-withdraw <address> <sat>`, `send-withdraw <id>`, `withdraw <address> <sat>` (both
  steps), `status <id>`, `sync`, `quit`.
- Data dir: `$BERLIN_WALLET_DATA_DIR` or `.berlin-wallet`; network is fixed to regtest.
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
