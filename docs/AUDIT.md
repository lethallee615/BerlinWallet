# Audit Brief

This is a starting brief for an independent review, not a claim that a review has
happened. The code is an alpha developer preview and is unaudited.

## 1. Scope and priority

Audit **`crates/berlin-wallet-core` first**. The highest-risk code is the money
state machine, not the UI:

1. `crates/berlin-wallet-core/src/journal_db.rs` — versioned rows, claim/index
   ownership, open-row indexes, watermark, transitive state transitions.
2. `crates/berlin-wallet-core/src/deposit.rs` — receive/claim/credit path,
   adoption, net-credit reconciliation.
3. `crates/berlin-wallet-core/src/withdrawal.rs` — quote/send, send adoption,
   final-state reconciliation, change-output wait.
4. `crates/berlin-wallet-core/src/bdk.rs` — descriptor derivation, funding
   broadcast, reorg rescan and DB swap, keychain index preservation.
5. `crates/berlin-wallet-core/src/client.rs` — join/open/recover, module
   registration, database partitions.
6. `crates/berlin-wallet-core/src/backup.rs` — lock check, age archive, and manifest/import validation.

Audit **`crates/berlin-wallet-ffi` second**: the `PegWallet` boundary, async
lifetime/shutdown behavior, error flattening, and whether a host can observe or
mutate state after `shutdown()`. The CLI is a development harness; audit it only
where it demonstrates a core/FFI defect.

## 2. Assets and attacker model

Use `docs/THREAT-MODEL.md`. For this audit, assume:

- The local OS/process is trusted while running; the mnemonic is age-encrypted
  at rest but decrypted into that process, and the live databases are readable.
- The Esplora server is Byzantine for chain observations. A single server can
  lie, withhold, or censor.
- The federation is a threshold custodian and may halt, censor, or rug. Its
  operation log is the authority for walletv2 final states.
- The network/FFI host may be malicious outside the documented boundary.
- No hardware wallet or independent chain source exists. The mnemonic and
  backups are passphrase-encrypted; live database files rely on full-disk
  encryption.

## 3. Invariants to try to break

### I1. Single-open deposit and withdrawal

**Claim:** at most one open deposit and one open withdrawal exist per federation.

**Where:** `TAG_OPEN_DEPOSIT` / `TAG_OPEN_WITHDRAWAL` in `journal_db.rs`;
`is_open_deposit` / `is_open_withdrawal` in `journal.rs`; `insert_deposit`,
`insert_withdrawal`, `open_deposit`, `open_withdrawal`.

**Try to break:**

- Race two `insert_deposit` calls (the in-memory and DB tests cover this).
- Cancel while the address task is about to write; `set_deposit_address_if`
  must not resurrect a cancelled row.
- Open a withdrawal while a `Quoting` or `Submitted` row exists.
- Force a write conflict between the row write and the index write; they must be
  in one transaction.
- Reopen the DB after a crash and verify the index points at a real row.

### I2. No double-credit

**Claim:** one on-chain claim outpoint credits at most one deposit row, and a
deposit is not credited twice by watcher + adoption + restart.

**Where:** `TAG_CLAIM` and `claim_bind` / `claim_adopt_if_absent` /
`bound_outpoints` in `journal_db.rs`; `finish_deposit`,
`close_deposit_done`, `close_deposit_done_if_open`;
`scan_receive_claims`, `filter_orphan_claims`, `select_claim_for_success`.

**Try to break:**

- Two rows select the same claim; one must get `Bound`, the other `TakenBy`.
- A watcher and the adoption task race to bind the same outpoint.
- A crash after `claim_bind` but before `finish_deposit`; on restart, can the same
  outpoint be credited again?
- A crash after `finish_deposit` but before the open index is cleared; does the
  same row get credited again?
- A malformed/duplicate operation-log entry with the same outpoint.
- A changed address script or a claim with unknown script; does the fallback
  selection bind an unrelated claim?

### I3. Watermark monotonicity

**Claim:** the receive-scan watermark never moves backwards and never advances
past a gap that was not fully scanned to a terminal condition.

**Where:** `scan_receive_claims`, `is_older_than`,
`should_advance_watermark`, `SCAN_OVERLAP`, `TAG_WATERMARK`.

**Try to break:**

- A page shorter than `SCAN_PAGE` that is not actually the end.
- A state-wait budget exhausted after some operations; the watermark must not
  advance.
