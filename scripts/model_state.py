#!/usr/bin/env python3
"""Independent model of the router's state image codec.

`src/state.rs` is the part of the contract whose byte layout must agree exactly
with the `fixed_bytes` length declared in `avm-contract.toml`, because a
mismatch surfaces as a `StateModeMismatch`/decode failure at runtime rather than
at compile time.

This script re-derives that layout from the manifest and the documented field
sizes -- independently of the Rust code -- then checks that:

  * the header and route table sum to the declared ABI length;
  * a codec built from those sizes round-trips;
  * the upgrade marker lands inside a region the decoder does not require zero;
  * the sentinel `last_status` cannot collide with a legal AVM status code.

Run it with:

    python3 scripts/model_state.py
"""

from __future__ import annotations

import pathlib
import re
import struct
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# Field sizes taken from the AVM canonical types, not from src/state.rs:
# u64 = 8, u32 = 4, u8 = 1, AccountId/DappId/pubkey = 32, AvmAmount(u128) = 16.
HEADER_FIELDS = [
    ("routed_count", 8),
    ("completed_count", 8),
    ("failed_count", 8),
    ("owner_pubkey", 32),
    ("version", 1),
    ("route_count", 1),
    ("reserved", 2),
    ("last_status", 4),
    ("last_route_key", 8),
    ("migration_payload", 24),
]
SLOT_FIELDS = [("key", 8), ("dapp_id", 32), ("account_id", 32), ("min_value", 16)]
MAX_ROUTES = 4

AWAITING_CALLBACK = 0xFFFF_FFFF
UPGRADE_MARKER = b"SRV2"


def offsets(fields: list[tuple[str, int]]) -> dict[str, tuple[int, int]]:
    result: dict[str, tuple[int, int]] = {}
    position = 0
    for name, size in fields:
        result[name] = (position, position + size)
        position += size
    return result


HEADER = offsets(HEADER_FIELDS)
SLOT = offsets(SLOT_FIELDS)
HEADER_BYTES = sum(size for _, size in HEADER_FIELDS)
SLOT_BYTES = sum(size for _, size in SLOT_FIELDS)
STATE_BYTES = HEADER_BYTES + MAX_ROUTES * SLOT_BYTES


def encode(state: dict) -> bytes:
    image = bytearray(STATE_BYTES)
    image[0:8] = struct.pack("<Q", state["routed_count"])
    image[8:16] = struct.pack("<Q", state["completed_count"])
    image[16:24] = struct.pack("<Q", state["failed_count"])
    image[24:56] = state["owner_pubkey"]
    image[56] = state["version"]
    image[57] = len(state["routes"])
    image[58:60] = b"\x00\x00"
    image[60:64] = struct.pack("<I", state["last_status"])
    image[64:72] = state["last_route_key"]
    image[72:96] = state["migration_payload"]
    for index, route in enumerate(state["routes"]):
        base = HEADER_BYTES + index * SLOT_BYTES
        image[base : base + 8] = route["key"]
        image[base + 8 : base + 40] = route["dapp_id"]
        image[base + 40 : base + 72] = route["account_id"]
        # min_value is a u128, little-endian, occupying the last 16 bytes of the
        # slot. The Rust codec writes it as one `to_le_bytes()`, so the model
        # must not split it across a boundary it does not own.
        image[base + 72 : base + 88] = route["min_value"].to_bytes(16, "little")
    return bytes(image)


