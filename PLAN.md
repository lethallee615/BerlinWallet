# PLAN: Fedimint ↔ BDK Peg Wallet crate (UniFFI-exposable)

## Sources consulted (and cited)

| Source | What it gave me |
|---|---|
| https://docs.rs/fedimint-client/0.12.0 | Full `Client` / `ClientBuilder` / `ClientPreview` / `ClientHandle` API surface |
| https://github.com/fedimint/fedimint-clientd | `main.rs` entrypoint, `AppState`, `multimint` usage (`register_new`, `all`), REST endpoint shape (`/v2/onchain/deposit-address`, `await-deposit`, `withdraw`) |
| https://docs.rs/fedimint-mint-client/0.12.0 | `MintClientModule` methods (reissue/spend ecash) |
| https://docs.rs/fedimint-walletv2-client/0.12.0 | walletv2 `WalletClientModule`: `receive`, `send`, `await_receive`, final-state awaits, fees, `tx_chain` |
| https://docs.rs/bdk-wallet/3.2.0 | `Wallet` API (create/load, addresses, `build_tx`, `apply_update`/`take_staged`/`persist`, chain sync) |
| https://github.com/douglaz/fedimint-wallets | `wallet-fedimint`: `MultiClient`, `FedimintJournal` (app prefix `[0x00]`, raw-byte + serde_json rows, atomic intent+index transitions), `executor`/`runtime` |
| https://github.com/fedimint/fedimint-sdk | `onchain.rs` / `onchain/v2.rs` usage patterns (`find_claim`, 120 s address wait) |
| `VERIFY.md` §8 (this repo) | **Ground truth** for the wallet module: published versions, exact signatures, observation paths, hazards |

All three fedimint crates are at **0.12.0** (released 27 Aug 2026) and **bdk_wallet 3.2.0** (released 20 Sep 2026). The wallet module is **`fedimint-walletv2-client`**, not the v1 `fedimint-wallet-client` (see VERIFY.md §8; the v1 names are SUPERSEDED).

---

## 1. Crate layout (workspace)

```
pegwallet/                      # workspace root
  Cargo.toml                    # workspace definition (edition 2024, rust 1.91 MSRV)
  crates/
    pegwallet-core/             # pure Rust orchestration (no uniffi, no FFI types)
      src/lib.rs
      src/client.rs             # Fedimint Client construction + module registration
      src/bdk.rs                # BDK wallet wrapper + chain sync
      src/deposit.rs            # position-attributed deposit flow
      src/withdrawal.rs         # op-id-attributed withdrawal flow
      src/journal.rs            # crash-safe write-ahead journal (douglaz pattern)
      src/sync.rs               # concurrent BDK chain task + fedimint federation task
      src/ops.rs                # OperationId / idempotency-key mapping
      error.rs
    pegwallet-ffi/              # thin UniFFI crate (depends on pegwallet-core)
      src/lib.rs                # uniffi::export, PegWallet object
      uniffi/                   # generated bindings (TS/Swift/Kotlin/etc. via uniffi-bindgen)
    pegwallet-cli/              # optional dev CLI (not required by spec)
```

---

## 2. Version pins (for Cargo.toml)

| Crate | Version | Role |
|---|---|---|
| `fedimint-client` | `=0.12.0` | federation client, `Client`, `ClientBuilder` |
| `fedimint-mint-client` | `=0.12.0` | ecash mint module client (`MintClientInit`) |
| `fedimint-walletv2-client` | `=0.12.0` | on-chain wallet module client (`WalletClientInit`, `WalletClientModule`) |
| `fedimint-walletv2-common` | `=0.12.0` | `WalletClientConfig`, `OutputInfo`, `TxInfo`, endpoint constants |
| `fedimint-client-module` | `=0.12.0` | `ClientModule` trait, `ClientModuleInit` |
| `fedimint-connectors` | `=0.12.0` | `ConnectorRegistry` |
| `fedimint-core` | `=0.12.0` | `Database`, `OperationId`, `Amount`, `OutPoint`, `FederationId` |
| `bdk_wallet` | `=3.2.0` | on-chain wallet (`Wallet`, `TxBuilder`, persistence); features `["bdk-tx", "file_store", "rusqlite"]` |
| `bdk_chain` | `=0.23.3` | chain data types (`CheckPoint`) |
| `bdk_file_store` | `=0.22.0` | optional flat-file persistence for BDK ChangeSet |
| `rusqlite` | `0.31.0` (`bundled`) | optional SQLite persistence for BDK (via `bdk_chain/rusqlite`) |
| `bdk_esplora` / `bdk_electrum` / `bdk_bitcoind_rpc` | `0.22.1` / `0.23.2` / `0.22.0` | chain source (pick one at runtime) |
| `uniffi` | `=0.32.0` | UniFFI (optional feature in core; required in ffi) |
| `uniffi_bindgen` | `0.32.0` | build-time binding generation |
| `tokio` | `1` (resolved `1.53.3`) | async runtime — **not `=1.48`**: `fedimint-connectors 0.12.0` → `iroh-mainline-address-lookup ^0.4.0` requires `tokio ^1.49` |
| `anyhow` | `=1.0.98` | errors |
| `thiserror` | `=2.0.18` | typed errors |
| `serde` / `serde_json` | `1` (resolved `1.0.229`) / `=1.0.149` | journal serialization — **not `serde =1.0.219`**: `serde_json 1.0.149` requires `serde ^1.0.220` |
| `tracing` | `=0.1.44` | logging |
| `async-trait` | `=0.1.88` | async traits (fedimint ecosystem) |
| `bitcoin` | `=0.32.8` | unified at a single version across fedimint + bdk (see note below) |

Do **not** enable `bdk_wallet`'s `keys-bip39` feature: it requires `bip39 ^2.2.2`, which is not
published (crates.io index max is `2.2.0`). `bdk_wallet`'s `bdk-tx` feature requires
`--cfg bdk_wallet_unstable` (see Build requirements).

The v1 `fedimint-wallet-client` crate is **superseded** by the two walletv2 crates above (VERIFY.md §8).

**MSRV:** `1.91` (effective). `bdk_wallet` advertises `1.85`, but `fedimint-connectors 0.12.0` pulls
`iroh 1.0.3` / `iroh-base 1.3.0` / `netwatch 0.19.3` / `portmapper 0.19.3` / `n0-*`, all of which
declare `rust-version = 1.91`. The default 1.89 toolchain cannot build the graph.
**Edition:** `2024`.

**bitcoin unification:** confirmed **YES** — a single `bitcoin 0.32.8` in `Cargo.toml`/`Cargo.lock`.

### Build requirements (verified — see VERIFY.md §0/§5)

- **Toolchain 1.91.1** — commit `rust-toolchain.toml` (`channel = "1.91.1"`).
- **`cmake`** — required by `aws-lc-sys`, a hard (non-optional) dependency of **both** `fedimint-core` and `fedimint-connectors`, which force its `bindgen` feature via the `ssl` feature.
- **`libclang`** — required by that bindgen step.
- **bdk PSBT/fee API** — `Wallet::create_psbt` (and `build_tx`) require the `bdk-tx` feature **and** `--cfg bdk_wallet_unstable`; `bdk_wallet`'s `src/lib.rs` otherwise emits `compile_error!("The \`bdk-tx\` feature requires \`--cfg bdk_wallet_unstable\`.")`. Configure via `.cargo/config.toml` (`[build] rustflags = ["--cfg", "bdk_wallet_unstable"]`).

Working sandbox setup:

```bash
rustup toolchain install 1.91 --profile minimal
uv tool install cmake
uv pip install --target /tmp/opencode/lc libclang   # glibc-compatible libclang (PyPI)
# LIBCLANG_PATH + BINDGEN_EXTRA_CLANG_ARGS are set in .cargo/config.toml [env]
cargo check   # -> exit 0
```

---

## 3. Client construction & module registration

`fedimint-clientd` constructs its clients via `multimint`; we do the equivalent directly with `fedimint_client`, registering the **walletv2** module:

