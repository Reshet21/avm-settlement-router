#!/usr/bin/env python3
"""Derive every method id and event class from its ABI name and verify it.

The platform derives dispatch ids from names:

    method_id = hash_u64_le(name) = u64::from_le_bytes(BLAKE3(name)[:8])

The same rule derives `event_class` from the event name and the reserved
upgrade id from `"avm.code_upgrade"`. This tool makes the repository
self-checking against that rule:

* names are taken from `avm-contract.toml` (`[[abi.methods]]`,
  `[[abi.events]]`), never hard-coded here;
* every name must have a matching constant in `src/wire.rs`, documented as
  `hash_u64_le("name")` — a method added to the ABI without a constant, or a
  constant without the annotation, is a failure;
* every constant value must equal the hash of its documented name;
* witness vectors taken from the SDK's own fixtures pin the rule itself.

Usage:

    python3 scripts/derive_ids.py          # print the table
    python3 scripts/derive_ids.py --check  # exit 1 on any mismatch

Requires the `blake3` module (`pip install blake3`).
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def derive(name: str) -> int:
    try:
        import blake3  # type: ignore
    except ImportError:
        raise SystemExit(
            "the blake3 module is required to verify method ids"
            " (pip install blake3); refusing to skip the check"
        )
    digest = blake3.blake3(name.encode()).digest(8)
    return int.from_bytes(digest, "little")


# Evidence that the rule is the right one: constants taken verbatim from
# gosh-sh/avm-sdk fixtures and runtime. If any of these stops matching, the
# rule — not the repository — is wrong.
WITNESSES = {
    "causal.a.version": 0x0DC1FEE693A5219C,
    "causal.a.deploy": 0x1608E0E0319B4D0F,
    "causal.b.drain": 0x4C2D0105CA924509,
    "causal.a.forwarded": 0x71228A3DC1799631,
    "causal.a.state": 0x127E58F93E9D40F8,
    "avm.code_upgrade": 0xC2C6C51BC610249D,
}


def wire_constants(source: str) -> dict[str, tuple[str | None, int]]:
    """Map constant name -> (annotated hash name or None, value).

    A constant is annotated when the doc comment immediately above it
    contains `hash_u64_le("...")`.
    """
    constants: dict[str, tuple[str | None, int]] = {}
    pending_name: str | None = None
    for line in source.splitlines():
        match = re.search(r'hash_u64_le\("([^"]+)"\)', line)
        if match:
            pending_name = match.group(1)
            continue
        match = re.match(r"pub const ([A-Z0-9_]+): u64 = (0x[0-9a-fA-F_]+);", line)
        if match:
            constants[match.group(1)] = (
                pending_name,
                int(match.group(2).replace("_", ""), 16),
            )
            pending_name = None
            continue
        if line.strip() and not line.lstrip().startswith("///"):
            pending_name = None
    return constants


def manifest_names(manifest: str) -> tuple[list[str], list[str]]:
    """Collect method names and event names from the ABI manifest."""
    methods: list[str] = []
    events: list[str] = []
    section = None
    for line in manifest.splitlines():
        header = re.match(r"\[\[abi\.([a-z.]+)\]\]", line)
        if header:
            section = header.group(1)
            continue
        if section in ("methods", "events"):
            match = re.match(r'name = "([^"]+)"', line)
            if match:
                (methods if section == "methods" else events).append(match.group(1))
    return methods, events


def main() -> int:
    check = "--check" in sys.argv[1:]
    wire = (ROOT / "src/wire.rs").read_text()
    manifest = (ROOT / "avm-contract.toml").read_text()

    constants = wire_constants(wire)
    methods, events = manifest_names(manifest)

    problems: list[str] = []
    rows: list[tuple[str, str, str, str]] = []
    seen: set[str] = set()

    def expect(abi_name: str, kind: str) -> None:
        holders = [
            (const, annotated, value)
            for const, (annotated, value) in constants.items()
            if annotated == abi_name
        ]
        if not holders:
            problems.append(
                f"{kind} {abi_name!r} has no hash_u64_le(\"...\")-annotated"
                f" constant in src/wire.rs"
            )
            rows.append((abi_name, "-", f"0x{derive(abi_name):016x}", "MISSING"))
            return
        if len(holders) > 1:
            problems.append(f"{abi_name!r} is annotated on several constants")
        for const, _annotated, value in holders:
            seen.add(const)
            derived = derive(abi_name)
            flag = "ok" if value == derived else "MISMATCH"
            if flag != "ok":
                problems.append(
                    f"{const} = 0x{value:016x} but hash_u64_le({abi_name!r}) = "
                    f"0x{derived:016x}"
                )
            rows.append((abi_name, const, f"0x{derived:016x}", flag))

    # Rule witnesses first: if these fail, stop trusting the rule itself.
    for name, expected in WITNESSES.items():
        actual = derive(name)
        if actual != expected:
            problems.append(
                f"hash witness broken: hash_u64_le({name!r}) = 0x{actual:016x},"
                f" expected 0x{expected:016x}"
            )

    for name in methods:
        expect(name, "method")
    for name in events:
        expect(name, "event")

    # Outbound ids are sender-chosen but documented by name; they must stay
    # annotated and value-consistent even though the ABI does not declare
    # them. Any id constant without an annotation is a failure.
    for const, (annotated, value) in sorted(constants.items()):
        if annotated is None:
            problems.append(
                f"{const} is an id constant without a hash_u64_le(\"...\")"
                " annotation; name it"
            )
            continue
        if const in seen:
            continue
        derived = derive(annotated)
        flag = "ok" if value == derived else "MISMATCH"
        if flag != "ok":
            problems.append(
                f"{const} = 0x{value:016x} but hash_u64_le({annotated!r}) = "
                f"0x{derived:016x}"
            )
        rows.append((annotated, const, f"0x{derived:016x}", flag))

    width = max((len(row[0]) for row in rows), default=4)
    print(f"{'name':{width}}  {'constant':28}  {'hash_u64_le(name)':18}  status")
    for abi_name, const, derived, flag in rows:
        print(f"{abi_name:{width}}  {const:28}  {derived:18}  {flag}")
    for problem in problems:
        print(f"FAIL: {problem}", file=sys.stderr)
    print(f"{len(rows)} identifiers checked, {len(problems)} problems")
    return 1 if problems and check else 0


if __name__ == "__main__":
    raise SystemExit(main())