- An operation whose outcome becomes `Success` after the first scan; the
  overlap must re-cover it.
- An idle scan; it must not move the cursor forwards or backwards.
- A concurrent watermark update; check the last-writer behavior and whether a
  stale scan can overwrite a newer watermark.

### I4. `claim_bind` exclusivity

**Claim:** the claim key `(federation_id, outpoint)` has exactly one owner and is
written atomically with the row's `funding_outpoint`.

**Where:** `claim_key`, `read_claim_owner`, `write_claim`, `claim_bind`,
`claim_adopt_if_absent`, `bound_outpoints`.

**Try to break:**

- Two concurrent binds for different `DepositId`s.
- Rebind by the same row; must return `Bound`, not a duplicate.
- A bind after a cancel/terminal state.
- A claim key for a different federation with the same txid/vout.
- A malformed claim key; `outpoint_from_claim_key` must reject it.

### I5. Never credit before `await_receive` returns `Success`

**Claim:** `DepositState::Done` with a `net_credit` is only written after a
walletv2 receive final state of `Success` (or adoption of a claim whose cached
state is `Success`).

**Where:** `watch_deposit` (`FinalReceiveOperationState::Success`),
`adopt_orphan_claims`, `finish_deposit`, `claim_adopt_if_absent`,
`close_deposit_done*`.

**Try to break:**

- `AwaitingAddress` or `AddressShown` with a funding txid must not credit.
- `Aborted` must not credit; it returns to `AddressShown` with a new position.
- A missing/late operation-log outcome must leave the row open or pending, not
  `Done` with a guessed credit.
- A `Success` with no claim metadata must not invent an operation id, outpoint,
  gross, or fee.
- A pending operation fee (`None`) must leave `net_credit = None` and the
  pending index populated; it must never become `0`.
- A permanent receive error must mark `Failed`, not `Done`.

### I6. One withdrawal row per walletv2 Send

**Claim:** every walletv2 Send operation is represented by exactly one
withdrawal row; a custom-meta id cannot clobber a different operation.

**Where:** `TAG_WITHDRAWAL_OP`, `insert_adopted_withdrawal`,
`set_withdrawal_operation_id`, `adopt_send_claims`, `find_sent_operation`.

**Try to break:**

- Adopt the same operation twice with different custom-meta ids.
- Adopt with a custom-meta id that belongs to an existing row bound to a
  different operation.
- Crash after `send()` before `set_withdrawal_operation_id`; recovery must find
  the operation in the log and not send again.
- Crash after `Success(txid)` before `set_broadcast`; the next pass must set
  `Broadcast`, not send again.
- A `Submitted` row with no operation id and an inconclusive scan; it must not
  be treated as “safe to resend” merely because the scan returned an error.

### I7. Reorg and keychain index preservation

**Claim:** an orphaned checkpoint triggers a full rescan; the DB handoff reopens
the final inode; the next external/internal derivation index never regresses.

**Where:** `reorg_decision`, `sync_attempt`, `reset_and_full_scan`,
`swap_in_scanned_db`, `reveal_addresses_to`, `next_derivation_index`.

**Try to break:**

- Lower remote tip; equal-height hash mismatch; matching overlap.
- Crash before the rename and after the rename.
- A write after the swap; it must not hit SQLite `SQLITE_READONLY_DBMOVED`
  (1032).
- A revealed-but-unused address; after rescan it must not be handed out again.
- A reorg while a deposit is credited and while a withdrawal is pending.

### I8. Backup/restore and single-process ownership

**Claim:** a backup is a complete age-encrypted tar (encrypted mnemonic, BDK
SQLite + WAL/SHM, RocksDB), refuses a live fedimint DB, is authenticated and
versioned, and restores into an empty target.

**Where:** `backup_data_dir`, `import_data_dir`, `fedimint.db.lock`, the
`.berlin-wallet-backup.json` manifest, and the core/CLI backup tests
(`encrypted_backup_round_trips_sidecars_and_chain_source`,
`wrong_passphrase_and_tampering_leave_destination_untouched`,
`import_requires_an_empty_target_and_ignores_stale_temporary_dirs`,
`backup_refuses_a_live_database_and_ignores_stale_archive_temps`,
`backup_does_not_overwrite_an_existing_archive`,
`backup_refuses_a_legacy_plaintext_mnemonic`, and the CLI round-trip tests).

**Try to break:**