```rust
// pegwallet-core/src/client.rs
use fedimint_client::{Client, ClientBuilder, ClientHandle, ClientHandleArc, RootSecret};
use fedimint_client::module::module::init::ClientModuleInit; // real path (VERIFY.md §2)
use fedimint_mint_client::MintClientInit;
use fedimint_walletv2_client::WalletClientInit;               // walletv2 (VERIFY.md §8)
use fedimint_connectors::ConnectorRegistry;
use fedimint_core::db::Database;

pub async fn build_client(
    db: Database,
    connectors: ConnectorRegistry,
    root_secret: RootSecret,   // derived from BIP39 mnemonic
    invite: InviteCode,
) -> anyhow::Result<ClientHandleArc> {
    let mut builder = Client::builder().await?;
    builder.with_module(MintClientInit);
    builder.with_module(WalletClientInit::default());
    let preview = builder.preview(connectors.clone(), &invite).await?;
    let client = preview.join(db, root_secret).await?;   // ClientPreview::join -> ClientHandle
    Ok(Arc::new(client))
}
```

Confirmed (VERIFY.md §1): `Client::builder() -> Result<ClientBuilder>`,
`ClientBuilder::with_module<M: ClientModuleInit>(&mut self, module_init: M)`,
`ClientBuilder::preview(self, ConnectorRegistry, &InviteCode) -> Result<ClientPreview>`,
`ClientPreview::join(self, db_no_decoders: Database, pre_root_secret: RootSecret) -> Result<ClientHandle>`,
`Client::start_executor(self: &Arc<Self>)`, `Client::spawn_federation_reconnect(&self)`.

Storage layout (douglaz `MultiClient`): one `Database`; fedimint client `i` lives at
`db.with_prefix([0x01] ++ u32_le(db_prefix))`; the app journal lives under `[0x00]` (separate
RocksDB memtables — co-location causes `TryAgain`/`WriteConflict`).

---

## 4. Peg flows (walletv2 — deposit + withdrawal)

All API names/signatures below are exactly those in **VERIFY.md §8.2/§8.3** (ground truth). The
wallet module is `fedimint_walletv2_client::WalletClientModule`.

### 4.0 Exact signatures (verbatim from VERIFY.md §8.2)

```rust
pub async fn receive(&self) -> Address;                 // no Result, no OperationId

pub async fn send(
    &self,
    address: Address<NetworkUnchecked>,
    value: bitcoin::Amount,
    fee: Option<bitcoin::Amount>,
    custom_meta: serde_json::Value,
) -> Result<OperationId, SendError>;

pub async fn await_final_send_operation_state(&self, operation_id: OperationId)
    -> anyhow::Result<FinalSendOperationState>;
pub async fn await_final_receive_operation_state(&self, operation_id: OperationId)
    -> anyhow::Result<FinalReceiveOperationState>;
pub async fn await_receive(&self, position: EventLogId)
    -> anyhow::Result<(FinalReceiveOperationState, EventLogId)>;

pub async fn receive_fee(&self) -> Result<bitcoin::Amount, ReceiveError>;
pub async fn send_fee(&self) -> Result<bitcoin::Amount, SendError>;
pub async fn send_fee_quote(&self, amount: bitcoin::Amount) -> anyhow::Result<FeeQuote>;
pub async fn max_sendable_amount(&self, balance: Amount, fee: bitcoin::Amount)
    -> anyhow::Result<bitcoin::Amount>;

pub async fn tx_chain(&self) -> FederationResult<Vec<TxInfo>>;
pub async fn pending_tx_chain(&self) -> FederationResult<Vec<TxInfo>>;
```

And the position anchor (`fedimint-client 0.12.0`, VERIFY.md §1):

```rust
pub async fn get_next_event_log_id(&self) -> EventLogId;
```

### 4.1 Deposit — position-attributed, one open request per federation

A deposit is **not an operation we start**. `receive()` only returns an address; the federation
creates the operation when its scanner finds the payment. We attribute the deposit to the
**event-log position read before `receive()`**, never to an address or an invented operation id.

**One open deposit per federation (correctness constraint).** `get_next_event_log_id()` returns the
same `EventLogId` when no events were written in between, and `receive()` writes no event. Two
deposit requests with no payment between them therefore get the **same position**: a
`UNIQUE(federation_id, position)` constraint would reject the second insert, and two rows awaiting
the same position would both resolve on one deposit and **double-credit**. Fix: enforce **one open
deposit request per federation**. `request_deposit` first looks for an open row (`AwaitingAddress`
or `AddressShown`) for that federation and returns it; it does not read a new position or insert a
second row. This matches walletv2 under #9101, which returns the same address until the scanner sees
it paid. **Concurrent deposits are unsupported until #9306 is in the pin.**

**Funding source.** The deposit is **paid from the BDK wallet** (this is the peg-in: on-chain BDK
funds → federation ecash). After the address is persisted, build a transaction from the BDK wallet
to that address (`Wallet::build_tx()` → `create_psbt`/`sign_with_signers`/`finalize_psbt`),
broadcast it via the configured chain source, and persist `funding_txid`. External funding (handing
the address to a third party) follows the same row/observation path minus the BDK transaction step.

**Fees (order matters: quote → show net → fund).** Quote `module.receive_fee().await?` before
funding and persist it; show `net = amount − quoted_fee`. At claim time persist the actual
`ReceiveMeta.fee` and compute `net_credit = gross_deposited − claim_fee` (fall back to the quoted
`receive_fee()` if the claim has not recorded one). The Bitcoin network fee for the funding
transaction is paid by the BDK wallet and is **not** part of `receive_fee()`.

Steps:

1. **Single-flight check.** Look up an open deposit row for `federation_id`
   (`state ∈ {AwaitingAddress, AddressShown}`). If one exists, return it — do not read a new
   position and do not insert a second row.
2. Otherwise read `let position = client.get_next_event_log_id().await;` and persist
   `{ id, federation_id, position, amount, state = AwaitingAddress }` **before** calling `receive()`.
