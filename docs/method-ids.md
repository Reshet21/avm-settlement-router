# Method Identifiers

## The derivation rule

AVM dispatches a message on the `u64` `method_id` in the message header, and
the platform derives that number from the method name:

```text
method_id = hash_u64_le(name) = u64::from_le_bytes(blake3(name)[..8])
```

The same rule derives `event_class` from the event name, and it is where the
reserved account-local upgrade id comes from:

```text
CODE_UPGRADE_METHOD_ID = hash_u64_le("avm.code_upgrade") = 0xc2c6c51bc610249d
```

`avm-contract-build` enforces this: declaring your own number is rejected by
the build (see `gosh-sh/avm-sdk` issue #2 — the open part of that issue is
that the toolchain does not *print* the derived constants for you).

## Evidence

The rule is verified against the SDK's own fixtures — `hash_u64_le` of each
name equals the constant checked into `gosh-sh/avm-sdk`:

| Name | Constant in the SDK | `hash_u64_le` |
| --- | --- | --- |
| `causal.a.version` | `0x0dc1fee693a5219c` (contract-a-v1) | ✅ match |
| `causal.a.deploy` | `0x1608e0e0319b4d0f` (contract-a-v1) | ✅ match |
| `causal.b.drain` | `0x4c2d0105ca924509` (contract-b) | ✅ match |
| `causal.a.forwarded` | `0x71228a3dc1799631` (contract-b, event class) | ✅ match |
| `causal.a.state` | `0x127e58f93e9d40f8` (contract-b, event class) | ✅ match |
| `avm.code_upgrade` | `0xc2c6c51bc610249d` (`avm-contract-sdk`) | ✅ match |

## This contract's constants

Every constant in `src/wire.rs` is `hash_u64_le` of its name:

| Name | Constant | Value |
| --- | --- | --- |
| `settlement.router.settle` | `SETTLE_METHOD_ID` | `0x82d7ea8f0e15a928` |
| `settlement.router.completed` | `CALLBACK_METHOD_ID` | `0x7ce9d7f515c05256` |
| `settlement.router.route.set` | `ROUTE_SET_METHOD_ID` | `0xae488411c32fcee2` |
| `settlement.router.route.clear` | `ROUTE_CLEAR_METHOD_ID` | `0xb0a9ef6db4c2017c` |
| `settlement.router.withdraw` | `WITHDRAW_METHOD_ID` | `0x3ad02292939eff9c` |
| `settlement.router.version` | `VERSION_METHOD_ID` | `0x8fabf5023447277c` |
| `settlement.beneficiary.receive` | `SETTLEMENT_DELIVERY_METHOD_ID` | `0x0ef6cc14d234ec6f` |
| `settlement.owner.payout` | `PAYOUT_METHOD_ID` | `0xf507acdcf6f5c790` |
| `settlement.router.routed` | `ROUTED_EVENT_CLASS` | `0xf45f52c6ebbc5b2e` |
| `settlement.router.settled` | `SETTLED_EVENT_CLASS` | `0x182e4064462de927` |
| `settlement.router.route_changed` | `ROUTE_EVENT_CLASS` | `0x0b076b3d59a4889c` |
| `settlement.router.withdrawn` | `WITHDRAWN_EVENT_CLASS` | `0xf322003813d7882c` |

`SETTLEMENT_DELIVERY_METHOD_ID` and `PAYOUT_METHOD_ID` name methods of the
*destination* contracts, so they are not declared in this contract's
`avm-contract.toml`; they follow the same rule.

## No drift, ever

`scripts/derive_ids.py --check` takes every method and event name from
`avm-contract.toml`, recomputes `hash_u64_le` for each, and fails if the
matching constant in `src/wire.rs` is missing, unannotated, or disagrees; it
re-checks the six witness rows first, so if the derivation rule itself is ever
wrong, the check says so explicitly instead of failing obscurely. A method
added to the ABI without a named constant fails the same check. It runs in CI
on every push (it needs the `blake3` Python module):

## Reproducing a single number

```bash
python3 - <<'PY'
import blake3
name = "settlement.router.settle"
print(hex(int.from_bytes(blake3.blake3(name.encode()).digest(8), "little")))
PY
```
