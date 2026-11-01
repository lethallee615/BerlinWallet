#!/usr/bin/env python3
"""Mine blocks on a local OP_TRUE signet node.

This is a deterministic test-chain helper, not a production miner. It uses only
the Python standard library and Bitcoin Core's JSON-RPC. The node must be started
with a trivially-true challenge:

    bitcoind -chain=signet -signetchallenge=51 ...

Example:

    scripts/signet-mine.py --address tb1... --count 101
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import json
import struct
import time
from typing import Any


class Rpc:
    def __init__(self, host: str, port: int, user: str, password: str) -> None:
        self.host = host
        self.port = port
        self.auth = base64.b64encode(f"{user}:{password}".encode()).decode()

    def call(self, method: str, params: list[Any] | None = None) -> Any:
        body = json.dumps(
            {"jsonrpc": "1.0", "id": "signet-mine", "method": method, "params": params or []}
        )
        conn = http.client.HTTPConnection(self.host, self.port, timeout=30)
        conn.request(
            "POST",
            "/",
            body,
            {"Authorization": f"Basic {self.auth}", "Content-Type": "application/json"},
        )
        response = conn.getresponse()
        payload = response.read()
        conn.close()
        decoded = json.loads(payload)
        if decoded.get("error"):
            raise RuntimeError(decoded["error"])
        return decoded["result"]


def sha256d(data: bytes) -> bytes:
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def varint(value: int) -> bytes:
    if value < 0xFD:
        return bytes([value])
    if value <= 0xFFFF:
        return b"\xfd" + struct.pack("<H", value)
    if value <= 0xFFFFFFFF:
        return b"\xfe" + struct.pack("<I", value)
    return b"\xff" + struct.pack("<Q", value)


def script_num(value: int) -> bytes:
    """Minimal script-number encoding used for the BIP34 coinbase height."""
    if value == 0:
        return b"\x00"
    if 1 <= value <= 16:
        return bytes([0x50 + value])
    encoded = value.to_bytes((value.bit_length() + 7) // 8, "little")
    if encoded[-1] & 0x80:
        encoded += b"\x00"
    return bytes([len(encoded)]) + encoded


def target_from_bits(bits: int) -> int:
    exponent = bits >> 24
    mantissa = bits & 0xFFFFFF
    if exponent <= 3:
        return mantissa >> (8 * (3 - exponent))
    return mantissa << (8 * (exponent - 3))


def coinbase_tx(height: int, value: int, script_pubkey: bytes, commitment: bytes) -> bytes:
    # BIP34 height, then the OP_TRUE flag byte used by the local harness.
    script_sig = script_num(height) + b"\x51"
    outputs = [
        struct.pack("<Q", value) + varint(len(script_pubkey)) + script_pubkey,
        struct.pack("<Q", 0) + varint(len(commitment)) + commitment,
    ]

    def body(with_witness: bool) -> bytes:
        tx = struct.pack("<I", 1)  # version
        if with_witness:
            tx += b"\x00\x01"  # segwit marker/flag
        tx += varint(1)
        tx += b"\x00" * 32 + struct.pack("<I", 0xFFFFFFFF)  # null prevout
        tx += varint(len(script_sig)) + script_sig
        tx += struct.pack("<I", 0xFFFFFFFE)  # sequence
        tx += varint(len(outputs)) + b"".join(outputs)
        if with_witness:
            # Witness reserved value for the BIP141 commitment.
            tx += b"\x01" + varint(32) + b"\x00" * 32
        tx += struct.pack("<I", 0)
        return tx

    return body(True)


def txid(raw_tx_without_witness: bytes) -> bytes:
    return sha256d(raw_tx_without_witness)


def mine_one(rpc: Rpc, address: str) -> tuple[bytes, str, int]:
    template = rpc.call("getblocktemplate", [{"rules": ["segwit", "signet"]}])
    height = template["height"]
    info = rpc.call("validateaddress", [address])
    script_pubkey = bytes.fromhex(info["scriptPubKey"])
    commitment = bytes.fromhex(template["default_witness_commitment"])
    raw_coinbase = coinbase_tx(height, template["coinbasevalue"], script_pubkey, commitment)
    script_sig = script_num(height) + b"\x51"
    outputs = [
        struct.pack("<Q", template["coinbasevalue"])
        + varint(len(script_pubkey))
        + script_pubkey,
        struct.pack("<Q", 0) + varint(len(commitment)) + commitment,
    ]
    coinbase_body = (
        struct.pack("<I", 1)
        + varint(1)
        + b"\x00" * 32
        + struct.pack("<I", 0xFFFFFFFF)
        + varint(len(script_sig))
        + script_sig
        + struct.pack("<I", 0xFFFFFFFE)
        + varint(len(outputs))
        + b"".join(outputs)
        + struct.pack("<I", 0)
    )
    transactions = [bytes.fromhex(tx["data"]) for tx in template["transactions"]]
    txids = [txid(coinbase_body)] + [
        bytes.fromhex(tx["txid"])[::-1] for tx in template["transactions"]
    ]
    while len(txids) > 1:
        if len(txids) % 2:
            txids.append(txids[-1])
        txids = [sha256d(txids[i] + txids[i + 1]) for i in range(0, len(txids), 2)]
    merkle = txids[0]
    version = struct.pack("<I", template["version"])
    prev = bytes.fromhex(template["previousblockhash"])[::-1]
    block_time = max(int(time.time()), template["mintime"])
    bits = int(template["bits"], 16)
    bits_bytes = struct.pack("<I", bits)
    target = target_from_bits(bits)
    nonce = 0
    while True:
        header = (
            version
            + prev
            + merkle
            + struct.pack("<I", block_time)
            + bits_bytes
            + struct.pack("<I", nonce)
        )
        digest = sha256d(header)
        if int.from_bytes(digest, "little") <= target:
            break
        nonce += 1
        if nonce > 0xFFFFFFFF:
            block_time += 1
            nonce = 0
    block = (
        header
        + varint(1 + len(transactions))
        + raw_coinbase
        + b"".join(transactions)
    )
    result = rpc.call("submitblock", [block.hex()])
    if result not in (None, "duplicate"):
        raise RuntimeError(f"submitblock rejected: {result}")
    return digest, block.hex(), height


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--address", required=True, help="signet address for each coinbase")
    parser.add_argument("--count", type=int, default=1)
    parser.add_argument("--rpc-host", default="127.0.0.1")
    parser.add_argument("--rpc-port", type=int, default=18443)
    parser.add_argument("--rpc-user", default="u")
    parser.add_argument("--rpc-password", default="p")
    args = parser.parse_args()
    if not 1 <= args.count <= 1000:
        parser.error("--count must be 1..1000")
    rpc = Rpc(args.rpc_host, args.rpc_port, args.rpc_user, args.rpc_password)
    for _ in range(args.count):
        digest, block_hex, height = mine_one(rpc, args.address)
        print(f"height={height} hash={digest[::-1].hex()} block={block_hex[:32]}...")


if __name__ == "__main__":
    main()
