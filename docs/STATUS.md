# Project status

Last updated: 2026-10-31
Main at: cd2450e

This is the machine-readable status snapshot. `docs/TRACKING.md` explains the
vocabulary and the update rules. Every row names an issue or evidence; `—` means
neither exists yet, which is itself a finding.

Status values: `todo`, `in-progress`, `in-review`, `live`, `done`, `at-risk`,
`blocked`.

## MVP definition

The requirements are defined in `ROADMAP.md`. This table says where each one
actually stands.

| ID | Requirement | Status | Issues | Evidence |
|---|---|---|---|---|
| onchain-send | Arbitrary on-chain send with fee control and a recorded result | live | — | `API-VERIFICATION.md` §47, §58; devimint + reorg; local OP_TRUE signet send/reorg 2026-10-31 |
| peg-in-out | walletv2 deposit/claim and withdrawal/send | live | — | `API-VERIFICATION.md` §25, §27, §45; live gate 2026-10-31 |
| ecash-balance | mintv2 balance distinct from the BDK balance | live | — | `API-VERIFICATION.md` §25 |
| lightning | lnv2 send/receive | live | — | `API-VERIFICATION.md` §48, §49; paid send/receive, expiry, restart 2026-10-31; fresh join required |
| chain-backends | Esplora, Electrum, bitcoind RPC behind one contract | live | — | `API-VERIFICATION.md` §51; Electrum live 2026-10-31; Esplora and bitcoind live |
| restore-backup | Mnemonic restore, full-directory backup, documented limits | done | — | `API-VERIFICATION.md` §28–§31, §52; `docs/ARCHITECTURE.md` §8.1 |
| encryption-at-rest | Mnemonic and backups encrypted; live DB boundary documented | done | — | `API-VERIFICATION.md` §56; full-disk boundary for live RocksDB/SQLite |
| crash-safety | Documented crash windows survive restart | done | — | `API-VERIFICATION.md` §27, §53; deterministic CI matrix; one devimint-only window |
| fee-preview | Claim and mintv2 operation fee before funding | live | — | `API-VERIFICATION.md` §50; devimint 2026-10-31 |
| watch-progress | `watch` reports progress instead of polling silently | live | — | `API-VERIFICATION.md` §54; devimint heartbeat/SIGINT 2026-10-31 |

## Release v0.1.0-alpha.1

The first alpha is a **source-only CLI developer preview**. It is not a
production release, a binary release, or a crates.io publication.

| Release gate | Status | Issue | Evidence |
|---|---|---|---|
| Security/roadmap/status truth | done | #57 | PR #62 |
| Version, `--version`, release notes | done | #58 | PR #63 |
| Source-only pre-release workflow | done | #59 | PR #64 |
| Dependency queue and advisory reachability | done | #60 | PR #65 |
| Gate, tag, and publish `v0.1.0-alpha.1` | done | #61 | tag `cd2450e`; <https://github.com/lethallee615/BerlinWallet/releases/tag/v0.1.0-alpha.1> |
| Portable release checksum | in-review | #67 | this PR |

The `v0.1.0-alpha.1` pre-release exists. `ROADMAP.md` has the alpha boundary;
`SECURITY.md` has the current limitations. The portable-checksum follow-up #67
remains open so the next tag is verifiable without path editing.

## Backend coverage

| Backend | Wired | Live sync | Live deposit | Live reorg | Live restore or rebuild | Evidence |
|---|---|---|---|---|---|---|
| Esplora | yes | yes | yes | no | unit | §25, §34–§36; devimint Esplora panics on a live `invalidateblock` |
| Electrum | yes | yes | yes | no | no | §51; sync/deposit/withdrawal/restart/`chain.json` live; full scan/restore and reorg not live |
| bitcoind RPC | yes | yes | yes | yes | yes | JOURNAL 2026-10-24; PR #13 rebuild; §52 full-scan restore; deposit regression #14 |

## Live verification log

Newest first. A row is evidence only if it names the flow and the observation.