3. **Asynchronously** call `let addr = module.receive().await;` with a **120 s** bound and retry on
   timeout (#9205) up to `MAX_ADDRESS_ATTEMPTS` (5). Persist `{ address = addr, state =
   AddressShown }` **before** exposing it via `deposit_status(id)`. The background address task
   **stops at `AddressShown`**; it does not fund. If the retry budget is exhausted, set
   `state = Failed` with `last_error` and release the single-flight invariant. The FFI caller is
   not blocked (see §6).
   **Steps 4–5 run inside `fund_deposit(id)`**, not in the background address task; the background
   task still stops at `AddressShown`.
4. **Inside `fund_deposit(id)`: quote the deposit fee and show the net before funding**:
   `let quoted_fee = module.receive_fee().await?;` persist it and show
   `net = amount − quoted_fee`. `ReceiveError` variants (`FederationError`,
   `NoConsensusFeerateAvailable`) are transient — retry; they do not move the row to `Failed`.
5. **Still inside `fund_deposit(id)`: fund from BDK** (default, **idempotent**):
   `fund_deposit(id)` first checks `funding_txid`; if it is already set, return it and do **not**
   build a second transaction. Otherwise `bdk_wallet.build_tx()` paying `addr` for `amount`; sign
   with `sign_with_signers`, `finalize_psbt`, broadcast via the configured chain source, and persist
   `funding_txid`. External funding skips this step. If the BDK balance is below `amount`, the
   `build_tx` error is surfaced to the caller and the row stays `AddressShown` (no `funding_txid`
   is written); the caller may retry after funding the BDK wallet or `cancel_deposit`.
6. Subscribe to `ReceivePaymentEvent` for **UI only** (#9338).
7. **Bounded watch loop.** `await_receive(position)` is **unbounded** — it waits until a payment
   arrives, so a user who never pays would hang the task. Wrap it:
   `match timeout(DEPOSIT_WATCH_TIMEOUT, module.await_receive(position)).await`
   - **timeout (`Elapsed`)** → stay in `AddressShown`, **persist nothing new** (do not advance
     `next_position`), sleep a small exponential backoff (1s → 60s), and re-enter
     `await_receive(position)`.
   - **`Ok((final_state, next_pos))`** → persist `next_position = next_pos`, then **match on
     `FinalReceiveOperationState`**:
     - `Success` → `claim_bind(federation_id, ReceiveMeta.outpoint, R.id)`:
       - `Bound` → persist `gross_deposited`, `operation_id`, `funding_outpoint`, `claim_fee =
         ReceiveMeta.fee`, `net_credit = gross − claim_fee`, `state = Done`.
       - `TakenBy(other)` → the adoption pass already recorded this claim on row `other`; set R
         `Done` **without** inserting a second claim row and **without** a second credit (R's credit
         fields stay empty; the credit lives on `other`).
     - `Aborted` → the claim was rejected; the still-unspent output is reprocessed under a new
       receive operation. Do **not** set `Done` and do **not** credit. Persist `next_position`, set
       `last_error = "claim aborted"`, keep `state = AddressShown`, and re-enter
       `await_receive(next_position)` with bounded retry/backoff. (`await_receive` normally loops
       over aborted claims internally, so this branch is defensive.)
   - **Errors** → classify: timeouts/transport/federation errors are transient (stay
     `AddressShown`); a permanent error (e.g. the federation dropped the wallet module) moves the
     row to `Failed` and releases the invariant.
8. **Boot reconciliation (mandatory), two passes:**
   a. **Resume open rows:** for every row in `AwaitingAddress`/`AddressShown`, resume the address/
      claim steps from **`COALESCE(next_position, position)`** — `await_receive` advances past a
      rejected claim, so resuming from raw `position` can re-read an old claim.
   b. **Adopt orphan claims:** scan the operation log for every
      `WalletOperationMeta::Receive(ReceiveMeta { outpoint, .. })` whose final state is
      `FinalReceiveOperationState::Success`. For each, call
      `claim_bind(federation_id, outpoint, adopted_row_id)`, inserting the `origin = Adopted`,
      `state = Done` row in the **same `dbtx`**:
      - `Bound` → the adopted row is inserted and credited (`operation_id`, `funding_outpoint`,
        `txid`, `gross_deposited`, `claim_fee`, `net_credit`).
      - `TakenBy(R.id)` → adoption loses: **do not insert** a second row; bind to R and, if R is
        still open (`AddressShown`), close R as `Done` with the claim figures (only if R's credit
        fields are unset; never overwrite an existing credit).
      This records (i) a payment that lands after `cancel_deposit`/`Failed`, and (ii) the **second
      output of a double-payment**. Run this pass at boot **and** on a periodic timer / after each
      claim settles, because a second payment can arrive while the app is running. Never adopt an
      `Aborted` claim.

   **Invariant:** every claim in the operation log is represented by **exactly one**
   `deposit_requests` row, keyed `(federation_id, funding_outpoint)`; adoption is idempotent via
   `deposit_requests_claim`. Both writers go through `claim_bind` (§4.4); the loser gets `TakenBy`
   and must not insert or credit again. `Cancelled`/`Failed` rows are terminal and never block the
   next request; an `Adopted` row is `Done` and does not hold the single-flight slot.

   **Double-payment behaviour (chosen):** a second payment to the same address is a second funding
   output → a second claim → a **second row**. The original request row records the first claim and
   reaches `Done`; the second claim is adopted as `origin = Adopted`, `state = Done`, with the same
   address and its own `funding_outpoint`/`txid`/credit. Both credits are recorded; neither is
   dropped. `await_receive(position)` returns only the first claim, so the second is never observed
   by the request's watch loop — reconciliation is what records it.

```rust
// ---- request_deposit: caller gets the open row immediately; this part runs in background ----
if let Some(existing) = journal.open_deposit(federation_id).await? {
    return Ok(existing);                   // single-flight; same address under #9101
}
let position = client.get_next_event_log_id().await;
let id = new_deposit_id();                 // client ULID, not derived from position
journal.insert_deposit(DepositRow {
    id, federation_id, position, amount,
    state: DepositState::AwaitingAddress, ..Default::default()
}).await?;

let mut attempts = 0;
const MAX_ADDRESS_ATTEMPTS: u32 = 5;
let address = loop {
    match timeout(Duration::from_secs(120), module.receive()).await {
        Ok(addr) => break addr,            // receive() returns Address directly (no Result)
        Err(_elapsed) => {
            attempts += 1;
            if attempts >= MAX_ADDRESS_ATTEMPTS {
                journal.fail_deposit(id, "receive() timed out"); // Failed; releases invariant
                return Ok(());
            }
            // stay AwaitingAddress; see #9205
        }
    }
};
journal.set_deposit_address(id, &address, DepositState::AddressShown).await?;
// The background address task stops here. Funding is a separate caller action.

// ---- fund_deposit(id): caller-invoked; quote -> show net -> fund ----
if let Some(funding_txid) = journal.deposit_funding_txid(id).await? {
    return Ok(funding_txid);               // idempotent: never build a second tx
}
let quoted_fee = module.receive_fee().await?;   // quote BEFORE funding
journal.set_deposit_quoted_fee(id, quoted_fee).await?;
show_net(amount, quoted_fee);
let mut psbt = /* bdk_wallet.build_tx() paying `address` `amount` */;
bdk_wallet.sign_with_signers(&mut psbt, /* signers */, /* options */)?;
bdk_wallet.finalize_psbt(&mut psbt, /* options */)?;
let funding_txid = broadcast_via_chain_source(&psbt)?;
journal.set_deposit_funding_txid(id, funding_txid).await?;

// ---- watcher: bounded loop; a non-paying user must not hang the task ----
//      DEPOSIT_WATCH_TIMEOUT is a liveness bound, not a semantic deadline.
const DEPOSIT_WATCH_TIMEOUT: Duration = Duration::from_secs(600);
let mut backoff = Duration::from_secs(1);
let (final_state, next_position) = loop {
    match timeout(DEPOSIT_WATCH_TIMEOUT, module.await_receive(position)).await {
        Ok(Ok((state, next))) => break (state, next),
        Ok(Err(err)) => match classify_error(&err) {   // same rules as §4.3
            ErrorClass::Transient => {
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;                  // same position; stay AddressShown
            }
            ErrorClass::Permanent => {
                journal.fail_deposit(id, &err).await?; // Failed; releases invariant
                return Ok(());
            }
        },
        Err(_elapsed) => {
            sleep(backoff).await;          // small exponential backoff
            backoff = (backoff * 2).min(Duration::from_secs(60));
            continue;                      // stay AddressShown; persist nothing new
        }
    }
};
journal.set_deposit_next_position(id, next_position).await?;
match final_state {
    FinalReceiveOperationState::Success => {
        // outpoint/gross/claim_fee from ReceiveMeta.outpoint / .value / .fee
        let (outpoint, gross, claim_fee) = claim_figures(id).await?;
        match journal.claim_bind(federation_id, outpoint, id).await? {
            BindResult::Bound => {
                journal.finish_deposit(id, outpoint, gross, claim_fee).await?; // Done + one credit
            }
            BindResult::TakenBy(other) => {
                // adoption already credited this outpoint on `other`
                journal.close_deposit_done(id).await?; // Done; no second row / no second credit
            }
        }
    }
    FinalReceiveOperationState::Aborted => {
        // rejected claim: reprocessed under a new receive op; never credit here
        journal.set_deposit_aborted(id, next_position).await?; // stays AddressShown
        // re-enter await_receive(next_position) with bounded backoff
    }
}