- Backup with WAL/SHM present; import and open the wallet.
- Backup with a live `run` process; it must fail loudly.
- Destination inside source, non-empty destination, missing mnemonic/BDK/DB.
- Restore-backup then `run`; the retained operation log must remain.
- Two wallet processes against one data directory; document the failure mode
  rather than assuming a lock saves the directory.

## 4. Known-broken or unverified items

These should be treated as audit findings until proven otherwise:

| Item | Status / reference |
|---|---|
| Fresh restore does not resume in-flight operations and can strand a funded, unclaimed deposit. There is no public walletv2 rescan/re-claim in 0.12.0. | `docs/API-VERIFICATION.md` §30/§31 |
| A reorg that orphans an already-credited deposit is not deterministically tested. | §41 |
| The dependency advisory check is non-blocking and has explicit ignores; three high-severity open alerts are in the DNS/CRL graph. | `deny.toml`, `SECURITY.md`, `DEPENDENCIES.md` |
| devimint Esplora panics on a live `invalidateblock`; reorg tests need a restart. | §36 |
| No public signet/Mutinynet on-chain send and no mainnet run; the signet evidence is a local custom OP_TRUE chain. Unknown chain IDs fail closed unless the data directory is recorded as regtest. | §58, `network.rs` |
| `lnv2` gateway/liquidity/routing trust has not had a dedicated adversarial review; paid sends/receives live on devimint only. | §48/§49 |
| No structured FFI errors, event stream, per-operation cancellation, platform dirs, GUI, or mobile app. | `docs/ARCHITECTURE.md` §11, `ROADMAP.md` |
| A crash after the network broadcast but before BDK persistence and the journal's `funding_txid` write can leave no recorded txid and allow a new funding attempt; the exact double-fund outcome is not covered by §27's tested window. | `deposit.rs` `fund_deposit`; §27 covers only the post-journal crash |

## 5. What is verified vs assumed

### Source-verified against pinned crates

- The actual Fedimint/BDK/UniFFI names used by the core and FFI are recorded in
  `docs/API-VERIFICATION-FIRST-PASS.md` and `docs/API-VERIFICATION.md` §1–§26.
  This includes the `ClientPreview` return type, `Database::with_prefix`,
  `await_receive` shape, `FeeQuote`/`FeeQuoteWire`, `bdk_esplora` update methods,
  and the `WalletClientInit` registration.
- The BDK lower-tip no-op and the reorg fix are source-verified against
  `bdk_wallet 3.2.0`, `bdk_chain 0.23.3`, `bdk_esplora 0.22.1`, and
  `esplora-client 0.12.3`; see §34/§35.
- The root-secret derivation is pinned by a golden vector in `seed.rs` and
  compared with `PlainRootSecretStrategy`.
- FFI methods/records and Kotlin binding names are smoke-tested in §33.

### Live-tested

- Mutinynet deposit/withdrawal round trip (§25).
- Crash/restart matrix with debug fault injection (§27).
- Mnemonic restore and retained client DB behavior (§28–§31).
- Age-encrypted full-directory backup/import with live lock failure, WAL/SHM
  coverage, wrong-passphrase/tamper handling, and stale temporaries (§31, §56).
- devimint/live reorg checks (§35) and the transient sync behavior (§34).

### Assumed, not proven

- A consistent Byzantine Esplora server is outside the ancestry guard.
- Reorg behavior for an already-credited deposit is unknown.
- The `lnv2` gateway/liquidity/routing trust model has not had a dedicated
  adversarial review.
- Public-network latency, mempool, and faucet behavior are not tested for the
  on-chain send.
- Mainnet chain/federation behavior, public signet/Mutinynet on-chain send,
  multi-federation behavior, mobile device lifecycle, and real GUI behavior are
  not tested.
- The dependency ignores are based on reachability reasoning, not a full audit.

The first-pass document contains early assumptions that later sections supersede.
Read `docs/API-VERIFICATION.md` §27–§41 and the current source before relying on
`docs/API-VERIFICATION-FIRST-PASS.md` for a specific claim.

## 6. Suggested review outputs

For each finding, record:

- invariant and file/line;
- crash/interleaving/Byzantine assumptions;
- reproduction (unit test or devimint script) and observed result;
- impact: loss of funds, double credit, stuck funds, privacy, availability, or
  only a UX/observability defect;
- whether the fix changes the on-disk journal format;
- regression test and documentation update.

Do not call the wallet audited until an independent review covers the audited
commit and its findings are resolved or explicitly accepted.
