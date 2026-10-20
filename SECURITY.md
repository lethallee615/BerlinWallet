# Security Policy

Berlin Wallet is **pre-alpha** and **has not been audited**. Do not assume any
security property beyond what is explicitly tested and documented in this
repository.

## Scope

The security scope is:

- `crates/berlin-wallet-core` — wallet orchestration, journal, BDK wrapper,
  deposit/withdrawal state machines, backup, seed, network, and database
  backends.
- `crates/berlin-wallet-ffi` — the UniFFI `PegWallet` object, its records/enums,
  and the boundary between Rust errors and FFI errors.
- Dependency behavior only where it is reachable through the core/FFI and
  relevant to wallet funds, key material, or state integrity.

The CLI (`crates/berlin-wallet-cli`) is a development harness. It is in scope
for bugs that demonstrate a core/FFI issue or a dangerous operational default,
but it is not a production UI.

Out of scope for this policy:

- GUIs, mobile applications, and host applications that are not in this
  repository.
- A malicious host process, compromised OS, physical attacks, side channels, and
  memory scraping. See `docs/THREAT-MODEL.md`.
- Federation solvency, governance, and guardian operations. The federation is
  custodial by construction; "the federation can rug" is a documented trust
  assumption, not a wallet code vulnerability. Report it if the code makes that
  worse than documented.
- Third-party Esplora services, Fedimint deployments, BDK releases, and platform
  code, except where the pinned integration is the direct cause.
- Mainnet deployment, operational monitoring, incident response, and legal or
  regulatory issues.

## Reporting a vulnerability

Please **do not open a public issue** for a suspected vulnerability.

Use GitHub's private vulnerability reporting for this repository:

<https://github.com/lethallee615/berlin-wallet/security/advisories/new>

If that is unavailable, contact the repository owner through GitHub
(`@lethallee615`) and ask for a private channel.

A useful report includes:

- the affected commit/version and crate;
- the platform, toolchain, and build mode;
- whether the issue is in core, FFI, or a dependency integration;
- a minimal reproduction, expected behavior, and actual behavior;
- logs or database snippets with all secrets removed;
- whether funds, the mnemonic, the client data directory, or a live federation
  were involved.

**Never include a mnemonic, seed phrase, private key, live data directory, or
unredacted backup in a report.** Reproduce with a test mnemonic and a disposable
directory where possible.

This is a volunteer, pre-alpha project. There is no bug bounty and no guaranteed
response or disclosure timeline. The maintainers will try to acknowledge a
report, reproduce it, and coordinate a fix and disclosure; do not publish
details before a fix or an agreed disclosure date.

## Supported versions

There are no stable releases. The branch `main` is the only supported line.
There are no backports to tags or release branches, and no commitment that a
future release will preserve the current data format.

## Known weaknesses

These are current, documented limitations. They are listed here so reporters do
not spend time rediscovering them; they are still useful as audit targets. The
full trust-boundary analysis is in [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md).

1. **No audit.** The core, FFI, and dependency graph have not been independently
   audited.
2. **Plaintext mnemonic at rest.** `<data-dir>/mnemonic` is plaintext, mode
   `0600` on Unix. There is no seed encryption or OS keystore integration.
3. **Unencrypted backups.** `berlin-wallet backup` copies the whole directory,
   including the mnemonic, without encryption. Treat the archive as key
   material; encrypt it externally (`age`/`gpg`).
4. **Custodial federation.** A federation threshold can halt, censor, or rug.
   See the federation section of `docs/THREAT-MODEL.md`.
5. **Esplora privacy and chain trust.** One Esplora endpoint sees every BDK
   address/script and is trusted for chain observations. It can link the wallet's
   on-chain history and can lie about the chain. The ancestry guard is not a
   Byzantine-chain defense.
6. **Fresh restore can strand in-flight funds.** Mnemonic-only restore recovers
   balances but not the operation log or in-flight operations. A funded,
   unclaimed deposit is not re-claimed by requesting a new deposit. The recovery
   artifact is mnemonic **plus** the client data directory. See
   `docs/API-VERIFICATION.md` §30/§31.
7. **Withdrawal quote mismatch.** A test observed `send_fee_quote` differing from
   the later charged operation fee by 224 msat; the quote-drift check did not
   catch it. See `docs/API-VERIFICATION.md` §38.
8. **Unverifiable change outpoints.** walletv2 `change_outpoint_range` named
   txids that were never observed on chain in testing. `await-change` does not
   prove the outpoints it names. See `docs/API-VERIFICATION.md` §37.
9. **Mintv2 fee not previewed.** The deposit UI/API does not show the mintv2
   operation fee at request time; it is accounted for later in `net_credit`.
10. **Silent watch.** `watch` polls without a heartbeat; a hang is not
    distinguishable from progress. See `docs/API-VERIFICATION.md` §39.
11. **No structured FFI errors or event stream.** `PegWalletError` is a display
    string; the GUI must poll status. There is no per-operation cancellation.
12. **Single-open data directory.** There is no application-level whole-directory
    lock. Running two processes against one directory is unsupported. The backup
    lock covers the Fedimint DB, not the BDK SQLite DB.
13. **Dependency advisories are non-blocking.** CI runs
    `cargo deny check licenses sources bans` strictly, but the advisory check is
    `continue-on-error`. Known advisories are explicitly listed in
    `deny.toml` under `[advisories].ignore`; see the next section.
14. **No mainnet run recorded.** Live peg tests are Mutinynet/devimint. Unknown
    chain IDs fall back to `Network::Regtest`.
15. **devimint Esplora panics on live reorgs.** `invalidateblock` can kill the
    devimint Esplora process; the reorg test needs a manual restart
    (`docs/API-VERIFICATION.md` §36).

## Dependency advisory policy

The advisory check is intentionally non-blocking today. `.github/workflows/ci.yml`
runs:

```text
cargo deny check licenses sources bans     # strict
cargo deny check advisories                # continue-on-error: true
```

The current advisory ignores, with one-line reasons, are in
[`deny.toml`](deny.toml) under `[advisories].ignore`. They are transitive or
pinned-dependency issues that the current code does not exercise in the way the
advisory describes; that reasoning must be re-checked when dependencies change.

This is not a statement that the dependency graph is clean. It means the
maintainers have chosen not to block the pre-alpha build on those findings. A
future hardening step is to update the dependencies, remove the ignores, and make
the advisory check blocking. See `ROADMAP.md`.

## Security review pointers

- `docs/THREAT-MODEL.md` — trust boundaries and failure modes.
- `docs/AUDIT.md` — invariants and suggested audit scope.
- `docs/ARCHITECTURE.md` — journal, state machines, crash-safety windows.
- `docs/API-VERIFICATION.md` §27 (crash matrix), §30/§31 (restore),
  §33 (FFI), §34/§35 (reorg), §37–§41 (known gaps).
