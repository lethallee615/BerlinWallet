# Devimint and live-gate runbook

This is the reproducible way to run the Berlin Wallet CLI against a local
Fedimint v0.12.0 federation. It is the setup used for the live gates in
`docs/API-VERIFICATION.md` and for issue #14.

The wallet and devimint must be the **same Fedimint generation**. The workspace
pins `v0.12.0`; a checkout at another tag can fail at client/module registration
or produce a federation the wallet cannot join.

## What you need

- Nix with flakes (the Fedimint v0.12.0 flake supplies `bitcoind`, `esplora`,
  `lnd`, `lncli`, `just`, and the Rust toolchain).
- A Fedimint checkout at tag `v0.12.0` (the examples use `~/src/fedimint`).
- ~30 GB free for the first Fedimint build.
- The Berlin Wallet binary built with the repo toolchain (1.91.1) **outside** the
  Fedimint shell. The Fedimint shell uses a different Rust version; do not build
  the wallet inside it.

## 1. Start devimint

In terminal A:

```bash
export FEDIMINT_DIR="$HOME/src/fedimint"
test -d "$FEDIMINT_DIR" || \
  git clone --branch v0.12.0 --depth 1 \
    https://github.com/fedimint/fedimint "$FEDIMINT_DIR"
cd "$FEDIMINT_DIR"
nix develop
just devimint-env
```

The first `just devimint-env` builds the Fedimint workspace (this is the long
step). Wait for `Devfed ready` and the `[devimint]` prompt. That shell now has
`FM_INVITE_CODE`, `FM_BTC_CLIENT`, `FM_BITCOIN_RPC_URL`, `FM_PORT_ESPLORA`, and
the rest of the devimint environment. Keep it open; exiting shuts the federation
down.

Fast iteration: build only what `dev-fed` needs, then reuse it:

```bash
cargo build -p devimint -p fedimintd -p fedimint-cli -p gatewayd -p gateway-cli
SKIP_CARGO_BUILD=1 just devimint-env
```

## 2. Capture the environment in a second terminal

If you are already in the devimint shell, `FM_INVITE_CODE` is exported; skip to
step 3. The instructions below are for a separate terminal.

The link path is under `$CARGO_BUILD_TARGET_DIR`, which the Fedimint flake sets
to `$REPO_ROOT/target-nix` (not plain `target`). Do not hardcode it; resolve the
link:

```bash
DEVIMINT_LINK="$(find "$FEDIMINT_DIR" -maxdepth 2 -type l -name devimint | head -1)"
TEST_DIR="$(readlink -f "$DEVIMINT_LINK")"
source "$TEST_DIR/env"
export FM_INVITE_CODE="$(cat "$TEST_DIR/clients/default-0/invite-code")"
```

Check what the federation is actually using. Run these in the devimint shell
(terminal A), where `bitcoin-cli` is on `PATH`:

```bash
echo "$FM_INVITE_CODE"
echo "$FM_BITCOIN_RPC_KIND"     # bitcoind in the v0.12.0 dev shell
echo "$FM_BITCOIN_RPC_URL"      # http://bitcoin:bitcoin@127.0.0.1:<port>
echo "$FM_PORT_ESPLORA"         # dynamically allocated
echo "$FM_LOGS_DIR"
$FM_BTC_CLIENT getblockchaininfo | jq '{chain, blocks, bestblockhash}'
```

`FM_BTC_CLIENT` is the devimint `bitcoin-cli` command. It is exported as a bare
`bitcoin-cli ...` string, so it only works where the Fedimint shell's `PATH` is
active. Terminal B only needs `FM_BITCOIN_RPC_URL`, `FM_PORT_ESPLORA`, and
`FM_INVITE_CODE`; run chain commands in terminal A. The `default` wallet is at
`/wallet/default`, so commands that need a wallet use `-rpcwallet=default`.

## 3. Build and initialize the wallet

In terminal B, from the wallet repository:

```bash
cargo build -p berlin-wallet-cli
```

If you are already in the devimint shell, do **not** run `cargo build` there:
the Fedimint shell uses a different Rust toolchain. Build the binary outside
once, then invoke the absolute path (`~/projects/berlin_wallet/target/debug/berlin-wallet`)
from the devimint shell.

```bash
export BERLIN_WALLET_DATA_DIR=/tmp/bw-gate-14
export RUST_LOG=debug
rm -rf "$BERLIN_WALLET_DATA_DIR"
./target/debug/berlin-wallet init \
  --network regtest \
  "bitcoind:$FM_BITCOIN_RPC_URL?birthday=0"
```

`init` prints a `bdk address: bcrt1...`. Fund it from the same node the
federation uses:

```bash
$FM_BTC_CLIENT -rpcwallet=default sendtoaddress <bdk-address> 0.1
$FM_BTC_CLIENT generatetoaddress 1 \
  "$($FM_BTC_CLIENT -rpcwallet=default getnewaddress)"
```