// ---- boot reconciliation (mandatory): resume open rows, then adopt orphan claims ----
for row in journal.deposits_in(&[DepositState::AwaitingAddress, DepositState::AddressShown]).await? {
    let cursor = row.next_position.unwrap_or(row.position.expect("requested rows have a position"));
    resume_deposit_watch(row.id, cursor).await;      // COALESCE(next_position, position)
}
for claim in scan_receive_claims(&client).await? {
    if claim.final_state != FinalReceiveOperationState::Success { continue; }
    let Some(outpoint) = claim.meta.outpoint else { continue; };
    // insert-if-absent + claim_bind in one dbtx; rolls back on TakenBy
    match journal.claim_adopt_if_absent(federation_id, outpoint, AdoptedClaim {
        address: claim.meta.address, operation_id: claim.operation_id,
        value: claim.meta.value, fee: claim.meta.fee,
    }).await? {
        BindResult::Bound => {}                    // adopted + credited
        BindResult::TakenBy(row) => {
            // adoption lost: no second row; close the open owner if it has no credit yet
            journal.close_deposit_done_if_open(row, claim.meta.value, claim.meta.fee).await?;
        }
    }
}
```

### 4.2 Withdrawal — op-id-attributed, initiated

A withdrawal is an operation **we initiate**; the federation assigns its `OperationId` in `send()`.

Steps:

1. `let address = bdk_wallet.reveal_next_address(KeychainKind::External).address;`
   Persist `{ id, federation_id, address, value, state = Quoting }`.
2. Quote: `let chain_fee = module.send_fee().await?;` and
   `let quote = module.send_fee_quote(value).await?;` (`value: bitcoin::Amount`).
   Show `value`, `chain_fee`, the `FeeQuote`, and the **net/total debit**. For a sweep use
   `max_sendable_amount(balance, fee)`.
3. **Re-quote immediately before `send()`** (#9098): call `send_fee()` and `send_fee_quote(value)`
   again, compare with the persisted `chain_fee`/`quote`, and return a `QuoteChanged` error on any
   mismatch — do **not** call `send()` on drift.
4. `let operation_id = module.send(address, value, fee, custom_meta).await?;`
   (`WalletClientModule::send(&self, address: Address<NetworkUnchecked>, value: bitcoin::Amount,
   fee: Option<bitcoin::Amount>, custom_meta: serde_json::Value)
   -> Result<OperationId, SendError>`)
   Persist `operation_id` **immediately** and set `state = Submitted`. Put our idempotency key into
   `custom_meta` (`{"id": "<id>"}`) so `SendMeta.custom_meta` carries it through the operation log.
5. Await:
   `match module.await_final_send_operation_state(operation_id).await? { … }`
   - `FinalSendOperationState::Success(txid)` → persist `txid`, `state = Broadcast`
   - `FinalSendOperationState::Aborted` → `state = Refunded` (funding rejected; value is spendable
     again — **re-credit**)
   - `FinalSendOperationState::Failure` → `state = Failed` (programming error / malicious
     federation; surface an error, do not silently retry)
6. **On boot:** for every non-terminal row with an `operation_id`, re-await
   `await_final_send_operation_state(operation_id)`. **Never re-call `send()`** for the same `id`;
   the persisted `operation_id` is the dedup anchor.

```rust
// 1. destination + row
let bdk_address = bdk.reveal_next_address(KeychainKind::External).address;
journal.insert_withdrawal(WithdrawalRow {
    id, federation_id, address: bdk_address.to_string(), value,
    state: WithdrawalState::Quoting, ..Default::default()
}).await?;

// 2. quote
let chain_fee = module.send_fee().await?;                 // bitcoin::Amount
let quote = module.send_fee_quote(value).await?;          // FeeQuote
ui.show_net(value, chain_fee, &quote);

// 3. re-quote immediately before send(); abort on drift (#9098)
let fresh_fee = module.send_fee().await?;
let fresh_quote = module.send_fee_quote(value).await?;
if fresh_fee != chain_fee || fresh_quote != quote {
    return Err(PegError::QuoteChanged { quoted: quote, current: fresh_quote });
}

// 4. submit once; persist the operation id immediately
// BDK gives a checked address; send() takes Address<NetworkUnchecked>.
let destination: bitcoin::Address<bitcoin::address::NetworkUnchecked> =
    bdk_address.into_unchecked();
let custom_meta = serde_json::json!({ "id": id });
let operation_id = module
    .send(destination, value, Some(fresh_fee), custom_meta)
    .await?;
journal.set_withdrawal_operation_id(id, operation_id, WithdrawalState::Submitted).await?;

