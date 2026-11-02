# Contributing to Berlin Wallet

Berlin Wallet is an unaudited alpha developer preview. Contributions are
welcome, but the safety rules come first.

## Non-negotiable rules

- **Mainnet is disabled in the alpha.** Do not add a mainnet bypass, a mainnet
  demo, or a test that depends on real funds.
- **Never commit secrets.** No mnemonics, passphrases, invite codes, RPC
  credentials, private keys, live data directories, or unredacted backups.
- **Do not claim audit or production status.** The project is pre-alpha until an
  independent audit says otherwise.
- **Use the issue queue.** File a bug, feature, or live-gate report before a
  behavior change. One issue per PR-sized unit.
- **Do not force-push `main`.** Branch, open a PR, pass CI, squash-merge.

## Development setup

The repository pins Rust 1.91.1 and uses native dependencies from the Fedimint
and BDK graph.

```bash
nix develop          # preferred
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python3 scripts/check_status.py
```

Manual prerequisites are in the [README](../README.md): C++17, CMake,
`libclang`, and `pkg-config`.

## Live checks

- `scripts/signet-gate.sh` — local OP_TRUE signet send/confirm/reorg.
- `docs/DEVIMINT.md` — local Fedimint federation live gate.
- `docs/TESTING.md` — tester matrix and report format.
- `docs/DEMO.md` — presenter runbook.

A live gate is complete only when its observation is recorded in
`docs/API-VERIFICATION.md` and the issue.

## Pull requests

1. Branch from `main` with `feat/`, `fix/`, or `chore/`.
2. Link the issue: `Closes #N` or `Refs #N`.
3. Keep the change focused; include tests or a live-gate note.
4. Update `docs/STATUS.md`, `docs/JOURNAL.md`, and `docs/API-VERIFICATION.md`
   when the change affects scope, evidence, or behavior.
5. Run the full gate.
6. Squash-merge after CI is green.

## Code and documentation style

- Keep the core (`berlin-wallet-core`) free of UniFFI types.
- Keep chain access behind `ChainBackend`; a backend never persists the BDK
  database.
- Add source/API citations for new Fedimint or BDK calls.
- Record the why in the journal after the change, not as an open scratchpad.
- Do not add a generated changelog or a second project board.

## Reporting a vulnerability

Do **not** open a public issue. Follow [`SECURITY.md`](../SECURITY.md) and use
GitHub private vulnerability reporting.

## Community

By participating, you agree to the [Code of Conduct](CODE_OF_CONDUCT.md).

Use GitHub Discussions for questions and GitHub Issues for actionable reports.
There is no promised response time, bug bounty, or support SLA.