| Date | Flow | Backend | Result | Evidence |
|---|---|---|---|---|
| 2026-10-31 | REPL PTY: two-line paste executed both balances; stray `s` absorbed; `!echo`/`shell echo` ran; background WARN redrew the prompt; startup BDK address stable | local CLI | pass | #9, §57 |
| 2026-10-31 | Source-only alpha `v0.1.0-alpha.1` published as a pre-release (not latest); source archive + portable SHA-256 verified | GitHub release | pass | #61; release URL |
| 2026-10-31 | Local OP_TRUE signet alpha gate: 50,000 sat send at 2 sat/vB, fee 281 sat, total debit 50,281 sat, txid cc748ba8...; block 102 confirmed; invalidateblock + re-mine re-confirmed after BDK rebuild | signet bitcoind | pass | #38/#61, §58 |
| 2026-10-31 | 50,000 sat withdrawal: chain 228 sat + federation 101,600 msat; total debit 50,329,600 msat matched the balance delta; change range identified as ecash change | devimint bitcoind | pass | #28/#29, §37/§38 |
| 2026-10-31 | Legacy mnemonic migrated to age; encrypted backup/restore reopened with wallet and backup passphrases; wrong passphrase rejected | local CLI | pass | #27, §56 |
| 2026-10-31 | `RUST_LOG=info` quit with a funded/open deposit: no `fm::task` WARN/ERROR after the prompt; exit 0 | devimint bitcoind | pass | #39, §55 |
| 2026-10-31 | `watch` heartbeat: initial + 10s stderr lines with state/positions/funding; SIGINT returned to the prompt | devimint bitcoind | pass | #22, §54 |
| 2026-10-31 | Fresh restore recovered 99,577,856 msat and BDK 0.09849694; empty history/warnings; new receive did not reclaim a stranded 50,000 sat deposit | bitcoind RPC | pass | #26, §52 |
| 2026-10-31 | Electrum sync, 100,000 sat deposit -> Done (99,577,856 msat), 50,000 sat withdrawal -> Broadcast and 1 conf; restart and `chain.json` reuse | electrum tcp://127.0.0.1:60401 | pass | #24, §51 |
| 2026-10-31 | 100,000 sat deposit preview: claim 316 sat, operation estimate 100,100 msat; actual 103,072 msat, net 99,580,928 msat | bitcoind RPC | pass | #21, §50 |
| 2026-10-31 | lnv2 paid send 1,000 sat -> `Succeeded`, LND invoice `SETTLED` | devimint | pass | #23, §49 |
| 2026-10-31 | lnv2 paid receive 50,000 sat -> `Claimed` | devimint | pass | #23, §49 |
| 2026-10-31 | lnv2 receive paid while offline, then restart -> `Claimed` | devimint | pass | #23, §49 |
| 2026-10-31 | lnv2 expired receive/send -> `Expired`/`Failed`; duplicate send refused | devimint | pass | #23, §49 |
| 2026-10-31 | Devimint federation exposes `lnv2` as module 0 | devimint | pass | §48 |
| 2026-10-31 | 10,000 sat on-chain send at 2 sat/vB -> 1 conf; `invalidateblock` -> unconfirmed -> re-mine -> 1 conf | bitcoind RPC | pass | #18, §47 |
| 2026-10-31 | 100,000 sat deposit -> row `Done`, 99,577,856 msat credited; restart did not double credit | bitcoind RPC | pass | #14, §45 |
| 2026-10-30 | Combined `deposit` stays in `AddressShown` | bitcoind RPC | fail | #14 |
| 2026-10-26 | Orphaned-checkpoint rebuild names its scan floor | bitcoind RPC | pass | PR #13; JOURNAL 2026-10-25 |
| 2026-10-24 | 10-block reorg over a credited deposit | bitcoind RPC | pass | JOURNAL 2026-10-24; §41 |
| 2026-10-24 | SQLite swap and next-index preservation | — | pass | Regression test in `crates/berlin-wallet-core/src/bdk.rs` |
| 2026-10-20 | Deposit, claim, and withdrawal round trip | Esplora | pass | §25, §27 |

## Open work by priority

| Priority | Issues |
|---|---|
| P0 | — |
| P1 | #67 |
| P2 | — |

P0 means funds are at risk or the primary flow is broken. P1 blocks the MVP.
P2 is hardening. `docs/TRACKING.md` defines how these are assigned.
