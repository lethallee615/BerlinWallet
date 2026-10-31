<p align="center">
  <img src="assets/logo.png" alt="Berlin Wallet" width="180">
</p>

<p align="center">
  <a href="https://github.com/lethallee615/BerlinWallet/actions/workflows/ci.yml">
    <img src="https://github.com/lethallee615/BerlinWallet/actions/workflows/ci.yml/badge.svg" alt="CI status">
  </a>
</p>

# Berlin Wallet

> **Status: early and unaudited.** The core works end-to-end against a local
> Fedimint federation, but it has not had an external audit and no funds should
> be considered safe yet. Interfaces and the on-disk format may change without
> migration.

Berlin Wallet is a Rust wallet core for an on-chain **self-custodial** BDK
wallet and a **custodial** Fedimint ecash balance while funds are pegged. The
custody split is explicit:

- On-chain funds are held by a BDK wallet (`bdk_wallet 3.2.0`) using BIP84
  `wpkh(master/84'/coin_type'/0'/0/*)` and `.../1/*` descriptors derived from a
  12-word BIP39 mnemonic.
- Ecash is held in a Fedimint federation through the `walletv2` and `mintv2`
  modules (`fedimint-walletv2-client` and `fedimint-mintv2-client` 0.12.0).
  While pegged, that ecash is custodial at the federation, not self-custodial.
- Deposit (peg-in) is: request a federation deposit address, fund it from the
  BDK wallet, wait for the walletv2 receive operation, claim it, and credit the
  mintv2 ecash balance.
- Withdrawal (peg-out) is: quote the federation send, send to a caller-supplied
  Bitcoin address, wait for the federation's final send state, and record the
  broadcast txid.
- The same core is exposed through a UniFFI crate and a development CLI. The
  near-term deliverable is a CLI-first MVP; the desktop GUI is deferred until
  after MVP, with mobile later.

This is **pre-alpha software**.

## Status

- **Pre-alpha. Not audited.**
- Deposit and withdrawal have been exercised end to end on devimint and
  Mutinynet. The live Mutinynet round trip and crash/restart matrix are recorded
  in `docs/API-VERIFICATION.md` §25 and §27. Funding returns after broadcast; a
  per-deposit background observer follows the claim and is resumed from the
  journal on restart. A funded row still unclaimed after five minutes logs a
  WARN.
- The crash matrix covers: address shown before funding, funding broadcast before
  claim, crash during confirmation, crash after `Done`, crash after
  `send()` before the operation id is persisted, and crash after the final send
  state before `Broadcast` is written.
- Restore, Send adoption, and backup, plus the FFI and reorg work, have dedicated
  tests and live checks; see `docs/API-VERIFICATION.md` §28–§31 (restore, Send
  adoption, backup), §33 (FFI), §34 (BDK sync fix), and §35 (reorg finding).
- The current test suite is 120 core tests (119 run + 1 devimint-only ignored), 7 CLI tests, and 6 FFI tests.
- There is **no mainnet operational testing** recorded in this repository and no
  independent security audit. The desktop GUI and mobile app are deferred until
  after the CLI-first MVP.
- The CLI-first MVP scope now includes Lightning send/receive via `lnv2` and
  three BDK chain backends. Esplora, Electrum, and bitcoind RPC are all wired
  behind one trait; Esplora, Electrum, and bitcoind RPC have live runs, and
  `lnv2` is registered with paid send/receive live on devimint.
  A data directory created before lnv2 was registered needs a fresh join/restore
  to gain the module. See `ROADMAP.md` for the MVP checklist and
  `docs/STATUS.md` for the current snapshot.

### Do not use this yet for

- **Large amounts.** The code is unaudited, the dependency graph is pinned to a
  pre-release Fedimint generation, and several upstream edge cases are still
  open.
- **Your only copy of a seed or passphrase.** The mnemonic is age-encrypted at
  `<data-dir>/mnemonic` with a wallet passphrase (mode `0600`); backups are
  single age-encrypted archives with a separate backup passphrase. Losing either
  passphrase loses that layer. A mnemonic alone does not recover a fresh
  client's operation log or an in-flight deposit; the recovery artifact is the
  mnemonic **plus the client data directory** (or an encrypted backup plus both
  passphrases).
