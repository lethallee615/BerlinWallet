# Local signet send gate

This is the reproducible signet harness for the on-chain send path (#38). It is
a **custom OP_TRUE signet** chain, not the public signet-4 or Mutinynet network:
it uses Bitcoin Core's signet consensus with `-signetchallenge=51`, a trivial
challenge for which the empty signet solution is valid. No public faucet or
federation is involved.

It exists because this repository has no public signet federation/faucet
harness. The on-chain `send`/`list-onchain` path only needs a BDK wallet and a
signet node; it does not need a federation. The harness funds that wallet with
mature coinbases from `scripts/signet-mine.py`.

## Requirements

- `bitcoind` and `bitcoin-cli` (Bitcoin Core 28 or newer; the gate was run with
  31.0).
- `python3`, `jq`, and a built `target/debug/berlin-wallet`.
- About 15 minutes for 101 signet blocks on a typical CPU.

## Run

```bash
export BITCOIND=/path/to/bitcoind
export BITCOIN_CLI=/path/to/bitcoin-cli
export BERLIN_WALLET_PASSPHRASE=signet-pass
scripts/signet-gate.sh
```

The script:

1. Starts a fresh `-chain=signet -signetchallenge=51` node and creates a
   recipient wallet.
2. Runs `berlin-wallet init --network signet` against the local bitcoind RPC.
3. Mines 101 blocks to the BDK address (100-block coinbase maturity plus one).
4. Runs the headless `berlin-wallet send <external> 50000 2` and records the
   quote, fee, total debit, change, and txid.
5. Mines one block including the send transaction and prints
   `berlin-wallet list-onchain` and `gettransaction`.
6. Invalidates that block, re-mines the mempool transaction, and repeats
   `list-onchain`, forcing the BDK reorg rebuild.

`scripts/signet-mine.py` is a test-chain helper. It constructs a coinbase with
the BIP34 height, the OP_TRUE signet solution, the BIP141 witness commitment, and
mempool transactions from `getblocktemplate`.

## Scope and limitations

- This is a **custom signet**; the public signet-4 and Mutinynet networks were
  not rerun. The chain source is a local bitcoind RPC, not public Esplora.
- No federation is involved. The headless `send`/`list-onchain` commands open
  only the BDK wallet, the local journal, and the configured chain source.
- A signet chain source needs `?birthday=<height>` for the bitcoind backend. Use
  `birthday=1` on this fresh local chain; `birthday=0` is rejected when the reorg
  rebuild tries a non-regtest genesis scan.
- The 101 coinbases are test funds on a trivial custom chain. Never reuse this
  setup for public signet or mainnet.
- The miner is a deterministic test helper, not a robust/signet-aware mining
  implementation.
