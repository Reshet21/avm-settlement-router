#!/usr/bin/env python3
"""Static consistency checks for the settlement router.

There is no Rust toolchain in every environment this repository gets edited in,
so this script catches the classes of mistake a compiler would otherwise catch:
unbalanced delimiters, cross-module references that do not resolve, test imports
that name nothing, and ABI/state numbers that disagree with the source.

It is deliberately conservative: it checks names against definitions with a
regex, so it can produce false positives on unusual syntax. Run it with:

    python3 scripts/check_consistency.py
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SRC = ROOT / "src"
TESTS = ROOT / "tests"

FAILURES: list[str] = []


def fail(message: str) -> None:
    FAILURES.append(message)


def strip_noise(text: str) -> str:
    """Remove comments, string literals, and char literals."""
    text = re.sub(r"/\*.*?\*/", "", text, flags=re.S)
    text = re.sub(r"^\s*//(?!/).*$", "", text, flags=re.M)
    text = re.sub(r"^\s*//!.*$", "", text, flags=re.M)
    text = re.sub(r"\"(?:[^\"\\]|\\.)*\"", '""', text)
    text = re.sub(r"'(?:[^'\\]|\\.)'", "' '", text)
    return text


def check_delimiters(path: pathlib.Path, text: str) -> None:
    stripped = strip_noise(text)
    for opener, closer in [("{", "}"), ("(", ")"), ("[", "]")]:
        count = stripped.count(opener) - stripped.count(closer)
        if count != 0:
            fail(f"{path}: unbalanced {opener}{closer}, delta {count}")


def resolve_usize(text: str, name: str) -> int:
    """Evaluate a `usize` constant expression, resolving names transitively."""
    match = re.search(rf"pub const {re.escape(name)}: usize = ([^;]+);", text)
    if not match:
        fail(f"constant {name} is not defined")
        return -1
    expression = match.group(1).strip()
    for _ in range(20):
        identifiers = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", expression)
        if not identifiers:
            break
        for identifier in identifiers:
            nested = re.search(rf"pub const {re.escape(identifier)}: usize = ([^;]+);", text)
            if not nested:
                fail(f"{name}: cannot resolve identifier {identifier}")
                return -1
            expression = re.sub(rf"\b{re.escape(identifier)}\b", nested.group(1), expression)
    expression = expression.replace("usize::MAX", "0")
    if not re.fullmatch(r"[0-9+\-*/() \t]+", expression):
        fail(f"{name}: expression is not plain arithmetic: {expression!r}")
        return -1
    try:
        return int(eval(expression, {"__builtins__": {}}, {}))  # noqa: S307 - arithmetic only
    except Exception as error:  # pragma: no cover - defensive
        fail(f"{name}: cannot evaluate {expression!r}: {error}")
        return -1


def defined_names(text: str) -> set[str]:
    names: set[str] = set()
    patterns = [
        r"\bpub(?:\([^)]*\))?\s+const\s+(\w+)",
        r"\bpub(?:\([^)]*\))?\s+fn\s+(\w+)",
        r"\bpub(?:\([^)]*\))?\s+struct\s+(\w+)",
        r"\bpub(?:\([^)]*\))?\s+enum\s+(\w+)",
        r"\bpub(?:\([^)]*\))?\s+trait\s+(\w+)",
        r"\bpub(?:\([^)]*\))?\s+mod\s+(\w+)",
        r"\bpub(?:\([^)]*\))?\s+type\s+(\w+)",
        r"\bpub(?:\([^)]*\))?\s+static\s+(\w+)",
        r"\bfn\s+(\w+)",
        r"\bstruct\s+(\w+)",
        r"\benum\s+(\w+)",
        r"\bmod\s+(\w+)",
        r"\bconst\s+(\w+)",
    ]
    for pattern in patterns:
        names.update(re.findall(pattern, text))
    # struct/enum variants and fields
    names.update(re.findall(r"^\s{4}(\w+)\s*(?::|,|\{)", text, flags=re.M))
    return names


def main() -> int:
    modules = {name: SRC / f"{name}.rs" for name in ("lib", "state", "wire", "router")}
    for name, path in modules.items():
        if not path.exists():
            fail(f"missing module src/{name}.rs")
    if FAILURES:
        report()
        return 1

    sources = {name: path.read_text() for name, path in modules.items()}
    for name, text in sources.items():
        check_delimiters(modules[name], text)

    defined = {name: defined_names(strip_noise(text)) for name, text in sources.items()}
    # `status` is a nested module in wire.rs; collect its constants too.
    defined["wire"].update(re.findall(r"pub const (\w+): u32", sources["wire"]))

    # --- cross-module `crate::<mod>::<name>` and `use crate::<mod>::<name>` ---
    for name, text in sources.items():
        for used_mod, symbol in re.findall(r"crate::(\w+)::(\w+)", text):
            if used_mod in ("state", "wire", "router"):
                if symbol not in defined[used_mod]:
                    fail(f"src/{name}.rs: crate::{used_mod}::{symbol} is not defined")
        for used_mod, symbol in re.findall(r"^use crate::(\w+)::(\w+);", text, flags=re.M):
            if used_mod in ("state", "wire", "router"):
                if symbol not in defined[used_mod]:
                    fail(f"src/{name}.rs: use crate::{used_mod}::{symbol} is not defined")
        # bare `use crate::<mod>::status;` then `status::FOO`
        for const in re.findall(r"status::(\w+)", text):
            if const not in defined["wire"]:
                fail(f"src/{name}.rs: status::{const} is not defined in wire.rs")

    # --- every imported SDK symbol must exist in the vendored SDK ---
    sdk_lib = (ROOT / "vendor/avm-contract-sdk/src/lib.rs").read_text()
    sdk_exports = set(re.findall(r"pub use \w+::\{([^}]*)\}", sdk_lib, flags=re.S))
    sdk_names: set[str] = set()
    for group in sdk_exports:
        sdk_names.update(part.strip().split(" as ")[0].strip() for part in group.split(","))
    sdk_names.update(re.findall(r"^pub (?:const|type|struct|enum|trait|fn) (\w+)", sdk_lib, flags=re.M))
    sdk_names.update(re.findall(r"pub use (\w+)::", sdk_lib))
    sdk_names |= {"export_message_contract", "export_batch_contract", "GuestArena"}

    for path in [SRC / f"{n}.rs" for n in modules] + sorted(TESTS.glob("*.rs")):
        text = path.read_text()
        for symbol in re.findall(r"^use avm_contract_sdk::(\w+);", text, flags=re.M):
            if symbol not in sdk_names:
                fail(f"{path}: avm_contract_sdk::{symbol} is not exported by the SDK")

    # --- test imports of our own crate must resolve ---
    for path in sorted(TESTS.glob("*.rs")):
        text = path.read_text()
        check_delimiters(path, text)
        for used_mod, symbol in re.findall(
            r"^use avm_settlement_router::(\w+)::(\w+);", text, flags=re.M
        ):
            if used_mod in defined and symbol not in defined[used_mod]:
                fail(f"{path}: avm_settlement_router::{used_mod}::{symbol} is not defined")
        for symbol in re.findall(r"^use avm_settlement_router::(\w+);", text, flags=re.M):
            if symbol not in defined["lib"]:
                fail(f"{path}: avm_settlement_router::{symbol} is not defined in lib.rs")
        # fully qualified paths used inline
        for used_mod, symbol in re.findall(r"avm_settlement_router::(\w+)::(\w+)", text):
            if used_mod in defined and symbol not in defined[used_mod]:
                fail(f"{path}: inline avm_settlement_router::{used_mod}::{symbol} is undefined")

    # --- ABI manifest must agree with the source ---
    manifest = (ROOT / "avm-contract.toml").read_text()
    state_bytes = resolve_usize(sources["state"], "STATE_BYTES")
    declared_len = int(re.search(r'name = "settlement_router_state"\nlen = (\d+)', manifest).group(1))
    if state_bytes != declared_len:
        fail(f"avm-contract.toml declares len={declared_len} but STATE_BYTES={state_bytes}")

    max_routes = resolve_usize(sources["state"], "MAX_ROUTES")
    route_bytes = resolve_usize(sources["state"], "ROUTE_BYTES")
    routes_offset = resolve_usize(sources["state"], "ROUTES_OFFSET")
    if routes_offset + max_routes * route_bytes != state_bytes:
        fail(
            "state layout does not add up: "
            f"{routes_offset} + {max_routes}*{route_bytes} != {state_bytes}"
        )

    # route-set body length must equal key + address + u128
    route_set = resolve_usize(sources["wire"], "ROUTE_SET_ARGS_BYTES")
    settle_args = resolve_usize(sources["wire"], "SETTLE_ARGS_BYTES")
    if route_set != 8 + 64 + 16:
        fail(f"ROUTE_SET_ARGS_BYTES should be 88, got {route_set}")
    if settle_args != 8 + 64:
        fail(f"SETTLE_ARGS_BYTES should be 72, got {settle_args}")

    # every status code in wire.rs must appear in the ABI errors table
    status_module = re.search(
        r"pub mod status \{(.*?)\n\}", sources["wire"], flags=re.S
    )
    if not status_module:
        fail("wire.rs has no `pub mod status` block")
        report()
        return 1
    wire_codes = set(re.findall(r"pub const \w+: u32 = (\d+);", status_module.group(1)))
    abi_codes = set(re.findall(r"status_code = (\d+)", manifest))
    missing = wire_codes - abi_codes
    if missing:
        fail(f"status codes defined in wire.rs but absent from avm-contract.toml: {sorted(missing)}")
    for code in sorted(wire_codes | abi_codes):
        if 1 <= int(code) < 1000:
            fail(f"status code {code} collides with the SDK-reserved runtime range 1..1000")

    # every ABI method name must be documented in docs/method-ids.md
    method_names = re.findall(r'name = "(settlement\.router\.[^"]+)"', manifest)
    ids_doc = (ROOT / "docs/method-ids.md").read_text()
    for method in method_names:
        if method not in ids_doc:
            fail(f"ABI method {method} is not documented in docs/method-ids.md")

    # method id constants must be unique and non-zero
    ids = re.findall(r"pub const \w+_METHOD_ID: u64 = (0x[0-9a-f_]+);", sources["wire"])
    values = [int(value.replace("_", ""), 16) for value in ids]
    if 0 in values:
        fail("a method id is zero, which is not a valid dispatch target")
    if len(set(values)) != len(values):
        fail("duplicate method ids in wire.rs")
    classes = [
        int(value.replace("_", ""), 16)
        for value in re.findall(r"pub const \w+_EVENT_CLASS: u64 = (0x[0-9a-f_]+);", sources["wire"])
    ]
    if len(set(classes)) != len(classes):
        fail("duplicate event classes in wire.rs")

    check_hash_derived_ids(sources["wire"])

    # --- E2E failure-flow marker protocol (see README) ---
    # `examples/contract-b` in gosh-sh/avm-sdk defines this convention: four
    # marker pairs delimit the payloads the E2E harness may rewrite, and the
    # hook must keep calling the helper.
    lib = sources["lib"]
    for region in ("FILE", "RUSTFMT", "HOOK", "HELPER"):
        begin = f"/* AVM_E2E_FAILURE_FLOW_{region}_BEGIN */"
        end = f"/* AVM_E2E_FAILURE_FLOW_{region}_END */"
        if lib.count(begin) != 1 or lib.count(end) != 1:
            fail(f"src/lib.rs must contain exactly one {region} marker pair")
        elif lib.index(begin) > lib.index(end):
            fail(f"src/lib.rs {region} markers are out of order")
    hook_payload = re.search(
        r"AVM_E2E_FAILURE_FLOW_HOOK_BEGIN \*/(.*?) /\* AVM_E2E_FAILURE_FLOW_HOOK_END",
        lib,
        flags=re.S,
    )
    if not hook_payload or "failure_flow(&input)" not in hook_payload.group(1):
        fail("the failure-flow hook payload must call failure_flow(&input)")
    after_helper = lib.split("AVM_E2E_FAILURE_FLOW_HELPER_BEGIN")[-1]
    if "fn failure_flow" not in after_helper.split("AVM_E2E_FAILURE_FLOW_HELPER_END")[0]:
        fail("the failure-flow helper must live inside the HELPER markers")
    if 'option_env!("AVM_E2E_FAILURE_FLOW")' not in after_helper:
        fail("the failure-flow helper must gate on the AVM_E2E_FAILURE_FLOW build flag")

    report()
    return 1 if FAILURES else 0


# The platform derives dispatch ids from names: `method_id = hash_u64_le(name)`,
# which is the first eight bytes of BLAKE3(name) read little-endian. The same
# rule derives `event_class` from the event name and the reserved upgrade id
# from `"avm.code_upgrade"`. Every constant below must equal the hash of its
# name, so a renamed method can never keep a stale number.
HASH_DERIVED_IDS = {
    "SETTLE_METHOD_ID": "settlement.router.settle",
    "CALLBACK_METHOD_ID": "settlement.router.completed",
    "ROUTE_SET_METHOD_ID": "settlement.router.route.set",
    "ROUTE_CLEAR_METHOD_ID": "settlement.router.route.clear",
    "WITHDRAW_METHOD_ID": "settlement.router.withdraw",
    "VERSION_METHOD_ID": "settlement.router.version",
    "SETTLEMENT_DELIVERY_METHOD_ID": "settlement.beneficiary.receive",
    "PAYOUT_METHOD_ID": "settlement.owner.payout",
    "ROUTED_EVENT_CLASS": "settlement.router.routed",
    "SETTLED_EVENT_CLASS": "settlement.router.settled",
    "ROUTE_EVENT_CLASS": "settlement.router.route_changed",
    "WITHDRAWN_EVENT_CLASS": "settlement.router.withdrawn",
}

# Evidence that the derivation rule is the right one, taken from the SDK's own
# fixtures and the reserved upgrade constant.
WITNESSES = {
    "causal.a.version": 0x0DC1FEE693A5219C,
    "causal.a.forwarded": 0x71228A3DC1799631,
    "causal.a.state": 0x127E58F93E9D40F8,
    "avm.code_upgrade": 0xC2C6C51BC610249D,
}


def hash_u64_le(name: str) -> int:
    try:
        import blake3  # type: ignore
    except ImportError:
        raise RuntimeError(
            "the blake3 module is required to verify method ids"
            " (pip install blake3); refusing to skip the check"
        )
    digest = blake3.blake3(name.encode()).digest(8)
    return int.from_bytes(digest, "little")


def check_hash_derived_ids(wire_source: str) -> None:
    try:
        for name, expected in WITNESSES.items():
            actual = hash_u64_le(name)
            if actual != expected:
                fail(
                    f"hash witness broken: hash_u64_le({name!r}) = "
                    f"0x{actual:016x}, expected 0x{expected:016x}"
                )
        for constant, name in HASH_DERIVED_IDS.items():
            match = re.search(rf"pub const {constant}: u64 = (0x[0-9a-f_]+);", wire_source)
            if not match:
                fail(f"{constant} is not defined in wire.rs")
                continue
            actual = int(match.group(1).replace("_", ""), 16)
            expected = hash_u64_le(name)
            if actual != expected:
                fail(
                    f"{constant} = 0x{actual:016x} but hash_u64_le({name!r}) = "
                    f"0x{expected:016x}; method ids are name-derived and must match"
                )
    except RuntimeError as error:
        fail(str(error))


def report() -> None:
    if FAILURES:
        print("FAILED")
        for message in FAILURES:
            print(f"  - {message}")
    else:
        print("OK: delimiters, cross-module names, SDK imports, test imports, and ABI all agree")


if __name__ == "__main__":
    sys.exit(main())
