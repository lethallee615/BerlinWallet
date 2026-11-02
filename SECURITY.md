# Security Policy

Berlin Wallet is an **unaudited alpha developer preview**. Do not assume any
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

This is a volunteer, alpha developer-preview project. There is no bug bounty and
no guaranteed response or disclosure timeline. The maintainers will try to
acknowledge a report, reproduce it, and coordinate a fix and disclosure; do not
publish details before a fix or an agreed disclosure date.

## Supported versions

There are no stable releases. `v0.1.0-alpha.1` and `v0.1.0-alpha.2` are
source-only developer previews; the branch `main` is the only supported line. There are no backports to
tags or release branches, and no commitment that a future release will preserve
the current data format.

Mainnet is disabled in the current alpha source and FFI. The CLI/FFI reject
mainnet initialization and restore, and a data directory recorded as mainnet
fails closed. Alpha.1 predates that guard; do not use it with mainnet.

## Known weaknesses

These are current, documented limitations. They are listed here so reporters do
not spend time rediscovering them; they are still useful as audit targets. The
full trust-boundary analysis is in [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md).

1. **No audit.** The core, FFI, and dependency graph have not been independently
   audited.
2. **Custodial federation.** A federation threshold can halt, censor, or rug.
   See the federation section of `docs/THREAT-MODEL.md`.
3. **Esplora privacy and chain trust.** One Esplora endpoint sees every BDK
   address/script and is trusted for chain observations. It can link the wallet's
   on-chain history and can lie about the chain. The ancestry guard is not a
   Byzantine-chain defense. The bitcoind RPC and Electrum backends have different
   trust boundaries, documented in the threat model.
4. **Fresh restore can strand in-flight funds.** Mnemonic-only restore recovers
   balances but not the operation log or in-flight operations. A funded,
   unclaimed deposit is not re-claimed by requesting a new deposit. The recovery
   artifact is mnemonic **plus** the client data directory (or the encrypted
   backup). See `docs/API-VERIFICATION.md` §30/§31.
5. **No structured FFI errors or event stream.** `PegWalletError` is a display
   string; a GUI must poll status. There is no per-operation cancellation or
   event stream.
6. **Single-open data directory.** There is no application-level whole-directory
   lock. Running two processes against one directory is unsupported. The backup
   lock covers the Fedimint DB, not the BDK SQLite DB.
7. **Dependency advisories are non-blocking.** CI runs
   `cargo deny check licenses sources bans` strictly, but the advisory check is
   `continue-on-error`. The open GitHub alerts include three high-severity
   findings in the `hickory-resolver`/`hickory-proto` DNS and
   `rustls-webpki` CRL paths. The wallet does not use those APIs directly; the
   reachability reasoning is summarized in
   [`docs/DEPENDENCIES.md`](docs/DEPENDENCIES.md) and the accepted RustSec
   exceptions are in `deny.toml`. This is not a full audit of the dependency
   graph.
8. **No public signet or mainnet operational run for the on-chain send.** Live
   evidence is devimint, the earlier Mutinynet deposit/withdrawal round trip,
   and a local custom OP_TRUE signet send/reorg gate
   (`docs/API-VERIFICATION.md` §47, §58). There is no mainnet run. Public
   signet-4/Mutinynet on-chain send is not yet recorded.
9. **devimint Esplora panics on live reorgs.** `invalidateblock` can kill the
   devimint Esplora process; the reorg test needs a manual restart
   (`docs/API-VERIFICATION.md` §36).
10. **The CLI is a development harness.** There is no desktop GUI or mobile app.
    The CLI is suitable for developers and disposable test data, not a
    production user interface.
11. **The live databases are not application-encrypted.** Full-disk encryption
    is the supported boundary for `fedimint.rocksdb` and `bdk.sqlite`. The
    mnemonic and backup archive are age-encrypted; the live databases are not.
12. **No whole-device hardening.** There is no hardware wallet, secure enclave,
    memory zeroization guarantee, or protection against a compromised running
    OS. See `docs/THREAT-MODEL.md`.

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
maintainers have chosen not to block the **alpha developer preview** on those
findings. A future hardening step is to update the dependencies, remove the
ignores, and make the advisory check blocking before beta/production. See
`ROADMAP.md`.

## Security review pointers

- `docs/THREAT-MODEL.md` — trust boundaries and failure modes.
- `docs/AUDIT.md` — invariants and suggested audit scope.
- `docs/ARCHITECTURE.md` — journal, state machines, crash-safety windows.
- `docs/API-VERIFICATION.md` §27 (crash matrix), §30/§31 (restore),
  §33 (FFI), §34/§35 (reorg), §36 (devimint Esplora), §47/§58 (on-chain send),
  §52 (restore policy), §56 (encryption).