Using `$FM_BITCOIN_RPC_URL` is deliberate: the CLI and the federation must observe
the same chain. A different node is the first thing to suspect when a confirmed
deposit is invisible to the federation.

## 4. Run and deposit

```bash
./target/debug/berlin-wallet run "$FM_INVITE_CODE"
```

Expected startup:

```text
recorded network: regtest
chain-id network: regtest
chain source: bitcoind: http://127.0.0.1:<port> (rpc user bitcoin)
```

In the REPL:

```text
sync
balance
deposit 100000
status <deposit-id>
operations
ecash-balance
```

`deposit` now returns after broadcast and starts the background observer.
Immediately after it, `status` should show `AddressShown`, `funding_txid`, and
`funding_outpoint`. `operations` may still be empty and `ecash-balance` 0; the
claim needs confirmations.

## 5. Mine and wait for the claim

In the devimint shell (terminal A):

```bash
$FM_BTC_CLIENT generatetoaddress 10 \
  "$($FM_BTC_CLIENT -rpcwallet=default getnewaddress)"
$FM_BTC_CLIENT getrawtransaction <funding-txid> 1 | \
  jq '{confirmations, blockhash}'
```

Walletv2 finalizes receive outputs at `tip - 6`, so 10 blocks is comfortable.
Then in the wallet REPL:

```text
status <deposit-id>
ecash-balance
operations
```

Success means:

- `status` is `Done`, with `gross_deposited`, `claim_fee`, and a non-empty
  `net_credit`;
- `ecash-balance` increased by `net_credit`;
- `operations` shows a walletv2 Receive operation with the claim txid;
- the BDK balance dropped by the deposit plus the claim fee.

Restart check:

```text
quit
```

Then run again with the same data directory and invite. The row must still be
`Done` and the ecash balance must be unchanged (no double credit).

## 6. If the deposit stalls

The new WARN names the condition:

```text
funded deposit still has no federation receive claim after 300s; ...
```

Collect these before changing anything:

```bash
$FM_BTC_CLIENT getrawtransaction <txid> 1
$FM_BTC_CLIENT getblockcount
$FM_BTC_CLIENT getblockhash <height>
cat "$BERLIN_WALLET_DATA_DIR/chain.json"
echo "$FM_BITCOIN_RPC_URL"
echo "$FM_BITCOIN_RPC_KIND"
curl -s "http://127.0.0.1:$FM_PORT_ESPLORA/tx/<txid>"   # if esplora
ls "$FM_LOGS_DIR"
# tail the federation logs for walletv2/output-scanner errors
```

Decision tree:

1. **Is the CLI on the same node as the federation?** Compare the host and port
   in `chain.json` with `$FM_BITCOIN_RPC_URL`. A different node explains a
   confirmed transaction the federation never sees.
2. **Is it the same chain?** `getblockhash <height>` on the node the CLI used and
   on the devimint node.
3. **Did the federation see the output?** If `FM_BITCOIN_RPC_KIND=esplora`, check
   the esplora endpoint and its logs; the devimint esplora is known to panic after
   a live `invalidateblock` (`API-VERIFICATION.md` §36). Restart devimint rather
   than testing a reorg in the same session.
4. **Is the output claimable?** Compare the deposit amount with `receive_fee`.
   The pre-broadcast guard refuses an amount that cannot cover the fee; a
   federation-side fee change is the remaining case.
5. **Is the federation deferring?** `pending_tx_chain` of 3 or more makes
   walletv2 delay claims; mine blocks to drain it.

If the federation did see the output and still did not claim, capture the
fedimintd log lines and the `operations` output and keep #14 open. If the CLI
was on the wrong node, the fix is a startup preflight, not the deposit watcher.

## Cleanup

Exit the devimint shell to stop the federation, then:

```bash
rm -rf /tmp/bw-gate-14
```

Do not run `invalidateblock` during a deposit gate. For a deterministic reorg
test use the bitcoind backend and restart the devimint esplora afterwards
(`API-VERIFICATION.md` §36, §41).

## Variants

- **Esplora backend instead of bitcoind:** use
  `esplora:http://127.0.0.1:$FM_PORT_ESPLORA`. The esplora server is backed by the
  same devimint bitcoind, so the chain is shared.
- **One terminal:** from the Fedimint shell,
  `devimint --link-test-dir "${CARGO_BUILD_TARGET_DIR:-$PWD/target}/devimint" dev-fed --exec bash`
  gives a plain shell with the environment; run the prebuilt wallet binary from
  there. Exit the shell to stop the federation.
- **Electrum live run (#24):** devimint's electrs exposes a regtest Electrum RPC
  on `127.0.0.1:60401`; use `electrum:tcp://127.0.0.1:60401`.