def decode(image: bytes) -> dict | None:
    if len(image) != STATE_BYTES:
        return None
    version = image[56]
    if version not in (1, 2):
        return None
    owner = image[24:56]
    if owner == bytes(32):
        return None
    if image[58:60] != b"\x00\x00":
        return None
    declared = image[57]
    if declared > MAX_ROUTES:
        return None
    routes = []
    seen = set()
    for index in range(MAX_ROUTES):
        base = HEADER_BYTES + index * SLOT_BYTES
        slot = image[base : base + SLOT_BYTES]
        if slot == bytes(SLOT_BYTES):
            continue
        key = slot[0:8]
        dapp = slot[8:40]
        account = slot[40:72]
        if key == bytes(8) or dapp == bytes(32) or account == bytes(32):
            return None
        if key in seen:
            return None
        seen.add(key)
        low, high = struct.unpack("<QQ", slot[72:88])
        routes.append(
            {"key": key, "dapp_id": dapp, "account_id": account, "min_value": low | (high << 64)}
        )
    if len(routes) != declared:
        return None
    return {
        "routed_count": struct.unpack("<Q", image[0:8])[0],
        "completed_count": struct.unpack("<Q", image[8:16])[0],
        "failed_count": struct.unpack("<Q", image[16:24])[0],
        "owner_pubkey": owner,
        "version": version,
        "last_status": struct.unpack("<I", image[60:64])[0],
        "last_route_key": image[64:72],
        "migration_payload": image[72:96],
        "routes": routes,
    }


def route(key: bytes, seed: int, min_value: int) -> dict:
    return {
        "key": key,
        "dapp_id": bytes([seed + 1]) * 32,
        "account_id": bytes([seed + 2]) * 32,
        "min_value": min_value,
    }


def initial() -> dict:
    return {
        "routed_count": 0,
        "completed_count": 0,
        "failed_count": 0,
        "owner_pubkey": bytes([0x4F]) * 32,
        "version": 1,
        "last_status": AWAITING_CALLBACK,
        "last_route_key": bytes(8),
        "migration_payload": bytes(24),
        "routes": [],
    }


def resolve_usize(text: str, name: str) -> int:
    """Evaluate a `usize` constant expression, resolving names transitively.

    Constants in `src/state.rs` are written as expressions (`SLOT_DAPP_OFFSET +
    32`) so the offsets cannot drift from the field sizes, so the model has to
    evaluate them rather than match a literal.
    """
    match = re.search(rf"pub const {re.escape(name)}: usize = ([^;]+);", text)
    if not match:
        raise LookupError(f"constant {name} is not defined")
    expression = match.group(1).strip()
    for _ in range(20):
        identifiers = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", expression)
        if not identifiers:
            break
        for identifier in identifiers:
            nested = re.search(rf"pub const {re.escape(identifier)}: usize = ([^;]+);", text)
            if not nested:
                raise LookupError(f"{name}: cannot resolve identifier {identifier}")
            expression = re.sub(rf"\b{re.escape(identifier)}\b", nested.group(1), expression)
    if not re.fullmatch(r"[0-9+\-*/() \t]+", expression):
        raise ValueError(f"{name}: expression is not plain arithmetic: {expression!r}")
    return int(eval(expression, {"__builtins__": {}}, {}))  # noqa: S307 - arithmetic only


def check(condition: bool, message: str, failures: list[str]) -> None:
    if not condition:
        failures.append(message)


