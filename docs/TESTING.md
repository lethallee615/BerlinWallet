# Alpha testing guide

This is the tester checklist for the source-only developer preview. The audience
is developers and testnet testers, not end users with real funds.

## Safety boundary

- **Mainnet is disabled.** `init --network mainnet` and `restore --network
  mainnet` must fail.
- Use disposable data directories and test mnemonics only.
- Do not report real seeds, passphrases, invite codes, RPC credentials, or
  unredacted data directories.
- No test in this guide requires real funds.
- The signet gate is a local custom OP_TRUE chain; it is not public signet.

## Environment record

Include this in every report:

- release tag or commit (`berlin-wallet --version`, `git rev-parse HEAD`);
- OS and architecture;
- Rust/toolchain version;
- chain backend (`esplora`, `electrum`, or `bitcoind`);
- network (`signet`, `testnet`, or `regtest`);
- federation/devimint commit or image, if relevant;
- exact command(s) and the expected result.

## Test matrix

### T1 — build and identity

```bash
cargo build --release -p berlin-wallet-cli
target/release/berlin-wallet --version
target/release/berlin-wallet --help
```

Pass: version matches the tag; help labels the binary as an unaudited developer
preview and says mainnet is disabled.

### T2 — mainnet guard

```bash
export BERLIN_WALLET_DATA_DIR=/tmp/bw-test-mainnet
export BERLIN_WALLET_PASSPHRASE=test-pass
target/release/berlin-wallet init --network mainnet
```

Pass: exit is non-zero, the error says mainnet is disabled, and no
`network.json`/`bdk.sqlite` is created. Repeat for FFI `init` if you use the
bindings, and for `restore --network mainnet` with a disposable mnemonic.

### T3 — local signet send and reorg

```bash
export BITCOIND=/path/to/bitcoind
export BITCOIN_CLI=/path/to/bitcoin-cli
export BERLIN_WALLET_PASSPHRASE=signet-pass
scripts/signet-gate.sh
```

Pass: `signet gate ok`; 50,000 sat send at 2 sat/vB; fee 281 sat; total debit
50,281 sat; block 102 confirmation; `invalidateblock` rebuild; re-mine;
same txid confirmed again.

This test takes about 15 minutes. If your machine mines slowly, report the
observed time rather than treating it as a failure.

### T4 — send to another wallet

Use the `recipient` bitcoind wallet or a second `berlin-wallet` data directory
against the same local signet node. Send a fresh amount, mine a block, and show
the recipient's `gettransaction`/balance.

Pass: the external wallet received the exact amount; the sender's
`list-onchain` row has the same txid and a confirmation.

### T5 — devimint deposit and withdrawal

Follow `docs/DEVIMINT.md` to start a federation and obtain the invite. Then:

```text
init --network regtest
run <invite>
request-deposit 100000
deposit 100000
ecash-balance
withdraw <external-address> 50000
status <withdrawal-id>
```

Pass: deposit reaches `Done`, ecash increases by the expected net credit,
withdrawal reaches `Broadcast`, and the external recipient sees the transaction.

### T6 — restart and backup

- Stop and restart the wallet; confirm no double credit and no lost open row.
- Run `backup <file>` with a separate backup passphrase.
- Import into an empty directory with `restore-backup <file>`.
- Try the wrong wallet/backup passphrase once.

Pass: encrypted archive imports; wrong passphrase fails; no plaintext mnemonic
is accepted in a new backup.

### T7 — chain backend

Repeat a sync/deposit/send on Electrum or bitcoind RPC if available. Pass: the
recorded `chain.json` backend is reused on restart and a changed backend is never
silent.

### T8 — lnv2 (optional, devimint)

Use the prepared devimint gateway to pay an invoice and receive one; restart
between receive and claim if possible. Record any gateway/routing failure as a
report, not a pass.

## Report format

Use the live-gate issue template or a GitHub issue. Include:

```text
Test: T3
Release/commit:
OS/arch:
Backend:
Network:
Command:
Expected:
Actual:
Result: pass/fail
Evidence: redacted output, txid, block height
```

For a failure, include the smallest reproduction and the first error. Do not
attach the data directory or any secret.

## First local alpha cycle exit criteria

- At least 3–5 testers complete one scenario each.
- T1, T2, T3, and T5 have at least one report.
- Every report is triaged into an issue or closed with a reason.
- No open P0 finding.
- No unresolved onboarding blocker for the next alpha.

The test plan is intentionally small. The goal is honest signal about the
documented paths, not exhaustive coverage.
