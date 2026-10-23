# Roadmap

This is a staged plan, not a release schedule. There are no dates. A stage is
done only when its gate is met and documented.

The wallet is pre-alpha: deposit and withdrawal have been exercised end to end on
devimint and Mutinynet, but the code is unaudited and several upstream edge cases
are still open. The near-term product is a **CLI-first MVP**. A desktop GUI is
deferred until after the MVP; mobile is later still.

## MVP is done when…

This is the current definition of the MVP. All boxes must be checked before the
MVP is called done.

- [ ] **On-chain send/receive.** A user can reveal/receive to a BDK address and
      send an arbitrary on-chain payment from the BDK wallet, with fee control and
      a recorded result.
- [x] **Peg-in and peg-out.** `walletv2` deposit/claim and withdrawal/send work
      end to end on devimint and Mutinynet.
- [x] **Ecash balance.** The mintv2 BTC balance is readable and represented
      distinctly from the BDK on-chain balance.
- [ ] **Lightning send/receive via `lnv2`.** Invoices can be paid and received
      through the federation's Lightning module, with final states and failure
      handling.
- [ ] **Three chain backends.** The BDK chain source can be Esplora, Electrum, or
      bitcoind RPC, selected by configuration, with the same reorg and persistence
      guarantees.
- [x] **Restore and backup.** Mnemonic restore, full-directory backup/import, and
      the limitations of a mnemonic-only restore are implemented and documented.
      In-flight-operation recovery remains an upstream gap.
- [ ] **Crash safety.** The state machines survive the documented crash windows
      without double credit, double send, lost credit, or a stale open row. The
      existing matrix is a start; the narrow broadcast/persist windows and
      credited-deposit reorg need coverage.
- [ ] **Fee preview at request-deposit.** The deposit flow shows the on-chain
      claim fee and the mintv2 operation fee before the user commits funds.
- [ ] **Watch progress output.** `watch` reports meaningful progress/heartbeat
      information instead of polling silently.

## Now — hygiene and documentation

Current state:

- `cargo fmt --check`, strict clippy, and `cargo test --workspace` pass in CI.
- `cargo deny check` passes with explicit advisory ignores; licenses, sources,
  and bans are strict.
- README, architecture, threat model, security policy, audit brief, journal, and
  this roadmap exist.
- The UniFFI surface and Kotlin binding smoke test exist.
- The CLI is a development REPL, not a production UI.
- The Nix devShell supplies the pinned Rust toolchain and native dependencies.

Remaining “now” work:

- Keep the docs synchronized with source and with upstream Fedimint/BDK behavior.
- Keep the crash matrix and live verification notes current after any state-machine
  change.
- Make the repository’s agent context (`AGENTS.md`) and API notes agree with the
  actual crate layout when they drift.
- Turn the new MVP definition into tracked implementation work rather than leaving
  it only in this document.

## Hardening

This is the work that must happen before “real use” is more than an aspiration.

### On-chain send/receive

- Add a first-class BDK send path for arbitrary recipients, not only federation
  deposit funding.
- Define fee selection, change handling, PSBT/signing, broadcast, and on-chain
  transaction history.
- Make received on-chain funds visible with confirmation state and reorg-aware
  balance reporting.
- Test send/receive, fee bumps if supported, and reorgs on devimint and a signet
  environment.

### Lightning via `lnv2`

- Register and verify `fedimint-lnv2-client` against the pinned Fedimint version;
  it is currently **not** registered and its API surface has not been verified in
  this repository.
- Define the gateway/lightning requirements for paying and receiving invoices,
  including liquidity and routing assumptions.
- Build send/receive state machines with the same journal discipline as the
  on-chain peg: idempotency, final-state reconciliation, crash recovery, and
  restart adoption.
- Add fee/quote visibility, payment expiry, failure reasons, and retry rules.
- Add devimint and live tests for paid, failed, expired, and interrupted invoices.
- Do not assume `lnv2` is trustless or free of federation/gateway trust; document
  the actual trust boundary before calling it supported.

### Alternate chain backends

- [x] Implement Electrum (`bdk_electrum`) and bitcoind RPC (`bdk_bitcoind_rpc`)
      behind the BDK wallet as selectable chain sources, alongside Esplora.
- [x] Keep one chain-source contract: sync, full scan, broadcast, tip/ancestry
      checks, reorg detection, and persistence behavior. The contract is the
      `ChainBackend` trait; the reorg anchor check is shared and pure, and
      persistence stays in `BdkWallet`.