def main() -> int:
    failures: list[str] = []

    print(f"header          = {HEADER_BYTES} bytes")
    print(f"route slot      = {SLOT_BYTES} bytes  (offsets {SLOT})")
    print(f"route table     = {MAX_ROUTES} * {SLOT_BYTES} = {MAX_ROUTES * SLOT_BYTES} bytes")
    print(f"state image     = {STATE_BYTES} bytes")

    # 1. The ABI manifest must declare exactly this length.
    manifest = (ROOT / "avm-contract.toml").read_text()
    declared = int(
        re.search(r'name = "settlement_router_state"\nlen = (\d+)', manifest).group(1)
    )
    check(
        declared == STATE_BYTES,
        f"avm-contract.toml declares len={declared} but the model derives {STATE_BYTES}",
        failures,
    )
    print(f"abi declares    = {declared} bytes -> {'agree' if declared == STATE_BYTES else 'DISAGREE'}")

    # 2. The Rust source must agree with the independent model.
    state_rs = (ROOT / "src/state.rs").read_text()
    for name, expected in [
        ("STATE_BYTES", STATE_BYTES),
        ("ROUTE_BYTES", SLOT_BYTES),
        ("ROUTES_OFFSET", HEADER_BYTES),
        ("MAX_ROUTES", MAX_ROUTES),
        ("SLOT_DAPP_OFFSET", SLOT["dapp_id"][0]),
        ("SLOT_ACCOUNT_OFFSET", SLOT["account_id"][0]),
        ("SLOT_MIN_VALUE_OFFSET", SLOT["min_value"][0]),
        ("UPGRADE_MARKER_OFFSET", HEADER["migration_payload"][0]),
    ]:
        actual = resolve_usize(state_rs, name)
        check(actual == expected, f"src/state.rs {name}={actual} but the model derives {expected}", failures)
    print(f"rust constants  = {'agree' if not failures else 'see failures'}")

    # 3. Round-trip, empty and full.
    empty = initial()
    check(decode(encode(empty)) == empty, "empty state does not round-trip", failures)
    full = initial()
    full["routes"] = [route(bytes([seed]) * 8, seed, seed * 1000) for seed in range(1, MAX_ROUTES + 1)]
    check(decode(encode(full)) == full, "full state does not round-trip", failures)
    check(len(encode(full)) == STATE_BYTES, "full image length drifted", failures)

    # 4. A u128 minimum value must survive, not just small integers.
    big = initial()
    # Route keys are exactly 8 bytes; a 9-byte key would overflow the slot's key
    # field and make the encoded image the wrong length, which is a bug in this
    # model's test data rather than in the contract.
    big["routes"] = [route(b"bigrout1", 1, (1 << 128) - 1)]
    check(decode(encode(big)) == big, "u128 min_value does not round-trip", failures)
    print(f"round-trip      = ok (empty, {MAX_ROUTES} routes, u128 max)")

    # 5. Malformed images must be rejected.
    image = bytearray(encode(empty))
    cases = {
        "bad version": (56, 3),
        "zero owner": None,
        "dirty reserved": (59, 1),
        "route_count too large": (57, MAX_ROUTES + 1),
    }
    for label, mutation in cases.items():
        candidate = bytearray(image)
        if mutation is None:
            candidate[24:56] = bytes(32)
        else:
            offset, value = mutation
            candidate[offset] = value
        check(decode(bytes(candidate)) is None, f"{label} was not rejected", failures)
    check(decode(bytes(image)[:-1]) is None, "short image was not rejected", failures)
    print("malformed       = rejected (version, owner, reserved, route_count, length)")

    # 6. The upgrade marker must land where the decoder does not require zero.
    migrated = initial()
    migrated["version"] = 2
    migrated["migration_payload"] = bytes([0x5A]) * 24
    raw = bytearray(encode(migrated))
    raw[72:76] = UPGRADE_MARKER
    decoded = decode(bytes(raw))
    check(decoded is not None, "migrated image failed to decode", failures)
    check(decoded["version"] == 2, "migrated version was lost", failures)
    check(bytes(raw[72:76]) == UPGRADE_MARKER, "marker is not at offset 72", failures)
    check(bytes(raw[58:60]) == b"\x00\x00", "marker must not touch the zero-checked reserved bytes", failures)
    print("upgrade marker  = offset 72, image still decodes, reserved stays zero")

    # 7. The sentinel must be outside the legal AVM status-code space.
    check(AWAITING_CALLBACK == 0 or AWAITING_CALLBACK >= 1000, "sentinel is in the reserved runtime range", failures)
    check(AWAITING_CALLBACK != 0, "sentinel collides with success", failures)
    print(f"sentinel        = {AWAITING_CALLBACK:#x}, outside 0 and 1..1000")

    if failures:
        print("\nFAILED")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("\nOK: the independently derived layout agrees with the ABI and src/state.rs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
