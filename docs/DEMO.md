# Demo runbook

This is the presenter runbook for a local developer meetup. It is not a release
process and it is not a security demonstration. The audience is developers and
testnet testers.

## Non-negotiable boundaries

- **No mainnet.** The alpha CLI and FFI reject mainnet. Do not use alpha.1 for a
  mainnet demo; alpha.1 predates the guard.
- **No real funds.** Use a disposable test mnemonic and disposable data
  directories.
- **No secrets on screen.** Do not show passphrases, seed words, invite codes,
  RPC credentials, or unredacted logs.
- This is unaudited software. Say that early and plainly.

## Demo shape

Target: **25 minutes plus Q&A**.

| Time | Segment |
|---|---|
| 5 min | What it is, custody split, unaudited status, mainnet disabled |
| 8–10 min | Local signet: send to an external wallet, confirm, `list-onchain` |
| 8–10 min | Devimint: deposit, ecash balance, withdrawal to external address |
| 5 min | Recovery artifact, backup, known limitations, roadmap |
| 5–10 min | Test card, GitHub issues/Discussions, Q&A |

Lightning is optional. Use a recorded fallback if the gateway is not stable.

## Preflight — at least one day before

1. Build the tagged release:

   ```bash
   cargo build --release -p berlin-wallet-cli
   target/release/berlin-wallet --version
   ```

   Do not build for the first time during the meetup.

2. Run the complete signet gate once and keep the node/data directory for the
   demo:

   ```bash
   export BITCOIND=/path/to/bitcoind
   export BITCOIN_CLI=/path/to/bitcoin-cli
   export BERLIN_WALLET_PASSPHRASE=signet-pass
   SIGNET_KEEP_NODE=1 scripts/signet-gate.sh
   ```

   The gate mines 101 blocks, sends, confirms, invalidates, rebuilds, and
   re-mines. It leaves the node and `/tmp/bw-signet-gate` in place when
   `SIGNET_KEEP_NODE=1` is set.

3. Create a separate miner wallet for demo confirmation blocks:

   ```bash
   export BITCOIN_CLI=/path/to/bitcoin-cli
   $BITCOIN_CLI -chain=signet -datadir=/tmp/bw-signet-gate \
     -rpcuser=u -rpcpassword=p -rpcport=18443 createwallet miner
   ```

4. Start devimint before the meetup and verify one deposit/withdraw cycle. Keep
   the federation and invite code available. Follow `docs/DEVIMINT.md`; do not
   improvise the federation setup live.

5. Record a 60–90 second fallback video or keep a terminal transcript of:
   - signet send and confirmation;
   - devimint deposit and withdrawal;
   - `list-onchain` output.

6. Confirm the demo machine has large terminal font, a clean data directory, and
   enough disk for the RocksDB/SQLite files.

## Live demo — signet send to another wallet

The gate has already created a funded wallet and a `recipient` bitcoind wallet.

Use two terminals:

- terminal A: bitcoind/mining and recipient inspection;
- terminal B: the Berlin Wallet CLI.

```bash
export BITCOIN_CLI=/path/to/bitcoin-cli
export BERLIN_WALLET_DATA_DIR=/tmp/bw-signet-gate/wallet
export BERLIN_WALLET_PASSPHRASE=signet-pass

# Show the funded wallet and its current history.
target/release/berlin-wallet list-onchain

# Create an external recipient address.
EXT=$($BITCOIN_CLI -chain=signet -datadir=/tmp/bw-signet-gate \
  -rpcuser=u -rpcpassword=p -rpcport=18443 \
  -rpcwallet=recipient getnewaddress)
echo "$EXT"

# Send to the external wallet.
target/release/berlin-wallet send "$EXT" 25000 2
```

Mine a confirmation block to the separate miner wallet. The signet miner includes
mempool transactions from `getblocktemplate`:

```bash
MINER=$($BITCOIN_CLI -chain=signet -datadir=/tmp/bw-signet-gate \
  -rpcuser=u -rpcpassword=p -rpcport=18443 \
  -rpcwallet=miner getnewaddress)
python3 scripts/signet-mine.py --address "$MINER" --count 1 \
  --rpc-port 18443 --rpc-user u --rpc-password p
```

Show the recipient transaction and the wallet history:

```bash
$BITCOIN_CLI -chain=signet -datadir=/tmp/bw-signet-gate \
  -rpcuser=u -rpcpassword=p -rpcport=18443 \
  -rpcwallet=recipient gettransaction <txid>

target/release/berlin-wallet list-onchain
```

The point is the external address, not an explorer. The recipient wallet's
`gettransaction` output is the proof that another wallet received the payment.

## Live demo — Fedimint deposit and withdrawal

Use the devimint federation prepared in preflight. A concise flow is:

```bash
export BERLIN_WALLET_DATA_DIR=/tmp/bw-demo-fed
export BERLIN_WALLET_PASSPHRASE=wallet-pass
target/release/berlin-wallet init --network regtest
target/release/berlin-wallet run "$FM_INVITE_CODE"
```

Inside the REPL:

```text
request-deposit 100000
deposit 100000
ecash-balance
withdraw <external-address> 50000
status <withdrawal-id>
quit
```

Use a recipient address from the devimint bitcoind wallet and show its
confirmation. The withdrawal destination is the external wallet; the federation
broadcast is the proof.

If Lightning is demonstrated, use the prepared devimint LND flow and a recorded
fallback. Do not spend the meetup debugging gateway liquidity.

## Failure fallback

If a live step stalls:

- stop retrying after the timebox;
- switch to the recorded clip/transcript;
- state exactly what failed;
- write the failure down as a test report;
- continue to the next segment.

Never improvise a new federation, network, or mainnet path during the meetup.

## Data hygiene and cleanup

- Demo data directories contain test keys. Do not commit them, share them, or
  reuse their passphrases.
- Remove disposable directories after the meetup.
- Stop devimint and the signet node.
- Do not copy real wallet data into the demo directory.
- Keep logs redacted if posting them publicly.

## After the meetup

- File the reports using the live-gate template or a GitHub issue.
- Record the demo result and any P0 in `docs/JOURNAL.md` and the issue queue.
- Do not add features before triaging the reports.
- Keep the next tag source-only unless a packaging decision is made explicitly.
