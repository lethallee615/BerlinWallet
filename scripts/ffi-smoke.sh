#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p berlin-wallet-ffi
lib="target/debug/libberlin_wallet_ffi.so"
test -f "$lib"

out="${TMPDIR:-/tmp}/berlin-wallet-ffi-bindings"
rm -rf "$out"
mkdir -p "$out"
cargo run -p berlin-wallet-ffi --features uniffi-bindgen --bin uniffi-bindgen -- \
  generate --library "$lib" --language kotlin --no-format --out-dir "$out"

bindings="$out/uniffi/berlin_wallet_ffi/berlin_wallet_ffi.kt"
test -f "$bindings"
grep -q "class PegWallet" "$bindings"
grep -q "netCreditMsat" "$bindings"
grep -q "requestDeposit" "$bindings"
grep -q "quotedFeeSat" "$bindings"
grep -q "totalDebitMsat" "$bindings"
grep -q "recipientAmountSat" "$bindings"
grep -q "shutdown" "$bindings"
grep -q "open" "$bindings"
echo "ffi smoke ok: cdylib + kotlin bindings (PegWallet/DepositView/WithdrawalView surface present)"
