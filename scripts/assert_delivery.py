#!/usr/bin/env python3
"""Asserts that a settle call completed a *successful* delivery round trip.

Reads the call JSON captured by `scripts/stand-e2e.sh` (plus the events
journal) and checks the outcome of the delivery, not just `status: ok` of the
command:

* `settlement.router.settled` carries `settled: true` and `status_code: 0`;
* `completed_count` grows and `failed_count` stays 0, both in the event fields
  and in the returned state image (routed 0..8, completed 8..16, failed 16..24,
  last_status 60..64 per `src/state.rs`);
* `settlement.router.routed` reports the settled value;
* the beneficiary's `settlement.beneficiary.payout` event landed in the journal
  with the same route key and amount.

Event field tables use the canonical `encode_event_fields` layout from the SDK
(`u16 count, u16 0, [u16 name_len, u8 kind, u16 value_len, name, value]`,
names sorted), which the executor output embeds verbatim.
"""

import json
import re
import struct
import sys

PAYOUT_EVENT_CLASS = "3b94d38ab8ef4554"  # 0x5445efb88ad3943b, little-endian
ROUTE_KEY_HEX = "726f7574652d3031"  # "route-01"


def parse_table(blob, start):
    count, zero = struct.unpack_from("<HH", blob, start)
    if zero != 0 or count == 0 or count > 12:
        raise ValueError("not a field table")
    off = start + 4
    fields = {}
    previous = ""
    for _ in range(count):
        nlen, kind, vlen = struct.unpack_from("<HBH", blob, off)
        off += 5
        if nlen == 0 or nlen > 64 or kind > 8 or vlen > 4096:
            raise ValueError("field header out of range")
        name = blob[off : off + nlen].decode("utf-8")
        off += nlen
        if not re.fullmatch(r"[a-z][a-z0-9_]{0,63}", name):
            raise ValueError("implausible field name")
        if name <= previous:
            raise ValueError("names not sorted")
        previous = name
        value = blob[off : off + vlen]
        if len(value) != vlen:
            raise ValueError("truncated value")
        off += vlen
        if kind == 0:
            fields[name] = None
        elif kind == 1:
            fields[name] = bool(value[0])
        elif kind in (2, 3, 4, 5):
            fields[name] = int.from_bytes(value, "little")
        elif kind == 6:
            fields[name] = value.decode("utf-8")
        else:
            fields[name] = value.hex()
    return fields, off


def field_tables(blob):
    tables = []
    for start in range(max(0, len(blob) - 4)):
        try:
            fields, end = parse_table(blob, start)
        except (ValueError, UnicodeDecodeError, struct.error):
            continue
        tables.append((start, end, fields))
    return tables


def fail(message):
    print(f"ASSERT-DELIVERY FAILED: {message}", file=sys.stderr)
    sys.exit(1)


def main():
    if len(sys.argv) != 6:
        fail("usage: assert_delivery.py SETTLE_JSON EVENTS_JSON ROUTED COMPLETED VALUE")
    settle_path, events_path, exp_routed, exp_completed, exp_value = sys.argv[1:6]
    exp_routed, exp_completed, exp_value = int(exp_routed), int(exp_completed), int(exp_value)

    call = json.load(open(settle_path))
    if call.get("status") != "ok":
        fail(f"the settle call itself failed: {call.get('status')}")
    raw = call.get("raw_executor_output", "")
    blob = bytes.fromhex(raw[2:] if raw.startswith("0x") else raw)
    tables = [t for _, _, t in field_tables(blob)]

    settled = [t for t in tables if "settled" in t and "status_code" in t]
    if not settled:
        fail("no settlement.router.settled field table in the executor output")
    event = settled[0]
    if event["settled"] is not True:
        fail(f"settled == {event['settled']!r}, expected true")
    if event["status_code"] != 0:
        fail(f"status_code == {event['status_code']}, expected 0")
    if event["completed_count"] != exp_completed:
        fail(f"completed_count == {event['completed_count']}, expected {exp_completed}")
    if event["failed_count"] != 0:
        fail(f"failed_count == {event['failed_count']}, expected 0")

    routed = [t for t in tables if "routed_count" in t and "value_main" in t]
    if not routed:
        fail("no settlement.router.routed field table in the executor output")
    if routed[0]["routed_count"] != exp_routed:
        fail(f"routed_count == {routed[0]['routed_count']}, expected {exp_routed}")
    if routed[0]["value_main"] != exp_value:
        fail(f"value_main == {routed[0]['value_main']}, expected {exp_value}")

    state_hex = call.get("state") or call.get("raw_state") or ""
    state = bytes.fromhex(state_hex[2:] if state_hex.startswith("0x") else state_hex)
    if len(state) < 64:
        fail(f"state image too short: {len(state)} bytes")
    got = (
        int.from_bytes(state[0:8], "little"),
        int.from_bytes(state[8:16], "little"),
        int.from_bytes(state[16:24], "little"),
    )
    if got != (exp_routed, exp_completed, 0):
        fail(f"state counters {got}, expected ({exp_routed}, {exp_completed}, 0)")
    last_status = int.from_bytes(state[60:64], "little")
    if last_status != 0:
        fail(f"last_status == {last_status:#x}, expected 0 (settlement closed)")

    journal = json.load(open(events_path))
    hexes = journal.get("events", [])
    value_le = exp_value.to_bytes(16, "little").hex()[:16]
    payouts = [
        h
        for h in hexes
        if PAYOUT_EVENT_CLASS in h and ROUTE_KEY_HEX in h and value_le in h
    ]
    if not payouts:
        fail("no settlement.beneficiary.payout event with the route key and amount in the journal")

    print(
        f"ASSERT-DELIVERY OK: settled=true status_code=0 "
        f"routed={exp_routed} completed={exp_completed} failed=0 value={exp_value} "
        f"payout_events={len(payouts)}"
    )


if __name__ == "__main__":
    main()
