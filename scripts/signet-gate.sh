#!/usr/bin/env bash
# Live on-chain send gate on a local OP_TRUE signet chain.
#
# This is a test harness, not a production miner or federation. It starts a
# signet bitcoind with a trivially-true challenge, mines coinbase funds to the
# BDK wallet, exercises the headless `send`/`list-onchain` path, and performs a
# block reorg. The block miner is scripts/signet-mine.py.
#
# Requirements: bitcoind, bitcoin-cli, python3, jq, a built berlin-wallet binary,
# and ~15 minutes for 101 signet blocks on a typical CPU.
#
# Env overrides:
#   BITCOIND, BITCOIN_CLI, BERLIN_WALLET (binary)
#   SIGNET_DATA_DIR (default /tmp/bw-signet-gate)
#   SIGNET_RPC_PORT (18443), SIGNET_P2P_PORT (38333)
#   BERLIN_WALLET_PASSPHRASE (required)
#   SIGNET_KEEP_NODE=1 (leave bitcoind running for a demo/rehearsal)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BITCOIND="${BITCOIND:-bitcoind}"
BITCOIN_CLI="${BITCOIN_CLI:-bitcoin-cli}"
WALLET="${BERLIN_WALLET:-$ROOT/target/debug/berlin-wallet}"
DATA="${SIGNET_DATA_DIR:-/tmp/bw-signet-gate}"
RPC_PORT="${SIGNET_RPC_PORT:-18443}"
P2P_PORT="${SIGNET_P2P_PORT:-38333}"
RPC_USER=u
RPC_PASS=p
CHAIN_SOURCE="bitcoind:http://$RPC_USER:$RPC_PASS@127.0.0.1:$RPC_PORT?birthday=1"

test -n "${BERLIN_WALLET_PASSPHRASE:-}" || {
    echo "set BERLIN_WALLET_PASSPHRASE (wallet data is encrypted)" >&2
    exit 1
}
test -x "$WALLET" || {
    echo "berlin-wallet not found at $WALLET; build with: cargo build -p berlin-wallet-cli" >&2
    exit 1
}

btc() {
    "$BITCOIN_CLI" -chain=signet -datadir="$DATA" \
        -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" -rpcport="$RPC_PORT" "$@"
}

stop_node() {
    if [ "${SIGNET_KEEP_NODE:-0}" = "1" ]; then
        echo "SIGNET_KEEP_NODE=1: leaving signet bitcoind running on port $RPC_PORT"
        return
    fi
    btc stop >/dev/null 2>&1 || true
}
trap stop_node EXIT

rm -rf "$DATA"
mkdir -p "$DATA"
"$BITCOIND" -chain=signet -signetchallenge=51 -datadir="$DATA" -daemon \
    -server -rpcuser="$RPC_USER" -rpcpassword="$RPC_PASS" \
    -rpcport="$RPC_PORT" -port="$P2P_PORT" -fallbackfee=0.0001 -txindex \
    >/dev/null
for _ in $(seq 1 60); do
    btc getblockchaininfo >/dev/null 2>&1 && break
    sleep 1
done
btc getblockchaininfo >/dev/null
btc createwallet recipient >/dev/null

export BERLIN_WALLET_DATA_DIR="$DATA/wallet"
INIT=$(timeout 180 "$WALLET" init --network signet "$CHAIN_SOURCE")
printf '%s\n' "$INIT" | grep '^chain source: bitcoind:'
BDK_ADDR=$(printf '%s\n' "$INIT" | sed -n 's/^bdk address: //p' | head -1)
test -n "$BDK_ADDR"

echo "mining 101 signet blocks to $BDK_ADDR"
python3 "$ROOT/scripts/signet-mine.py" \
    --address "$BDK_ADDR" --count 101 \
    --rpc-port "$RPC_PORT" --rpc-user "$RPC_USER" --rpc-password "$RPC_PASS"

EXT_ADDR=$(btc -rpcwallet=recipient getnewaddress)
echo "external signet address: $EXT_ADDR"

echo "=== headless signet send ==="
SEND=$(timeout 300 "$WALLET" send "$EXT_ADDR" 50000 2)
printf '%s\n' "$SEND"
TXID=$(printf '%s\n' "$SEND" | sed -n 's/^  txid: //p' | head -1)
test -n "$TXID"
printf '%s\n' "$SEND" | grep -q 'fee: 281 sat'
printf '%s\n' "$SEND" | grep -q 'total debit: 50281 sat'

echo "=== confirm in the next signet block ==="
python3 "$ROOT/scripts/signet-mine.py" \
    --address "$BDK_ADDR" --count 1 \
    --rpc-port "$RPC_PORT" --rpc-user "$RPC_USER" --rpc-password "$RPC_PASS"
btc -rpcwallet=recipient gettransaction "$TXID" | jq '{txid, confirmations, blockhash}'
timeout 300 "$WALLET" list-onchain | grep -E "journalled sends:|\b$TXID\b"

BLOCK=$(btc -rpcwallet=recipient gettransaction "$TXID" | jq -r '.blockhash')
test -n "$BLOCK"

echo "=== invalidate $BLOCK and re-mine (reorg) ==="
btc invalidateblock "$BLOCK"
timeout 300 "$WALLET" list-onchain >/tmp/bw-signet-reorg-before.txt 2>&1 || true
python3 "$ROOT/scripts/signet-mine.py" \
    --address "$BDK_ADDR" --count 1 \
    --rpc-port "$RPC_PORT" --rpc-user "$RPC_USER" --rpc-password "$RPC_PASS"
btc -rpcwallet=recipient gettransaction "$TXID" | jq '{txid, confirmations, blockhash}'
timeout 300 "$WALLET" list-onchain | grep -E "journalled sends:|\b$TXID\b"

echo "signet gate ok: send, confirmation, and reorg exercised"