- **An untrusted device.** The mnemonic ciphertext and encrypted backups are
  safe to read without their passphrases, but the live RocksDB/SQLite files are
  not application-encrypted; full-disk encryption is the supported boundary for
  those. Once the wallet is running, the unlocked process has the seed and ecash
  keys in memory. LUKS protects the disk only while the device is off or the
  volume is locked; it does not protect a compromised running OS.
- **A production custody or payments service.** The federation is custodial by
  construction, the Esplora server is trusted for chain observations, and the
  FFI/GUI/operational layer is not complete.

## Build prerequisites

The workspace builds Rust only; native C/C++ dependencies come from the
Fedimint/BDK graph.

### With Nix

The repository includes a `flake.nix` devShell. It supplies Rust 1.91.1 through
`oxalica/rust-overlay` with `rustfmt` and `clippy`, plus the native build
dependencies and derived bindgen environment. It is a **devShell only**, not a
package build.

```bash
nix develop
cargo fmt --check
cargo build --workspace
cargo test --workspace
```

For Nix users, the §40 bindgen caveat does not apply: the shell derives
`LIBCLANG_PATH` from the Nix `libclang` and
`BINDGEN_EXTRA_CLANG_ARGS` from `clang -print-resource-dir`. The build and the
`berlin-wallet` binary must be run **inside** the `nix develop` shell. The shell
does not install rustup; rust-overlay provides the toolchain directly. It also
provides `go`, which `aws-lc-sys` needs when it builds its CMake feature probes
from source, and exports an optimized `CFLAGS`/`CXXFLAGS` so a
`_FORTIFY_SOURCE` probe is not rejected at `-O0`.

If `aws-lc-sys` fails after switching between the Nix shell and the system
toolchain, the CMake cache is stale. Remove the cached build and retry:

```bash
rm -rf target/debug/build/aws-lc-sys-*
```

### Manual prerequisites

| Requirement | Why |
|---|---|
| Rust **1.91.1** | `rust-toolchain.toml` pins it. The default 1.89 toolchain cannot build the graph: `fedimint-connectors 0.12.0` pulls `iroh` crates that declare `rust-version = 1.91`. Edition is 2024. |
| C++17 compiler (`g++` or `clang++`) | `librocksdb-sys` and `aws-lc-sys` compile C/C++. `cc` alone is not sufficient. |
| `cmake` | Required by `aws-lc-sys`, a hard dependency of both `fedimint-core` and `fedimint-connectors`. |
| `clang` / `libclang` | `aws-lc-sys` enables bindgen. `LIBCLANG_PATH` must point at the directory containing `libclang.so`. |
| `pkg-config` | Used by parts of the native dependency graph. |
| Nix (optional) | Provides the checked-in devShell and is also used for Fedimint devimint local-federation testing. Not required if the manual prerequisites are installed. |

### Shell environment for bindgen

`.cargo/config.toml` deliberately does **not** set `LIBCLANG_PATH` or
`BINDGEN_EXTRA_CLANG_ARGS`. They depend on the machine and must come from the
shell. A Fedora 44 example is:

```bash
export LIBCLANG_PATH=/usr/lib64
export BINDGEN_EXTRA_CLANG_ARGS="-isystem /usr/lib/clang/22/include -isystem /usr/local/include -isystem /usr/include"
```

Do not copy the clang path blindly: derive it from `clang -E -v` and re-derive it
after every clang major upgrade. The host-specific details and the exact failure
mode of a missing `libclang` are in
[`docs/API-VERIFICATION.md`](docs/API-VERIFICATION.md), §40 and
[`docs/API-VERIFICATION-FIRST-PASS.md`](docs/API-VERIFICATION-FIRST-PASS.md), §0.

### Build and test

```bash
rustup toolchain install 1.91.1 --profile minimal --component rustfmt --component clippy
# Export LIBCLANG_PATH and BINDGEN_EXTRA_CLANG_ARGS as above.
cargo build --workspace
cargo test --workspace
```