- [x] Add configuration/CLI selection and clear errors for unreachable or
      misconfigured backends. The choice is persisted in `chain.json` and a
      backend change is never silent.
- [ ] Test each backend against reorgs, transient failures, restore full scans,
      and wallet restart. **Only Esplora has a live run.** Electrum needs
      devimint's electrs (regtest Electrum RPC on 127.0.0.1:60401); bitcoind
      needs the devimint node plus a birthday height. The deterministic
      `invalidateblock` reorg test is expected to run on the bitcoind backend,
      because the devimint Esplora binary panics on a live invalidate.
- [x] Document the privacy and operational differences: an Electrum server and
      an Esplora server both see addresses; bitcoind RPC is a different trust
      boundary (`docs/THREAT-MODEL.md`).

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

## Audit

- Start with the core, especially `journal_db.rs`, `deposit.rs`,
  `withdrawal.rs`, and `bdk.rs`. The FFI boundary is second.
- Use `docs/AUDIT.md` as the brief: single-open, no double-credit, watermark
  monotonicity, `claim_bind` exclusivity, “never credit before
  `await_receive` returns `Success`”, one Send per operation, reorg/index
  preservation, and backup/restore.
- Extend the audit scope to Lightning/`lnv2` and the Electrum/bitcoind RPC
  backends once they exist.
- Run an AI-assisted review first if useful, but do not treat it as an audit.
  Commission a human audit if the project is going to hold real funds or face
  real users; an AI review does not replace adversarial human review or a
  reproducible test suite.
- Fix findings, add regression tests, and publish the results and residual risks.
- Do not describe the wallet as audited until an independent report exists and
  covers the audited commit.

## MVP delivery

The MVP is the CLI-first feature set in the checklist above. It is not a GUI
release. The CLI should expose the complete on-chain, peg, ecash, and Lightning
flows, with the recovery and fee-preview behavior documented rather than hidden.

## Deferred after MVP

### Desktop GUI

The GUI is deferred until the MVP is done. When it is resumed:

- Build it on `berlin-wallet-ffi` (core → binding → GUI).
- Implement the event stream, structured errors, progress, cancellation, and
  platform data directories that the current binding lacks.
- Make the recovery policy visible:
  - “Recovery requires mnemonic plus the original client data backup.”
  - “A previous deposit may still need to be claimed; a new deposit request does
    not recover it.”
  - “History is unavailable after a fresh restore; balances were recovered.”
- Gate mainnet/large-value use behind the audit, encryption, dependency cleanup,
  and restore/in-flight policy.

The GUI prerequisites are:

- an event stream for deposit/withdrawal/balance changes;
- structured, branchable errors instead of one display string;
- non-blocking long operations with progress and resumability;
- per-operation cancellation;
- platform data-directory defaults and permissions;
- a defined multi-federation model if the GUI is to support more than one.

These are not implemented today. See `docs/ARCHITECTURE.md` §11.

### Mobile

- Reuse the UniFFI layer; do not fork the state machine.
- Treat `shutdown()` on backgrounding as mandatory: it stops tasks, consumes the
  client handle, and releases database locks.
- Add platform storage, backup, lifecycle, and notification integrations that do
  not exist in this repository.
- Test the actual generated bindings on devices, not only the Kotlin smoke test.

## Out of scope today

- Hardware wallets, BDK multisig, secure elements.
- Multi-federation management and federation discovery.
- Chain-analysis resistance or a privacy-preserving Esplora replacement. The
  alternate backends are about redundancy/operations, not privacy.
- Mainnet operational tooling, hosted services, and custody operations.
- GUI and mobile applications until the CLI-first MVP is complete.

## Open decisions

- Seed encryption design and recovery UX (passphrase, OS keystore, both?).
- Backup encryption format and whether it becomes an application feature or stays
  an operator procedure.
- Whether database-level encryption is required in addition to full-disk
  encryption.
- How to select and configure the three chain backends, and which one becomes the
  default for each network.
- `lnv2` gateway/lightning requirements, liquidity assumptions, and trust model.
- GUI framework and desktop platform targets after MVP.
- Mobile platforms and the FFI lifecycle contract on each.
- Whether the project will ever claim a trustless bridge (it should not: this is a
  custodial Fedimint peg).
- How much upstream walletv2 recovery work is practical versus documenting the
  full-directory backup requirement indefinitely.
