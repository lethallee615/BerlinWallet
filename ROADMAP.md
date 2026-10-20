# Roadmap

This is a staged plan, not a release schedule. There are no dates. A stage is
done only when its gate is met and documented.

The wallet is pre-alpha: deposit and withdrawal have been exercised end to end on
devimint and Mutinynet, but the code is unaudited and several upstream edge cases
are still open.

## Now — hygiene and documentation

Current state:

- `cargo fmt --check`, strict clippy, and `cargo test --workspace` pass in CI.
- `cargo deny check` passes with explicit advisory ignores; licenses, sources,
  and bans are strict.
- README, architecture, threat model, security policy, audit brief, and this
  roadmap exist.
- The UniFFI surface and Kotlin binding smoke test exist.
- The CLI is a development REPL, not a production UI.

Remaining “now” work:

- Keep the docs synchronized with source and with upstream Fedimint/BDK behavior.
- Keep the crash matrix and live verification notes current after any state-machine
  change.
- Make the repository’s agent context (`AGENTS.md`) and API notes agree with the
  actual crate layout when they drift.

## Hardening

This is the work that must happen before “real use” is more than an aspiration.

### Encryption at rest and encrypted backups

- Encrypt the mnemonic at rest, or store the seed in a platform keystore, with a
  documented recovery path. The current `<data-dir>/mnemonic` is plaintext.
- Define an authenticated encrypted backup format (or use `age`/`gpg` with a
  documented, tested procedure) and include a version/format identifier.
- Decide whether database encryption is required for `fedimint.rocksdb` and
  `bdk.sqlite`, or whether full-disk encryption is the only supported boundary.
- Test backup/import across crashes, WAL/SHM sidecars, RocksDB sidecars, and
  stale backups. A backup is not useful if it cannot be restored.

### State-machine and integration tests

- Add devimint integration tests for the full deposit and withdrawal state
  machines, not only manual runs.
- Make the crash-injection matrix a repeatable test suite, including the windows
  around BDK broadcast, `send()`, operation-id persistence, change-output
  finalization, and `Broadcast`.
- Add deterministic reorg tests with bitcoind `invalidateblock` and
  `generatetoaddress`, including:
  - a reorg before a deposit is credited;
  - a reorg after a deposit is credited (currently untested);
  - a lower remote tip and an equal-height hash mismatch;
  - the SQLite connection handoff and next-index preservation.
- Add concurrency/property tests for:
  - one open deposit/withdrawal per federation;
  - one claim owner per outpoint;
  - one withdrawal row per walletv2 Send operation;
  - terminal-only watermark advance.
- Add a restore test that clearly asserts balances-only recovery, empty history,
  and stranded in-flight operations, so the UI cannot accidentally promise more.

### Fee and status correctness

- Preview the mintv2 operation fee at deposit request time, not only in the
  post-claim `net_credit`. The current UI cannot show the full round-trip cost
  before funding.
- Resolve the 224 msat withdrawal quote/charge mismatch and make the quote
  reconstruct the actual debit.
- Investigate `change_outpoint_range`: the named txids were not observed on
  chain. `await-change` must either verify real outpoints or be treated as an
  advisory wait.
- Give `watch` progress/heartbeat output; a silent hang is currently
  indistinguishable from work.

### Dependency and supply-chain work

- Update `anyhow`, `rustls-webpki`, `hickory-proto`, `imbl-sized-chunks`, and the
  other advisory-affected transitive crates as upstream allows.
- Remove the `[advisories].ignore` entries in `deny.toml` one by one and make the
  advisory check blocking.
- Revisit the 62-crate Fedimint `[patch.crates-io]` pin when a release with the
  needed crates is available.
- Add dependency review to the release checklist; license/source checks are not a
  security review.

### Recovery and upstream gaps

- Chase the Fedimint upstream issue: 0.12.0 has no public walletv2 rescan/re-claim
  path. A mnemonic-only restore can strand a funded, unclaimed deposit and does
  not resume an in-flight withdrawal.
- Ask upstream for a supported history/operation-log recovery path and for
  clarity on `FeeQuote`, `change_outpoint_range`, and restore semantics.
- Until that exists, make “mnemonic + full client data directory” the only
  documented recovery artifact and warn on every fresh restore.
- Consider a local operation-history export that is explicitly **not** a
  replacement for the client DB.

### GUI prerequisites

Before a desktop GUI can be more than a polling harness, the binding needs:

- an event stream for deposit/withdrawal/balance changes;
- structured, branchable errors instead of one display string;
- non-blocking long operations with progress and resumability;
- per-operation cancellation;
- platform data-directory defaults and permissions;
- a defined multi-federation model if the GUI is to support more than one.

These are not implemented today. See `docs/ARCHITECTURE.md` §11.

## Audit

- Start with the core, especially `journal_db.rs`, `deposit.rs`,
  `withdrawal.rs`, and `bdk.rs`. The FFI boundary is second.
- Use `docs/AUDIT.md` as the brief: single-open, no double-credit, watermark
  monotonicity, `claim_bind` exclusivity, “never credit before
  `await_receive` returns `Success`”, one Send per operation, reorg/index
  preservation, and backup/restore.
- Run an AI-assisted review first if useful, but do not treat it as an audit.
  Commission a human audit if the project is going to hold real funds or face
  real users; an AI review does not replace adversarial human review or a
  reproducible test suite.
- Fix findings, add regression tests, and publish the results and residual risks.
- Do not describe the wallet as audited until an independent report exists and
  covers the audited commit.

## Real use

### Desktop GUI

- Build the GUI on `berlin-wallet-ffi` (core → binding → GUI).
- Implement the event stream, structured errors, progress, cancellation, and
  platform data directories listed above.
- Make the recovery policy visible:
  - “Recovery requires mnemonic plus the original client data backup.”
  - “A previous deposit may still need to be claimed; a new deposit request does
    not recover it.”
  - “History is unavailable after a fresh restore; balances were recovered.”
- Gate mainnet/large-value use behind the audit, encryption, dependency cleanup,
  and restore/in-flight policy.

### Mobile

- Reuse the UniFFI layer; do not fork the state machine.
- Treat `shutdown()` on backgrounding as mandatory: it stops tasks, consumes the
  client handle, and releases database locks.
- Add platform storage, backup, lifecycle, and notification integrations that do
  not exist in this repository.
- Test the actual generated bindings on devices, not only the Kotlin smoke test.

## Out of scope today

- **Lightning / `lnv2`.** The test federation exposes it, but this wallet does not
  register or use it. Treat it as a future integration, not a current feature.
- Hardware wallets, BDK multisig, secure elements.
- Multi-federation management and federation discovery.
- Chain-analysis resistance or a privacy-preserving Esplora replacement.
- Mainnet operational tooling, hosted services, and custody operations.

## Open decisions

- Seed encryption design and recovery UX (passphrase, OS keystore, both?).
- Backup encryption format and whether it becomes an application feature or stays
  an operator procedure.
- Whether database-level encryption is required in addition to full-disk
  encryption.
- GUI framework and desktop platform targets.
- Mobile platforms and the FFI lifecycle contract on each.
- Whether the project will ever claim a trustless bridge (it should not: this is a
  custodial Fedimint peg).
- How much upstream walletv2 recovery work is practical versus documenting the
  full-directory backup requirement indefinitely.