The CLI binary is a development harness, not a production UI:

> **Network:** `init` requires `--network <mainnet|testnet|signet|regtest>` and
> records the choice in `<data-dir>/network.json`. `run` verifies it against the
> federation's chain-id and refuses a mismatch. An unknown chain-id fails closed
> unless the recorded choice is `regtest` (the explicit devimint/custom opt-in).
> A data directory created before `network.json` existed must run
> `init --network <the network it already uses>` once.

The REPL traps Ctrl+C (cancels a running `watch`) and refuses Ctrl+Z while the
database lock is held. `watch` prints its progress and heartbeat on stderr and
keeps stdout for the final row. Use `quit` to shut down; it stops the client and
releases the lock.

```bash
export BERLIN_WALLET_PASSPHRASE='choose-a-strong-wallet-passphrase'
cargo run -p berlin-wallet-cli -- init --network regtest
```

It creates or opens `<data-dir>/bdk.sqlite` and related state; the mnemonic is
encrypted with the wallet passphrase. `BERLIN_WALLET_DATA_DIR` overrides the
default `.berlin-wallet` directory. Interactive runs prompt for the passphrase;
automation can use the environment variable instead. Backups use a separate
`BERLIN_WALLET_BACKUP_PASSPHRASE`.

## Crate layout

```text
crates/
  berlin-wallet-core/          pure Rust orchestration; no UniFFI types
    src/client.rs              Fedimint client builder, recover/open, RocksDB/SQLite
    src/bdk.rs                 BDK wallet, BIP84, sync, reorg anchor check, DB swap
    src/chain/                 chain-backend trait, Esplora/Electrum/bitcoind, chain.json
    src/deposit.rs             deposit request/address/funding/watch/adoption
    src/withdrawal.rs          quote/send/reconcile/adoption
    src/journal.rs             row types, state enums, Journal trait
    src/journal_db.rs          versioned DB journal, tags, atomic transitions
    src/sync.rs                BDK, fedimint, and adoption tasks
    src/backup.rs              age-encrypted full-directory backup/import
    src/seed.rs                BIP39, canonical root secret, client identity
    src/network.rs             chain-id -> `bitcoin::Network` + recorded choice
    src/sqlite.rs              hand-written SQLite `Database` backend (mobile)
    src/wire.rs                serde wire form of `FeeQuote`
    src/error.rs               `PegError`, transient/permanent classification
    src/fault.rs               debug-only crash injection points
  berlin-wallet-ffi/          UniFFI object `PegWallet`; one namespace, one error enum
  berlin-wallet-cli/          REPL dev harness (`berlin-wallet`)
```

The core does not depend on the FFI crate. The near-term direction is
`berlin-wallet-core` → the CLI-first MVP. `berlin-wallet-ffi` remains the
binding boundary for a later desktop GUI or mobile app; neither exists yet.

## Documentation

- [`docs/README.md`](docs/README.md) — documentation index.
- [`docs/JOURNAL.md`](docs/JOURNAL.md) — plain-language narrative of what was
  built and why.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — crate boundaries, journal,
  state machines, crash safety, reorg handling, GUI gaps.
- [`docs/THREAT-MODEL.md`](docs/THREAT-MODEL.md) — trusted parties and failure
  modes.
- [`SECURITY.md`](SECURITY.md) — reporting, scope, known weaknesses.
- [`docs/AUDIT.md`](docs/AUDIT.md) — audit brief and invariants to attack.
- [`ROADMAP.md`](ROADMAP.md) — staged, honest plan.
- [`docs/API-VERIFICATION.md`](docs/API-VERIFICATION.md) — compile/live
  verification notes, crash matrix, restore experiments, reorg findings.
- [`docs/API-VERIFICATION-FIRST-PASS.md`](docs/API-VERIFICATION-FIRST-PASS.md) —
  first-pass source verification; superseded where later docs say so.
- [`docs/PLAN.md`](docs/PLAN.md) — original implementation plan and API map.

## License

MIT. See [`LICENSE`](LICENSE).

Copyright (c) 2026 lethallee615.
