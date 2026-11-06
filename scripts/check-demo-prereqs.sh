#!/usr/bin/env bash
# Check the local prerequisites for the signet demo.
#
# This does not install anything. It reports whether the wallet, Bitcoin Core,
# Python, jq, the signet miner, and the signet port are ready. Set BITCOIND,
# BITCOIN_CLI, and optionally BERLIN_WALLET before running it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WALLET="${BERLIN_WALLET:-$ROOT/target/release/berlin-wallet}"
if [ ! -x "$WALLET" ] && [ -x "$ROOT/target/debug/berlin-wallet" ]; then
    WALLET="$ROOT/target/debug/berlin-wallet"
fi
BITCOIND="${BITCOIND:-}"
BITCOIN_CLI="${BITCOIN_CLI:-}"
failures=0

ok() { printf 'ok: %s\n' "$*"; }
warn() { printf 'warn: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; failures=$((failures + 1)); }

if [ -x "$WALLET" ]; then
    if version="$("$WALLET" --version 2>/dev/null)"; then
        ok "wallet: $version"
    else
        fail "wallet exists but does not run: $WALLET"
    fi
else
    fail "wallet not found at $WALLET (build with: cargo build --release -p berlin-wallet-cli)"
fi

if [ -z "$BITCOIND" ]; then
    BITCOIND="$(command -v bitcoind || true)"
fi
if [ -z "$BITCOIN_CLI" ]; then
    BITCOIN_CLI="$(command -v bitcoin-cli || true)"
fi

if [ -n "$BITCOIND" ] && [ -x "$BITCOIND" ]; then
    version="$("$BITCOIND" --version 2>/dev/null | head -1 || true)"
    ok "bitcoind: ${version:-$BITCOIND}"
    major="$(printf '%s\n' "$version" | sed -n 's/.*version v\([0-9][0-9]*\).*/\1/p')"
    if [ -n "$major" ] && [ "$major" -lt 28 ]; then
        fail "Bitcoin Core 28 or newer is required (found v$major)"
    fi
else
    fail "bitcoind not found; set BITCOIND or install Bitcoin Core 28+"
fi

if [ -n "$BITCOIN_CLI" ] && [ -x "$BITCOIN_CLI" ]; then
    ok "bitcoin-cli: $("$BITCOIN_CLI" --version 2>/dev/null | head -1)"
else
    fail "bitcoin-cli not found; set BITCOIN_CLI or install Bitcoin Core 28+"
fi

for tool in python3 jq; do
    if command -v "$tool" >/dev/null 2>&1; then
        ok "$tool: $(command -v "$tool")"
    else
        fail "$tool is required"
    fi
done

if [ -x "$ROOT/scripts/signet-mine.py" ] && [ -x "$ROOT/scripts/signet-gate.sh" ]; then
    ok "signet scripts: $ROOT/scripts"
else
    fail "signet scripts are missing or not executable"
fi

if [ -n "${BERLIN_WALLET_PASSPHRASE:-}" ]; then
    ok "BERLIN_WALLET_PASSPHRASE is set (test passphrase)"
else
    warn "BERLIN_WALLET_PASSPHRASE is not set; the gate will prompt"
fi

if command -v ss >/dev/null 2>&1; then
    if ss -ltn 2>/dev/null | grep -qE '[:.]18443[[:space:]]'; then
        warn "port 18443 already has a listener; stop it or use SIGNET_RPC_PORT"
    else
        ok "port 18443 is free"
    fi
else
    warn "ss not available; cannot check port 18443"
fi

if [ "$failures" -ne 0 ]; then
    printf '\n%d prerequisite check(s) failed.\n' "$failures" >&2
    exit 1
fi
printf '\nDemo prerequisites look ready.\n'
