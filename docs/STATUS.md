# Project status

Last updated: 2026-10-31
Main at: b9262d8

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
| onchain-send | Arbitrary on-chain send with fee control and a recorded result | live | #38 | `API-VERIFICATION.md` §47; devimint + reorg 2026-10-31; signet run open |
| peg-in-out | walletv2 deposit/claim and withdrawal/send | live | — | `API-VERIFICATION.md` §25, §27, §45; live gate 2026-10-31 |
| ecash-balance | mintv2 balance distinct from the BDK balance | live | — | `API-VERIFICATION.md` §25 |
| lightning | lnv2 send/receive | todo | #23 | API verified §48; not registered or implemented |
| chain-backends | Esplora, Electrum, bitcoind RPC behind one contract | in-progress | #24 | Esplora live; bitcoind live reorg; Electrum unit only |
| restore-backup | Mnemonic restore, full-directory backup, documented limits | done | #26 | `API-VERIFICATION.md` §28–§31; `docs/ARCHITECTURE.md` §8.1 |
| crash-safety | Documented crash windows survive restart | in-progress | #25 | `API-VERIFICATION.md` §27; matrix is manual |
| fee-preview | Claim and mintv2 operation fee before funding | todo | #21 | Only known after the claim |
| watch-progress | `watch` reports progress instead of polling silently | todo | #22 | Silent polling |

## Backend coverage

| Backend | Wired | Live sync | Live deposit | Live reorg | Live restore or rebuild | Evidence |
|---|---|---|---|---|---|---|
| Esplora | yes | yes | yes | no | unit | §25, §34–§36; devimint Esplora panics on a live `invalidateblock` |
| Electrum | yes | no | no | no | no | Unit tests only; `crates/berlin-wallet-core/src/chain/electrum.rs` |
| bitcoind RPC | yes | yes | yes | yes | partial | JOURNAL 2026-10-24; PR #13 (rebuild live, restore flow unit); deposit regression #14 |

## Live verification log

Newest first. A row is evidence only if it names the flow and the observation.

| Date | Flow | Backend | Result | Evidence |
|---|---|---|---|---|
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
| P1 | #21, #23, #24, #25, #26 |
| P2 | #9, #22, #27, #28, #29, #30, #38, #39 |

P0 means funds are at risk or the primary flow is broken. P1 blocks the MVP.
P2 is hardening. `docs/TRACKING.md` defines how these are assigned.
