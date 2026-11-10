#!/usr/bin/env python3
"""Independent reference vectors for the `signing` module tests.

Re-implements the Python functions of the RISEx `integration.md` guide with a
pure-Python keccak-256 (`hashlib.sha3_256` is NOT keccak). No network, no keys.

Run: python3 gen_vectors.py   (writes vectors.json next to this file)
"""
import json
import os
import re

MASK = (1 << 64) - 1
RC = [
    0x0000000000000001, 0x0000000000008082, 0x800000000000808A, 0x8000000080008000,
    0x000000000000808B, 0x0000000080000001, 0x8000000080008081, 0x8000000000008009,
    0x000000000000008A, 0x0000000000000088, 0x0000000080008009, 0x000000008000000A,
    0x000000008000808B, 0x800000000000008B, 0x8000000000008089, 0x8000000000008003,
    0x8000000000008002, 0x8000000000000080, 0x000000000000800A, 0x800000008000000A,
    0x8000000080008081, 0x8000000000008080, 0x0000000080000001, 0x8000000080008008,
]
ROT = [
    [0, 36, 3, 41, 18],
    [1, 44, 10, 45, 2],
    [62, 6, 43, 15, 61],
    [28, 55, 25, 21, 56],
    [27, 20, 39, 8, 14],
]


def rol(x, n):
    return ((x << n) | (x >> (64 - n))) & MASK if n else x


def keccak_f(a):
    for rc in RC:
        c = [a[x][0] ^ a[x][1] ^ a[x][2] ^ a[x][3] ^ a[x][4] for x in range(5)]
        d = [c[(x - 1) % 5] ^ rol(c[(x + 1) % 5], 1) for x in range(5)]
        a = [[a[x][y] ^ d[x] for y in range(5)] for x in range(5)]
        b = [[0] * 5 for _ in range(5)]
        for x in range(5):
            for y in range(5):
                b[y][(2 * x + 3 * y) % 5] = rol(a[x][y], ROT[x][y])
        a = [[b[x][y] ^ (~b[(x + 1) % 5][y] & MASK & b[(x + 2) % 5][y]) for y in range(5)] for x in range(5)]
        a[0][0] ^= rc
    return a


