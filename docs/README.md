# Documentation Index

Start with [`../README.md`](../README.md) if you are new to the project. The
source code and the current verification notes are authoritative; older planning
documents are kept for history and may describe superseded assumptions.

## Public-facing documents

| Document | What it covers |
|---|---|
| [`../README.md`](../README.md) | What the wallet is, pre-alpha status, “do not use yet” limits, build prerequisites, crate layout, license. |
| [`JOURNAL.md`](JOURNAL.md) | Plain-language narrative of what was built and why, including the current CLI-first MVP scope decision. |
| [`STATUS.md`](STATUS.md) | Machine-readable snapshot: MVP row status, backend coverage, live verification log, and open work by priority. |
| [`TRACKING.md`](TRACKING.md) | The work queue, label/milestone taxonomy, status vocabulary, and the agent workflow. |
| [`DEVIMINT.md`](DEVIMINT.md) | Local Fedimint v0.12.0 federation setup and the deposit live-gate checklist (#14). |
| [`DEPENDENCIES.md`](DEPENDENCIES.md) | Dependabot policy, the Fedimint git-patch pin, and the documented `lru` exception. |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | Crate boundaries, data directory, journal format and invariants, deposit/withdrawal state machines, crash-safety windows, reorg handling, `[patch.crates-io]`, GUI gaps, unknowns. |
| [`THREAT-MODEL.md`](THREAT-MODEL.md) | Trusted parties, what breaks if the federation/Esplora/BDK/OS/mnemonic/backups/data-directory assumptions fail, and explicit out-of-scope items. |
| [`../SECURITY.md`](../SECURITY.md) | Reporting a vulnerability, scope, supported versions, known-weak list, advisory policy. |
| [`AUDIT.md`](AUDIT.md) | Audit brief: which crate first, invariants to attack, known-broken items, source-verified vs assumed. |
| [`../ROADMAP.md`](../ROADMAP.md) | Staged CLI-first MVP plan, hardening, audit, and deferred GUI/mobile work. |

## Verification and planning history

| Document | Status |
|---|---|
| [`API-VERIFICATION.md`](API-VERIFICATION.md) | Current compile/live verification notes: API deviations, live Mutinynet runs, crash matrix, restore/backup policy, reorg findings, FFI, observability. Sections §27–§41 are important. |
| [`API-VERIFICATION-FIRST-PASS.md`](API-VERIFICATION-FIRST-PASS.md) | First-pass source verification. Useful for exact early signatures, but some claims are superseded by `API-VERIFICATION.md`; check the later document before relying on a detail. |
| [`PLAN.md`](PLAN.md) | Original implementation plan and API map. It contains pre-implementation assumptions; the source and API verification notes win. |

## Repository-level files

| File | Notes |
|---|---|
| [`../AGENTS.md`](../AGENTS.md) | Durable working context for coding agents. It lives at the repository root because OpenCode reads it there. It is not API documentation and can lag the actual source layout. |
| [`../LICENSE`](../LICENSE) | MIT license, copyright 2026 lethallee615. |
| [`../deny.toml`](../deny.toml) | cargo-deny license/source/bans policy and the explicit advisory ignores. |
| [`../.github/workflows/ci.yml`](../.github/workflows/ci.yml) | CI: format, strict clippy, workspace tests, strict licenses/sources/bans, non-blocking advisory check. |
| [`../.github/dependabot.yml`](../.github/dependabot.yml) | Weekly Cargo and GitHub Actions update policy. |
| [`../scripts/ffi-smoke.sh`](../scripts/ffi-smoke.sh) | Builds the FFI cdylib and checks generated Kotlin binding surface. |

## Where to look for a specific topic

- **How the project got here / why the current scope changed:** `JOURNAL.md`.
- **What is left, who owns it, and what is verified:** `STATUS.md` and the open
  GitHub issues; the workflow is `TRACKING.md`.
- **Running a local federation / the #14 live gate:** `DEVIMINT.md`.
- **Why the journal is safe/unsafe:** `ARCHITECTURE.md` §5 and §8;
  `API-VERIFICATION.md` §18, §22, §27.
- **Deposit flow:** `ARCHITECTURE.md` §6; `crates/berlin-wallet-core/src/deposit.rs`;
  `API-VERIFICATION.md` §25, §27.
- **Withdrawal flow:** `ARCHITECTURE.md` §7;
  `crates/berlin-wallet-core/src/withdrawal.rs`;
  `API-VERIFICATION.md` §25, §27, §29.
- **Restore and in-flight funds:** `API-VERIFICATION.md` §28–§31;
  `ARCHITECTURE.md` §8.1 and §12; `THREAT-MODEL.md`.
- **Reorgs and Esplora:** `API-VERIFICATION.md` §34–§36; `ARCHITECTURE.md` §8.1;
  `crates/berlin-wallet-core/src/bdk.rs`.
- **FFI/GUI readiness:** `API-VERIFICATION.md` §33; `ARCHITECTURE.md` §11;
  `crates/berlin-wallet-ffi/src/lib.rs`.
- **Build failures:** `API-VERIFICATION.md` §40;
  `API-VERIFICATION-FIRST-PASS.md` §0; `README.md` build prerequisites.
- **Known upstream/blocked items:** `API-VERIFICATION.md` §30–§41;
  `AUDIT.md` §4; `SECURITY.md` known weaknesses.
- **What was actually verified:** `API-VERIFICATION.md` §1–§41;
  `AUDIT.md` §5 distinguishes source-verified, live-tested, and assumed.
