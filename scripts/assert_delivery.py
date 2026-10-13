#!/usr/bin/env python3
"""Asserts that a settle call completed a *successful* delivery round trip.

Reads the call JSON captured by `scripts/stand-e2e.sh` (plus the events
journal) and checks the outcome of the delivery, not just `status: ok` of the
command:

* the returned state image shows routed and completed counters growing with
  `failed_count = 0` and `last_status = 0` (fixed offsets 0/8/16/60 per
  `src/state.rs`);
* the call's `settlement.router.settled` journal frame carries the
  `settled = true` and `status_code = 0` field values;
* the beneficiary's `settlement.beneficiary.payout` journal frame carries the
  route key and the settled amount.

The journal event frames use an undocumented stand-side layout (NUL-terminated
field names with padded value slots). The value patterns asserted here were
verified byte-for-byte against the stand reports of successful runs (both
settles of run 38347037635): a bounce would carry `settled = 0` and a non-zero
`status_code` slot, so the patterns fail closed on any delivery failure.
"""

import json
import sys

PAYOUT_EVENT_CLASS = bytes.fromhex("3b94d38ab8ef4554")  # 0x5445efb88ad3943b
ROUTE_KEY = b"route-01"

# `settled` field value 1 (true) immediately followed by the `status_code`
# field with a zero value slot — the tail of a successful settled frame
# (identical in both settles of run 38347037635):
# b'settled\x00' + 01 00 01 00 + 00*4 + b'status_code\x00' + 00*4 + 02 00 00 00 + 00*4
SETTLED_SUCCESS_TAIL = bytes.fromhex(
    "736574746c65640001000100000000007374617475735f636f646500000000000200000000000000"
)
SETTLED_EVENT_NAME = b"settlement.router.settled"
PAYOUT_EVENT_NAME = b"settlement.beneficiary.payout"


def fail(message):
    print(f"ASSERT-DELIVERY FAILED: {message}", file=sys.stderr)
    sys.exit(1)


def frames_of(hexes):
    out = []
    for h in hexes:
        try:
            out.append(bytes.fromhex(h[2:] if h.startswith("0x") else h))
        except ValueError:
            fail(f"journal event is not hex: {h[:40]!r}")
    return out


def main():
    if len(sys.argv) != 6:
        fail("usage: assert_delivery.py SETTLE_JSON EVENTS_JSON ROUTED COMPLETED VALUE")
    settle_path, events_path, exp_routed, exp_completed, exp_value = sys.argv[1:6]
    exp_routed, exp_completed, exp_value = int(exp_routed), int(exp_completed), int(exp_value)

    call = json.load(open(settle_path))
    if call.get("status") != "ok":
        fail(f"the settle call itself failed: {call.get('status')}")

    # 1. The state image is the authoritative outcome (fixed offsets).
    state_hex = call.get("raw_state") or call.get("state") or ""
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

    # 2. The settled event of this call must report settled=true, status_code=0.
    frames = frames_of(call.get("events", []))
    settled_frames = [f for f in frames if SETTLED_EVENT_NAME in f]
    if not settled_frames:
        fail("no settlement.router.settled frame in the call's events")
    frame = settled_frames[-1]
    if SETTLED_SUCCESS_TAIL not in frame:
        fail(
            "the settled frame does not carry settled=true / status_code=0; "
            f"frame tail: {frame[-64:].hex()}"
        )

    # 3. The beneficiary's payout event with the route key and the value.
    value_le = exp_value.to_bytes(8, "little")
    payouts = [
        f
        for f in frames
        if PAYOUT_EVENT_CLASS in f and PAYOUT_EVENT_NAME in f and ROUTE_KEY in f and value_le in f
    ]
    if not payouts:
        fail("no settlement.beneficiary.payout frame with the route key and amount")

    # 4. Cross-check the cumulative journal when available.
    journal = frames_of(json.load(open(events_path)).get("events", []))
    journal_payouts = [f for f in journal if PAYOUT_EVENT_CLASS in f and PAYOUT_EVENT_NAME in f]
    if len(journal_payouts) < exp_routed:
        fail(
            f"the journal holds {len(journal_payouts)} payout events, "
            f"expected {exp_routed}"
        )

    print(
        f"ASSERT-DELIVERY OK: settled=true status_code=0 "
        f"routed={exp_routed} completed={exp_completed} failed=0 value={exp_value} "
        f"payout_events={len(journal_payouts)}"
    )


if __name__ == "__main__":
    main()