def keccak(data):
    rate = 136
    p = bytearray(data)
    p.append(0x01)
    p.extend(b"\x00" * (-len(p) % rate))
    p[-1] |= 0x80
    a = [[0] * 5 for _ in range(5)]
    for off in range(0, len(p), rate):
        for i in range(rate // 8):
            a[i % 5][i // 5] ^= int.from_bytes(p[off + 8 * i:off + 8 * i + 8], "little")
        a = keccak_f(a)
    return b"".join(a[i % 5][i // 5].to_bytes(8, "little") for i in range(4))


def hx(b):
    return "0x" + b.hex()


def w(v):
    return int(v).to_bytes(32, "big")


# --- integration.md: order packing and action hashes -------------------------

def pack_order(market_id, size_steps, price_ticks, side, post_only,
               reduce_only, stp_mode, order_type, tif):
    flags = ((side & 1) | (post_only << 1) | (reduce_only << 2)
             | ((stp_mode & 3) << 3) | ((order_type & 1) << 5) | ((tif & 3) << 6))
    return ((market_id << 70) | (size_steps << 38) | (price_ticks << 14)
            | (flags << 6) | (1 << 1))


def header_flags(builder_id, client_order_id, ttl_units):
    return 0x01 | (0x02 if builder_id else 0) | (0x04 if client_order_id else 0) | (0x10 if ttl_units else 0)


def place_hash(flags, order_data, builder_id, client_order_id, ttl_units):
    return keccak(keccak(b"RISE_PERPS_PLACE_ORDER_V1") + w(flags) + w(order_data)
                  + w(builder_id) + w(client_order_id) + w(ttl_units))


def cancel_hash(market_id, resting_order_id):
    return keccak(keccak(b"RISE_PERPS_CANCEL_ORDER_V1") + w(market_id) + w(resting_order_id))


def cancel_all_hash(market_id):
    return keccak(keccak(b"RISE_PERPS_CANCEL_ALL_ORDERS_V1") + w(market_id))


def leverage_hash(market_id, leverage):
    return keccak(keccak(b"RISE_PERPS_UPDATE_LEVERAGE_V1") + w(market_id) + w(leverage))


def margin_mode_hash(market_id, margin_mode):
    return keccak(keccak(b"RISE_PERPS_UPDATE_MARGIN_MODE_V1") + w(market_id) + w(margin_mode))


# --- EIP-712 ------------------------------------------------------------------

def struct_hash(type_str, values):
    types = [f.split(" ")[0] for f in re.fullmatch(r"\w+\((.*)\)", type_str).group(1).split(",")]
    assert len(types) == len(values), type_str
    out = keccak(type_str.encode())
    for t, v in zip(types, values):
        if t == "string":
            out += keccak(v.encode())
        elif t == "bytes32":
            assert len(v) == 32
            out += v
        elif t == "address":
            out += w(int(v, 16))
        else:
            assert t.startswith("uint"), t
            out += w(v)
    return keccak(out)


DOMAIN = {"name": "RISEx", "version": "1", "chain_id": 4153,
          "verifying_contract": "0x0D919DAA3f12AE715744Eb648c00066c5DBd66f0"}
DOMAIN_SEP = struct_hash(
    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    [DOMAIN["name"], DOMAIN["version"], DOMAIN["chain_id"], DOMAIN["verifying_contract"]])


def digest(type_str, values):
    return keccak(b"\x19\x01" + DOMAIN_SEP + struct_hash(type_str, values))


T_WITNESS = "VerifyWitness(address account,address target,bytes32 hash,uint48 nonceAnchor,uint8 nonceBitmap,uint32 deadline)"
T_REGISTER = "RegisterSigner(address account,address signer,string message,uint32 expiration,uint48 nonceAnchor,uint8 nonceBitmap)"
T_VERIFY = "VerifySigner(address account,uint48 nonceAnchor,uint8 nonceBitmap)"
T_REVOKE = "RevokeSigner(address account,address signer,uint48 nonceAnchor,uint8 nonceBitmap)"
T_REGISTER_V2 = "RegisterV2(address signer,string message,uint256 nonce)"
T_PLACE_TPSL = ("PlaceTpslOrder(address account,uint64 marketId,uint8 side,string size,uint8 stopType,"
                "string stopPrice,string limitPrice,uint8 orderType,uint8 stopPriceOption,uint8 tif,"
                "uint32 deadline,uint32 sizePercentBps)")
T_CANCEL_TPSL = "CancelTpslOrder(address account,string orderId,uint32 deadline)"
T_CANCEL_ALL_TPSL = "CancelAllTpslOrders(address account,uint64 marketId,uint32 deadline)"

# Known answers: keccak itself, then the type hashes read from the deployed auth contract.
assert keccak(b"").hex() == "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
assert keccak(b"a" * 135).hex() != keccak(b"a" * 136).hex()
for t, want in [
    (T_WITNESS, "055e6bcbf2ba5ff1c2ba5dc95b6648a5de6aaab3185251a34e3b88c11e116821"),
    (T_REGISTER, "a526f63b3968e56ae1b177ce9b3dc29766e0891e6397a9c23cf8c53ee8fc8f62"),
    (T_VERIFY, "4d298dcceb691695f582cc337308236426a0c97201a31834625e8eadc44d4230"),
    (T_REVOKE, "36db7f392f548b56f37d89469115d138685addf06be45684f9e5b0e8b5d28000"),
]:
    assert keccak(t.encode()).hex() == want, t

# --- vectors ------------------------------------------------------------------

# market_id, size_steps, price_ticks, side, post_only, reduce_only, stp_mode, order_type, tif,
# builder_id, client_order_id, ttl_units
PLACE = [
    (1, 1000, 65000, 0, 0, 0, 0, 1, 0, 0, 0, 0),
    (1, 1, 1, 1, 1, 0, 0, 1, 0, 0, 0, 0),
    (2, 250000, 3100, 1, 0, 1, 1, 0, 3, 0, 42, 0),
    (7, 4294967295, 16777215, 0, 1, 1, 2, 1, 1, 0, 18446744073709551615, 65535),
    (65535, 4294967295, 16777215, 1, 1, 1, 2, 1, 3, 65535, 18446744073709551615, 65535),
    (3, 123456789, 9876543, 0, 0, 0, 2, 1, 2, 12, 0, 0),
    (4, 5000, 0, 1, 0, 1, 0, 0, 3, 0, 0, 0),
    (5, 77, 1234567, 0, 1, 0, 1, 1, 1, 9, 1700000000123, 3),
]
CANCEL = [(1, 1), (1, 123456789), (2, 0), (65535, (1 << 40) - 1), (7, 987654321012), (300, 2 ** 63)]
CANCEL_ALL = [0, 1, 2, 7, 300, 65535]
LEVERAGE = [(1, 1), (1, 20), (2, 5), (7, 50), (300, 100), (65535, 255)]
MARGIN_MODE = [(1, 0), (1, 1), (2, 0), (7, 1), (300, 0), (65535, 1)]

place = []
for (m, s, p, side, po, ro, stp, ot, tif, b, c, ttl) in PLACE:
    od = pack_order(m, s, p, side, po, ro, stp, ot, tif)
    hf = header_flags(b, c, ttl)
    place.append({
        "market_id": m, "size_steps": s, "price_ticks": p, "side": side, "post_only": bool(po),
        "reduce_only": bool(ro), "stp_mode": stp, "order_type": ot, "time_in_force": tif,
        "builder_id": b, "client_order_id": c, "ttl_units": ttl,
        "order_data": hx(od.to_bytes(16, "big")), "header_flags": hf,
        "hash": hx(place_hash(hf, od, b, c, ttl)),
    })

# Signature-family inputs; the Rust tests build the same messages.
A0 = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"  # address of the test key
A1 = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8"
ROUTER = "0xaadde0cea454f2bcb26f46ed54c5709b7bb34a7e"
ANCHOR, DEADLINE = 2527069, 1786097000
WS_NONCE = "deadbeefcafebabe0123456789abcdef0123456789abcdef0123456789abcdef"
TPSL_ID = "550e8400-e29b-41d4-a716-446655440000"

out = {
    "domain": DOMAIN,
    "place": place,
    "cancel": [{"market_id": m, "resting_order_id": r, "hash": hx(cancel_hash(m, r))} for m, r in CANCEL],
    "cancel_all": [{"market_id": m, "hash": hx(cancel_all_hash(m))} for m in CANCEL_ALL],
    "leverage": [{"market_id": m, "leverage": v, "hash": hx(leverage_hash(m, v))} for m, v in LEVERAGE],
    "margin_mode": [{"market_id": m, "margin_mode": v, "hash": hx(margin_mode_hash(m, v))} for m, v in MARGIN_MODE],
    "digests": {
        "verify_witness": hx(digest(T_WITNESS, [A1, ROUTER, bytes.fromhex(place[0]["hash"][2:]), ANCHOR, 7, DEADLINE])),
        "register_signer": hx(digest(T_REGISTER, [A0, A1, "RISEx signer", 1800000000, ANCHOR, 3])),
        "verify_signer": hx(digest(T_VERIFY, [A1, ANCHOR, 3])),
        "revoke_signer": hx(digest(T_REVOKE, [A0, A1, ANCHOR, 4])),
        "register_v2": hx(digest(T_REGISTER_V2, [A0, "sign in with RISEx", int(WS_NONCE, 16)])),
        "place_tpsl": hx(digest(T_PLACE_TPSL, [A1, 1, 1, "1.5", 1, "70000", "0", 0, 1, 3, DEADLINE, 0])),
        "cancel_tpsl": hx(digest(T_CANCEL_TPSL, [A1, TPSL_ID, DEADLINE])),
        "cancel_all_tpsl": hx(digest(T_CANCEL_ALL_TPSL, [A1, 1, DEADLINE])),
    },
}

with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "vectors.json"), "w") as f:
    json.dump(out, f, indent=1)
    f.write("\n")