// 5. final state
match module.await_final_send_operation_state(operation_id).await? {
    FinalSendOperationState::Success(txid) => journal.set_broadcast(id, txid).await?,
    FinalSendOperationState::Aborted        => journal.set_refunded(id).await?,
    FinalSendOperationState::Failure        => journal.set_failed(id).await?,
}
```

Related walletv2 methods (VERIFY.md §8.2): `receive_fee() -> Result<bitcoin::Amount, ReceiveError>`,
`send_fee() -> Result<bitcoin::Amount, SendError>`,
`send_fee_quote(amount: bitcoin::Amount) -> anyhow::Result<FeeQuote>`,
`max_sendable_amount(balance, fee) -> anyhow::Result<bitcoin::Amount>`,
`tx_chain() -> FederationResult<Vec<TxInfo>>`,
`pending_tx_chain() -> FederationResult<Vec<TxInfo>>`.

**Dust and fee limits come from `WalletClientConfig`.** `dust_limit` drives `SendError::DustValue`
and `max_sendable_amount(...)`, so a wrong value makes small withdrawals fail. The config is decoded
from the federation config (`client.config().get_module_cfg(id).cast::<WalletClientConfig>()`,
VERIFY §8.5); `WalletClientInit::default()` is assumed and `WalletClientConfig`
construction/defaults are **UNVERIFIED** (§8.10). Validate `value >= dust_limit` against the decoded
config before `send()`.

### 4.3 State enums

```rust
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DepositOrigin {
    /// Created by `request_deposit`.
    Requested,
    /// Created by boot reconciliation for an operation-log claim with no row.
    Adopted,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DepositState {
    /// Row inserted; `position` persisted; `receive()` has not returned an address yet.
    AwaitingAddress,
    /// Address persisted *before* display; `await_receive(position)` is in flight.
    AddressShown,
    /// `FinalReceiveOperationState::Success`.
    Done,
    /// User cancelled from `AwaitingAddress`/`AddressShown` while `funding_txid IS NULL`.
    Cancelled,
    /// Terminal: address retries exhausted or a permanent error. Releases the invariant.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WithdrawalState {
    /// Destination revealed + persisted; quote not yet executed.
    Quoting,
    /// `send()` returned an `OperationId`; it is persisted.
    Submitted,
    /// `FinalSendOperationState::Success(txid)` — federation broadcast the withdrawal.
    Broadcast { txid: bitcoin::Txid },
    /// `FinalSendOperationState::Aborted` — funding rejected, value spendable again.
    Refunded,
    /// `FinalSendOperationState::Failure`, or a fatal `SendError` from `send()`.
    Failed,
    /// User cancelled from `Quoting` only.
    Cancelled,
}
```

Transient errors do **not** advance the state: the row stays `AwaitingAddress`/`AddressShown` or
`Quoting`/`Submitted` and is retried on boot.

**Terminal vs retryable (explicit).**
- *Retryable*: timeouts, transport/federation-unreachable errors, `TryAgain`/storage-busy, and
  `FinalReceiveOperationState::Aborted` claims (reprocessed under a new receive op).
- *Terminal (deposit)*: `receive()` exhausted `MAX_ADDRESS_ATTEMPTS`, or a permanent error (e.g.
  the federation dropped the wallet module) → `Failed`; user action → `Cancelled` (only from
  `AwaitingAddress`/`AddressShown` while `funding_txid IS NULL`).
- *Terminal (withdrawal)*: a fatal `SendError` from `send()` — `WrongNetwork`, `DustValue`,
  `UnsupportedAddress`, `InsufficientFunds` — or `FinalSendOperationState::Failure` → `Failed`;
  user action from `Quoting` only → `Cancelled`. `NoConsensusFeerateAvailable` and
  `FederationError(_)` are transient and stay `Quoting` for a boot retry.
- `Cancelled`/`Failed` fall outside the partial-unique indexes, so they release the single-flight
  invariant and unblock the next request.

`classify_error(&err) -> ErrorClass::{Transient, Permanent}` implements these rules; the deposit
watcher and the withdrawal `send()` path both use it (they never propagate a raw error past the
classification).

### 4.4 Persistence schema

**The DDL below is illustrative.** The real journal is not SQL: it is prefixed KV rows in
fedimint's `Database` (prefix `[0x00]`) with a `serde_json` envelope (`ROW_VERSION = 1`, per
douglaz `FedimintJournal`). The partial-unique invariants (`deposit_requests_one_open`,
`withdrawal_requests_one_open`) are enforced **in code, inside the same `dbtx`** as the insert and
the close — not by a database engine. The SQL is shown only to make the constraints explicit.

```sql
CREATE TABLE deposit_requests (
    id              BLOB PRIMARY KEY,          -- client-generated idempotency key (UUIDv7/ULID)
    federation_id   BLOB NOT NULL,
    origin          TEXT NOT NULL,             -- Requested | Adopted
    position        INTEGER,                   -- EventLogId read BEFORE receive(); NULL for Adopted
    amount          INTEGER NOT NULL,          -- sats the caller intends to deposit (gross for Adopted)
    address         TEXT,                      -- set when state = AddressShown (from ReceiveMeta for Adopted)
    state           TEXT NOT NULL,             -- AwaitingAddress | AddressShown | Done | Cancelled | Failed
    next_position   INTEGER,                   -- returned by await_receive()
    operation_id    BLOB,                      -- claim op learned via ReceiveMeta / event
    funding_outpoint BLOB,                     -- ReceiveMeta.outpoint: the claim's funding output
    funding_txid    BLOB,                      -- BDK peg-in transaction (Requested only)
    quoted_fee      INTEGER,                   -- receive_fee() quote, sats
    claim_fee       INTEGER,                   -- actual ReceiveMeta.fee, sats
    gross_deposited INTEGER,                   -- sats (bitcoin::Amount)
    net_credit      INTEGER,                   -- msat (fedimint_core::Amount)
    last_error      TEXT,
    created_at      INTEGER NOT NULL,          -- unix ms
    updated_at      INTEGER NOT NULL,          -- unix ms
    -- NO UNIQUE(federation_id, position): get_next_event_log_id() can repeat
);
-- At most ONE open deposit per federation (enforced in the same dbtx as insert/close):
CREATE UNIQUE INDEX deposit_requests_one_open
    ON deposit_requests (federation_id)
    WHERE state IN ('AwaitingAddress', 'AddressShown');
-- Exactly one row per claim (every operation-log claim is represented once):
CREATE UNIQUE INDEX deposit_requests_claim
    ON deposit_requests (federation_id, funding_outpoint)
    WHERE funding_outpoint IS NOT NULL;
CREATE INDEX deposit_requests_state ON deposit_requests (state);

CREATE TABLE withdrawal_requests (
    id              BLOB PRIMARY KEY,          -- client-generated idempotency key (UUIDv7/ULID)
    federation_id   BLOB NOT NULL,
    address         TEXT NOT NULL,             -- BDK reveal_next_address destination
    value           INTEGER NOT NULL,          -- sats (bitcoin::Amount)
    fee             INTEGER,                   -- send_fee() result, sats
    quote           BLOB,                      -- serialized FeeQuote
    operation_id    BLOB UNIQUE,               -- set immediately after send()
    state           TEXT NOT NULL,             -- Quoting | Submitted | Broadcast | Refunded | Failed | Cancelled
    txid            BLOB,                      -- set when state = Broadcast
    custom_meta     TEXT NOT NULL,             -- {"id": "<id>"} passed to send()
    last_error      TEXT,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE INDEX withdrawal_requests_state ON withdrawal_requests (state);
CREATE INDEX withdrawal_requests_fed_state ON withdrawal_requests (federation_id, state);
-- At most ONE in-flight withdrawal per federation (same dbtx as insert/close):
CREATE UNIQUE INDEX withdrawal_requests_one_open
    ON withdrawal_requests (federation_id)
    WHERE state IN ('Quoting', 'Submitted');
```

`get_next_event_log_id()` is **not** a unique key; the `deposit_requests_one_open` invariant is
enforced in the journal code. A new deposit row may be inserted only in the same `dbtx` that closes
the previous open row. `deposit_requests_claim` enforces **exactly one row per claim**
(`federation_id`, `funding_outpoint`), so orphan adoption is an insert-if-absent on that key.
Likewise `withdrawal_requests_one_open` serializes our own sends per federation (at most one
`Quoting`/`Submitted` row). `Broadcast { txid }` is stored as `state='Broadcast'` plus the `txid`
column. Every state transition writes the row and its pending-index entry in one `dbtx`
(`begin_transaction … commit_tx`), so a crash cannot leave a row indexed under a state it no longer
holds.

**Conflict-safe claim binding (`claim_bind`).** The watch loop and the adoption pass are both
writers of the claim key, so both go through one primitive:
`claim_bind(federation_id, funding_outpoint, row_id) -> BindResult::{Bound, TakenBy(row_id)}`,
implemented as an insert-if-absent on `(federation_id, funding_outpoint)` inside one `dbtx`
(`deposit_requests_claim` is the durable backstop):
- key absent → bind the outpoint to `row_id`, return `Bound`;
- key already maps to `row_id` → `Bound` (idempotent);
- key maps to another row → `TakenBy(existing)`; the caller must **not** insert a second claim row.

**Invariant:** credit is keyed by `(federation_id, funding_outpoint)` — **exactly one credit row
per outpoint**, no matter which path records it. An unbound open row (`AddressShown` with
`funding_outpoint IS NULL`) is not a credit; it must still be closed to `Done` when its claim is
adopted elsewhere.

### 4.5 Idempotency keys

- **Deposit `id`** = client-generated UUIDv7/ULID. `position` is an observation cursor, **not** a
  unique key: `get_next_event_log_id()` returns the same value until something is written, and
  `receive()` writes no event, so two requests can share a position. The idempotency fence is the
  **single open deposit per federation**: `request_deposit` returns the existing
  `AwaitingAddress`/`AddressShown` row instead of inserting a new one. A new row may be inserted
  only in the same `dbtx` that closes the previous open row (partial unique index in §4.4).
  Address retries (step 3 timeouts) reuse the same row. `cancel_deposit` (from
  `AwaitingAddress`/`AddressShown` with no `funding_txid`) and `Failed` set a terminal state
  outside the index and release the invariant. **Concurrent deposits are unsupported until #9306
  is in the pin.**
- **Claim invariant / orphan adoption:** every operation-log claim is represented by exactly one
  row, keyed `(federation_id, funding_outpoint)`. Both the watch loop and the adoption pass write
  through `claim_bind` (§4.4): `Bound` owns the credit; `TakenBy(other)` means the other path
  already owns it, so the open row is closed `Done` **without** a second claim row or a second
  credit. Adoption inserts an `origin = Adopted`, `state = Done` row only when `Bound`; on
  `TakenBy` it rolls back its insert and closes the open owner. A double-payment to the same
  address produces two claims and therefore **two rows** (the original request row + one adopted
  row), both credited; the second is never observed by `await_receive(position)`.
- **Withdrawal `id`** = client-generated UUIDv7/ULID, persisted before `send()`. `send()` is called
  **at most once per `id`**; the `Quoting → Submitted` transition is the fence. `operation_id` is
  persisted immediately and is `UNIQUE`; on boot we re-await by it and never re-submit. The same
  `id` is embedded in `custom_meta` so `SendMeta.custom_meta` carries the correlation through the
  federation operation log. One in-flight withdrawal per federation is enforced by
  `withdrawal_requests_one_open` (`Quoting`/`Submitted`), so our own sends cannot race each other;
  `cancel_withdrawal` (from `Quoting`) and a fatal `SendError → Failed` release it.
- **Crash window** (between `send()` returning and `operation_id` being persisted): on boot, do
  **not** call `send()` again until the reconciliation scan has **completed cleanly at least once**.
  Run a bounded retry loop (e.g. `MAX_SCAN_ATTEMPTS = 5`, exponential backoff) over the operation
  log for a `SendMeta` whose `custom_meta.id` matches the row's `id`:
  - `Ok(Some(op))` → adopt `op`, persist it as `Submitted`, continue at step 5.
  - `Ok(None)` after a clean, complete scan → `send()` did not commit; **only now** may the row be
    re-quoted and re-sent under the same `id`.
  - `Err(_)` / an incomplete scan is **inconclusive**: keep the row `Quoting`/`Submitted` and retry
    the scan later — **never re-send on an inconclusive scan**.

  ```rust
  async fn reconcile_before_resend(id: IdempotencyKey) -> Result<Option<OperationId>> {
      let mut backoff = Duration::from_secs(1);
      for _ in 0..MAX_SCAN_ATTEMPTS {
          match scan_for_send(id).await {
              Ok(Some(op)) => return Ok(Some(op)),  // adopt; never re-send
              Ok(None) => return Ok(None),          // clean, complete scan: safe to re-send
              Err(_) => {
                  sleep(backoff).await;
                  backoff = (backoff * 2).min(Duration::from_secs(60));
              }
          }
      }
      anyhow::bail!("reconciliation scan inconclusive; refusing to re-send")
  }
  ```

### 4.6 Known hazards

| Issue | Hazard | Mitigation in this design |
|---|---|---|
| `fedimint/fedimint#9101` | `receive()` returns only an address (no `Result`, no `OperationId`); repeated calls return the same address until the scanner sees it paid; there is no durable pre-payment operation. Correlating a payment to a UI request by address is unsafe with concurrent receives. | Attribute the deposit to the **`EventLogId` read before `receive()`** (`Client::get_next_event_log_id()`), persist the row **before** `receive()`, and observe with `await_receive(position)` (position-scoped). Enforce **one open deposit per federation** and return the existing row; never insert a second row against a possibly-repeated position. Never use the address as the correlation key. **Concurrent deposits are unsupported until #9306 is in the pin** (fixed on `main`; not in 0.12.0). |
| `fedimint/fedimint#9205` | Walletv2 client panics on first start when the address search outlives the DB write history (first-address derivation is a hash-prefix search that can take a long time). | Bound `receive()` at **120 s** and retry on timeout up to `MAX_ADDRESS_ATTEMPTS`; the row stays `AwaitingAddress` while retrying, then moves to `Failed` (releasing the single-flight invariant) if the budget is exhausted. Let the client/scanner run before the first `receive()`; never hold a DB transaction across the address search. |
| `fedimint/fedimint#9338` | The claim is committed in one DB transaction and its event-log announcement in another; a client stopping between them keeps the claim without the `ReceivePaymentEvent`. An event-only subscriber can miss the deposit. | `await_receive(position)` is the source of truth, not the event. On boot, **also reconcile via module operations**: scan the operation log for `WalletOperationMeta::Receive(ReceiveMeta { outpoint, .. })` and match the deposit's funding outpoint (fedimint-sdk `find_claim` pattern). `ReceivePaymentEvent` is UI-only. |
| `fedimint/fedimint#9098` | Quote and submit are not atomic; a concurrent spend between the quote and the funding can change what is selected, so the quoted fee/total may not match what is debited. | Re-quote immediately before `send()` (`send_fee()` + `send_fee_quote()`); compare with the persisted quote and abort with a `QuoteChanged`-style error on any drift; never silently absorb the difference. Serialize withdrawals per federation (single-flight) so our own sends cannot race each other. |

### 4.7 Testing

- **Local federation: devimint.** Bring up a federation with the repo's devimint harness; run the
  on-chain flow on **regtest first**, then **signet** with small amounts. Do not use mainnet.
- **Deposit:** assert a second `request_deposit` while a row is `AwaitingAddress`/`AddressShown`
  returns the same row/address (single-flight) and never inserts a second open row; crash/restart
  at each transition (`AwaitingAddress → AddressShown → Done`); kill between the claim commit and
  its event-log entry (#9338) and assert boot reconciliation credits exactly once; assert
  `net_credit = gross_deposited − claim_fee` and that `receive_fee()` was applied.
- **Withdrawal:** move the feerate between `send_fee_quote()` and `send()` to exercise quote drift
  (#9098) and assert the send is refused/re-quoted; crash after `send()` but before `operation_id`
  is persisted and assert boot reconciliation adopts the operation instead of re-sending; assert
  `Aborted → Refunded` re-credits exactly once.
- **Funding idempotency:** call `fund_deposit(id)` twice and assert only one BDK transaction exists
  and the same `funding_txid` is returned.
- **Unbounded wait:** let a deposit go unpaid past `DEPOSIT_WATCH_TIMEOUT` and assert the row stays
  `AddressShown`, nothing new is persisted, and the task re-enters `await_receive(position)`.
- **Withdrawal single-flight:** attempt a second withdrawal while one row is `Quoting`/`Submitted`
  and assert the open-row invariant refuses it.
- **Deposit liveness:** exhaust `MAX_ADDRESS_ATTEMPTS` and assert `Failed` releases the invariant
  (a new deposit can be created); `cancel_deposit` from `AddressShown` with no `funding_txid`
  releases it; `cancel_deposit` after funding is refused.
- **Withdrawal liveness:** a fatal `SendError` from `send()` moves `Quoting` → `Failed` and releases
  the invariant; a transient `FederationError` stays `Quoting` and retries on boot;
  `cancel_withdrawal` from `Quoting` releases it and is refused from `Submitted`.
- **Orphan claims:** pay after `cancel_deposit`/`Failed` and assert the next reconciliation pass
  inserts an `origin = Adopted`, `state = Done` row and records the credit exactly once; repeated
  passes are idempotent (`deposit_requests_claim`).
- **Double payment:** send two outputs to the same address; assert the first closes the request row
  as `Done` and the second is adopted as a separate `Done` row, with both credits recorded and no
  duplicate row on re-scan.
- **Adoption/watch race:** run the periodic adoption scan concurrently with a settling watch loop
  for the same claim (same `(federation_id, funding_outpoint)`); repeat to exercise both
  interleavings; assert exactly one credit row, the open row closes `Done` (not stuck
  `AddressShown`), and the single-flight slot is released.
- **Balance:** blocked by §8.2 (primary module for BTC) — do not assert balance movements until
  that is resolved.

---

## 5. Concurrent sync tasks with crash-safe persistence

Two long-running tokio tasks (plus the fedimint client's internal executor/reconnect tasks):

### Task A — BDK chain sync (`sync::bdk_chain_task`)

- Loop: call chain source → obtain an `Update`. For `bdk_esplora` use
  `bdk_esplora::EsploraAsyncExt::sync` (partial, from `Wallet::start_sync_with_revealed_spks()`) or
  `EsploraAsyncExt::full_scan` (full, from `Wallet::start_full_scan()`) — there is **no
  `EsploraClient` type and no `scan` method**. Then `wallet.apply_update(update)` →
  `wallet.take_staged()` → persist ChangeSet via `WalletPersister` (rusqlite or bdk_file_store).
- Detect the withdrawal arriving at a BDK address: `wallet.get_tx(txid)`, `wallet.list_unspent()`,
  `WalletEvent::TxConfirmed` via `apply_update_events`. The federation-side `txid` comes from
  `FinalSendOperationState::Success(txid)`, not from BDK.
- Withdrawal completion is owned by `await_final_send_operation_state`; BDK sync only records that
  the on-chain funds arrived at the destination.

### Task B — Fedimint federation sync (`sync::fedimint_sync_task`)

- `client.spawn_federation_reconnect()` (keeps guardian connections warm).
- Drive state machines: `client.start_executor()` (executor runs all module state machines in the
  background).
- React to events: `client.handle_events(tracker, handler_fn)` or
  `client.get_event_log_transient_receiver()` — filter the walletv2 `ReceivePaymentEvent` /
  `ReceivePaymentUpdateEvent` and `SendPaymentEvent` / `SendPaymentUpdateEvent` for UI/progress.
  **Deposits are observed to completion with `await_receive(position)`; withdrawals with
  `await_final_send_operation_state(operation_id)`** — events are supplementary.
- Balance stream: `client.subscribe_balance_changes(unit)` — **blocked by §8.2** (primary module for BTC unverified).

### Crash-safe persistence (douglaz patterns, `journal.rs`)

- App journal under `Database` prefix `[0x00]`; fedimint clients under `[0x01] ++ u32_le(db_prefix)` — **separate partitions**.
- Raw byte API + serde_json versioned envelopes (`ROW_VERSION = 1`): rows keyed by `[tag] ++ id_bytes` (`0x01 Intent`, `0x02 MoveRecord`, `0x03 FederationInfo`, `0x04 PendingIndex`, `0x05 LedgerRow`, …).
- **Row + pending index committed in ONE `dbtx`:** `begin_transaction … commit_tx` — a state change removes the old index entry and inserts the new one atomically.
- `Journal::pending` / `Journal::failed` read index + rows from ONE `begin_transaction_nc` snapshot.
- Every money intent gets an **idempotency key** and a **deterministic correlation id** (deposit: `position`; withdrawal: client ULID).
- `FederationInfo` registry row (`invite`, `db_prefix`, `joined_at`) persisted so restart can reopen existing partitions.
- Recovery: `ClientPreview::recover(...)` then `wait_for_all_recoveries()`; shutdown the recovery client and reopen before reading balance.
- A `join_lock` (`Mutex<()>`) + `active_recoveries` reservation serialize prefix allocation.
- `open_all` is best-effort: one un-openable federation is warn-logged and skipped.

---

## 6. Thin UniFFI crate (`pegwallet-ffi`)

`uniffi = "0.32.0"`.

- `#[uniffi::export]` a `PegWallet` ABI object (must be `Send + Sync + 'static`; internally `Arc<PegWalletCore>`).
- Methods:
  - `join_federation(invite_code: String) -> Result<String>`
  - `balance_sat() -> u64` — **blocked by §8.2** (primary module for BTC unverified).
  - `request_deposit_address() -> Result<DepositView>` — **non-blocking**: inserts/returns the single open `deposit_requests` row in `AwaitingAddress` and returns immediately. A background task runs `receive()` (120 s bound + retry) and performs the `AddressShown` write.
  - `fund_deposit(id: String) -> Result<String>` — **idempotent**: if `funding_txid` is already set, returns it without building a second BDK transaction; otherwise builds/signs/broadcasts the peg-in transaction and returns its txid. A BDK balance below `amount` surfaces the `build_tx` error and leaves the row `AddressShown`.
  - `deposit_status(id: String) -> Result<DepositView>` — current state, address (null until `AddressShown`), quoted fee, gross, net credit, funding txid.
  - `cancel_deposit(id: String) -> Result<DepositView>` — allowed only from `AwaitingAddress`/`AddressShown` while `funding_txid` is null; sets `Cancelled` and releases the single-flight invariant.
  - `withdraw(address: String, sat_amount: u64) -> Result<String>` (creates a `withdrawal_requests` row, returns its id)
  - `withdrawal_status(id: String) -> Result<WithdrawalView>` (state + operation_id + txid)
  - `cancel_withdrawal(id: String) -> Result<WithdrawalView>` — allowed only from `Quoting`; sets `Cancelled` and releases the invariant.
  - `sync() -> Result<()>`
- Streams: expose a polling `list_deposits()` / `list_withdrawals()` plus a callback subscription; map `anyhow::Error` to a `thiserror`-based `pegwallet_ffi::Error` (uniffi cannot expose `anyhow::Error`).

Note: `request_deposit_address()` does **not** wait up to 120 s. It returns the `AwaitingAddress`
row at once; the UI polls `deposit_status(id)` (or subscribes) until the address is available. The
address is shown only after the `AddressShown` write commits. BDK funding is a separate
`fund_deposit(id)` call so it cannot block the address request.

**Units at the FFI boundary.** `balance_sat()` returns **sats**; `DepositView.net_credit_msat`
returns **msat** (`fedimint_core::Amount`). 1 sat = 1000 msat. Convert exactly with
`fedimint_core::Amount::from_sats(gross_sats - claim_fee_sats)` (×1000) and read with `.msats()`;
never round `net_credit` down to sats for accounting. `balance_sat()` is display-only:
`balance_msat / 1000` (floor). The remainder (`balance_msat % 1000`) stays in the federation
balance — it is not discarded, not rounded into the next send, and not sent anywhere. Expose
`balance_msat()` if exact accounting is needed.

**Guessing at:** uniffi 0.32 async export support and per-language binding glue; start with blocking
methods + polling, then add async streams once bindings are verified.

---

## 7. Module map (confirmed function/module names)

**fedimint-client (0.12.0)**
- `Client::builder()`, `ClientBuilder::with_module()`, `ClientBuilder::preview()`, `ClientPreview::join()` (**→ `ClientHandle`**)
- `Client::get_balance_for_unit()`, `get_first_module::<M>()`, `get_first_module_arc::<M>()`
- `Client::get_next_event_log_id() -> EventLogId`
- `Client::finalize_and_submit_transaction(op_id, op_type, meta_gen, tx_builder) -> OutPointRange`
- `Client::transaction_updates(op_id) -> TransactionUpdates`
- `Client::await_primary_bitcoin_module_output(s)(...)`
- `Client::subscribe_balance_changes(unit) -> BoxStream<Amount>`
- `Client::spawn_federation_reconnect()`, `start_executor()` (**receiver `self: &Arc<Self>`**), `db()`, `executor()`, `operation_log()`, `handle_events()`, `get_event_log_transient_receiver()`
- `Client::chain_id()`, `federation_id()`, `config()`
- `ClientContext`, `OperationId` = `fedimint_core::core::OperationId`, `OutPointRange` = `fedimint_core::OutPointRange`, `TransactionBuilder` = `fedimint_client::module::transaction::TransactionBuilder`

**fedimint-walletv2-client (0.12.0)** — SUPERSEDES the v1 `fedimint-wallet-client`
- `WalletClientModule::receive() -> Address` (no `Result`, no `OperationId`)
- `WalletClientModule::send(address, value, fee, custom_meta) -> Result<OperationId, SendError>`
- `WalletClientModule::await_receive(position) -> anyhow::Result<(FinalReceiveOperationState, EventLogId)>`
- `WalletClientModule::await_final_send_operation_state(id) -> anyhow::Result<FinalSendOperationState>`
- `WalletClientModule::await_final_receive_operation_state(id) -> anyhow::Result<FinalReceiveOperationState>`
- `receive_fee()`, `send_fee()`, `send_fee_quote(amount)`, `max_sendable_amount(balance, fee)`
- `tx_chain()`, `pending_tx_chain()`
- `WalletClientInit`, `WalletClientConfig`, `WalletOperationMeta`/`SendMeta`/`ReceiveMeta`
- `FinalSendOperationState` (`Success(Txid) | Aborted | Failure`), `FinalReceiveOperationState` (`Success | Aborted`), `WalletClientStateMachines`
- `common::{OutputInfo, TxInfo}`; `events::{ReceivePaymentEvent, ReceivePaymentUpdateEvent, SendPaymentEvent, SendPaymentUpdateEvent}`
- `output_info_slice` is on the private `WalletFederationApi` (not public on the module); use `tx_chain()` / `pending_tx_chain()`. `OUTPUT_INFO_SLICE_ENDPOINT` is public via `common`.

**fedimint-mint-client (0.12.0)**
- `MintClientModule::reissue_external_notes()`, `send_oob_notes()`, `spend_notes_with_selector()`, `create_output()`, `await_output_finalized()`, `get_note_counts_by_denomination()`, `estimate_spend_all_fees()`
- `MintClientInit`

**bdk_wallet (3.2.0)**
- `Wallet::create(desc, change_desc)` (→ `CreateParams`), `Wallet::load()` (→ `LoadParams`)
- `reveal_next_address(KeychainKind::External)`, `next_unused_address`, `list_unspent()`, `get_utxo()`, `get_tx()`
- `build_tx()`, `create_psbt()` (**requires `bdk-tx` + `--cfg bdk_wallet_unstable`**), `sign_with_signers()`, `finalize_psbt()`, `take_staged()`, `apply_update()`, `balance()`, `calculate_fee()`, `insert_txout()`
- `finalize_psbt()` is **not deprecated** in 3.2.0 (source check: the `#[deprecated]` at `wallet/mod.rs:1958` is `policies`). `try_finalize_psbt()` is an additional per-input inspection API (no `SignOptions`, returns `FinalizePsbtOutcome`), not a replacement.
- `start_sync_with_revealed_spks()`, `start_full_scan()`
- `WalletEvent::TxConfirmed` etc. via `apply_update_events()`
- `Update` = `bdk_wallet::Update` (crate root — not `bdk_wallet::chain::Update`)
- Persistence via `bdk_wallet::rusqlite` (`PersistedWallet::create`/`load`) or `bdk_file_store`

**bdk_esplora (0.22.1) — chain source**
- trait `bdk_esplora::EsploraAsyncExt` (blocking: `EsploraExt`), impl'd for `esplora_client::AsyncClient` / `BlockingClient`
- `full_scan(request, stop_gap, parallel_requests)` (full), `sync(request, parallel_requests)` (partial)
- no `EsploraClient` type and no `scan` method; `bdk_esplora::esplora_client` is re-exported

**fedimint-core (0.12.0)**
- `Database::with_prefix(&self, prefix: Vec<u8>) -> Self`
- `AmountUnit` = `fedimint_core::module::AmountUnit`

**douglaz/fedimint-wallets (crash-safe patterns)** — source-derived, not compile-verified
- `MultiClient`, `FedimintJournal`, `runtime::Runtime` as above.

**Not verified (reference only) — fedimint-clientd**
- `AppState::new(db_path)`, `multimint.register_new(invite_code)`, `multimint.all()`; REST routes.

---

## 8. What I'm guessing at (needs verification against source code)

Items VERIFY.md resolved are marked **RESOLVED**; the rest remain unverified.

1. **Wallet-module flow** — **RESOLVED** (VERIFY.md §8): walletv2 uses `receive()` + `await_receive(position)` for deposits and `send()` + `await_final_send_operation_state(id)` for withdrawals. There is no `peg_in`/`allocate_deposit_address_pooled`/`withdraw` in the v2 module.
2. **Whether `WalletClientModule` or `MintClientModule` is the primary module for BTC** — **BLOCKING for the balance implementation**: `get_balance_for_unit(AmountUnit::Bitcoin)` and `subscribe_balance_changes(unit)` need a primary module, and both modules claim `supports_being_primary`. Do not implement balance until this is resolved against the source/config.
3. **`ClientBuilder::open` vs `join` signatures** — `join` is confirmed: `ClientPreview::join(self, db_no_decoders: Database, pre_root_secret: RootSecret) -> Result<ClientHandle>`; `preview_with_existing_config` is confirmed. The `ClientBuilder::open` restore flow is **unverified** (VERIFY.md did not exercise it).
4. **BDK persistence call shape** — **RESOLVED** (VERIFY.md §1): `PersistedWallet::create(&mut P, CreateParams) -> Result<Self, CreateWithPersistError<P::Error>>` and `PersistedWallet::load(&mut P, LoadParams) -> Result<Option<Self>, LoadWithPersistError<P::Error>>`.
5. **Chain source choice** — esplora/electrum/bitcoind-rpc; a product decision. `bdk_esplora::EsploraAsyncExt::sync`/`full_scan` is the verified esplora path.
6. **UniFFI async** — uniffi 0.32 async export support and per-language binding generation **unverified**.
7. **Depending on douglaz's `wallet_core`/`wallet-fedimint`** — AGPL-3.0, not on crates.io (git dep at a pinned commit). Either re-implement a small journal inline or take the git dep with the license consequence.
8. **`tx_builder` construction for fedimint transactions** — **RESOLVED** for walletv2: `send()` is the high-level wrapper that builds/submits internally and returns an `OperationId`; no manual `TransactionBuilder` is needed for peg-out.
9. **Recovery path (§5)** — `ClientPreview::recover` + `Client::wait_for_all_recoveries()` is on the boot critical path but **unverified**: VERIFY.md did not exercise a restore/recovery. The step that shuts down the recovery client and reopens the partition before reading balance is unconfirmed; treat it as blocking a production boot/restore implementation.
10. **`WalletClientConfig` construction/defaults** — **UNVERIFIED**: `WalletClientInit::default()` is assumed. `WalletClientConfig` carries `dust_limit` and `fee_consensus`; `dust_limit` drives `SendError::DustValue` and `max_sendable_amount(...)`, so a wrong default makes small withdrawals fail. Verify the decoded config path (`client.config().get_module_cfg(id).cast::<WalletClientConfig>()`) and how the module is constructed before implementing peg-out dust checks.
11. **`ReceiveMeta` full field set** — **UNVERIFIED** beyond `outpoint` and `fee` (VERIFY §8.3/§8.5 exercise those two): the adopted-row path also reads `address`, `value`, and `change_outpoint_range`. Confirm the exact `ReceiveMeta` fields on the pinned 0.12.0 before relying on them; orphan adoption depends on `outpoint` (dedup key), `value` (gross), `fee` (claim fee), and `address` (display).
12. **Deposit detection without a local bitcoind RPC** — **UNVERIFIED**: `WalletClientInit::default()` is assumed to carry no `DynBitcoindRpc`. The `receive`/`await_receive` path is expected to read **federation-reported chain state** (the federation's wallet module watches the chain), not a local RPC; confirm this before assuming deposits are detected without `ClientBuilder::with_bitcoind_rpc`.

---

## 9. Suggested implementation order

1. Workspace + `pegwallet-core` with pinned deps; `Client` construction with `MintClientInit` + `fedimint_walletv2_client::WalletClientInit`.
2. BDK wallet wrapper: create/load, `reveal_next_address`, `build_tx`+sign+broadcast, persist, `start_sync_with_revealed_spks` loop.
3. Journal + the two tables (`deposit_requests`, `withdrawal_requests`), atomic state transitions, boot reconciliation.
4. Deposit flow: single-flight open row (one per federation) → `get_next_event_log_id()` → async `receive()` (120 s bound) → persist `AddressShown` → BDK funding (`build_tx`/sign/broadcast) → `receive_fee()` → `await_receive(position)` with a real `Success`/`Aborted` match → `Done`; boot resume + `find_claim` reconcile. Concurrent deposits unsupported until #9306.
5. Withdrawal flow: `reveal_next_address` → `send_fee()` + `send_fee_quote()` → `send()` → persist `operation_id` → `await_final_send_operation_state`; boot re-await only.
6. Two sync tasks (BDK chain + fedimint federation) sharing the journal; events are UI-only.
7. `pegwallet-ffi` uniffi crate with the `PegWallet` object; generate bindings for one target language as a smoke test.

---

**Bottom line:** the wallet module is `fedimint-walletv2-client` (VERIFY.md §8). Deposits are
position-attributed, BDK-funded and discovered (`get_next_event_log_id()` before `receive()`, one
open deposit per federation, then `await_receive(position)` with real `Success`/`Aborted` handling);
withdrawals are op-id-attributed and initiated (`send()` then `await_final_send_operation_state`).
The v1 `fedimint-wallet-client` names are superseded and must not be used. No peg logic has been
implemented; `VERIFY.md` documents what was and was not verified.
