# Method Identifiers — Read Before Deploying

## The problem this document exists for

AVM dispatches a message on the opaque little-endian `u64` `method_id` in the
message header. The contract branches on that number, not on a name:

```rust
match input.message.method_id() {
    SETTLE_METHOD_ID => settle(input),
    ...
}
```

`avm-contract.toml` declares the human-readable names, and `avm-local-stand call
--method <name>` takes a name. Something therefore has to resolve a name to the
number the contract expects. **That resolution lives in `avm-contract-build` and
the stand, which are distributed as release binaries built from the private
`gosh-sh/acki-nacki` repository.** This repository cannot see that derivation.

## What the SDK examples show

The checked-in fixtures pick method ids that are *not* derived from their names:

| Example | ABI method name | id used in source |
| --- | --- | --- |
| `contract-a-v1` | `causal.a.version` | `0x0dc1_fee6_93a5_219c` |
| `contract-b` | (payout method, not in the ABI list) | `0x0dc1_fee6_93a5_219c` |

The same 64-bit value appears for two different names in two different
contracts, so the id is contract-chosen data rather than a hash of the name. The
SDK itself fixes exactly one id, for the reserved account-local upgrade:

```rust
pub const CODE_UPGRADE_METHOD_ID: u64 = 0xc2c6_c51b_c610_249d;
```

## What this contract does

`src/wire.rs` fixes every id explicitly. Nothing is computed at build time, so
the values are stable and reviewable:

| ABI method name | Constant | Value |
| --- | --- | --- |
| `settlement.router.settle` | `SETTLE_METHOD_ID` | `0x5a17e3c09d426b1f` |
| `settlement.router.completed` | `CALLBACK_METHOD_ID` | `0x2f6b18d47ca950e3` |
| `settlement.router.route.set` | `ROUTE_SET_METHOD_ID` | `0x6c9341a7e528b0d6` |
| `settlement.router.route.clear` | `ROUTE_CLEAR_METHOD_ID` | `0x0b74d25e93fa18c7` |
| `settlement.router.withdraw` | `WITHDRAW_METHOD_ID` | `0x3de097514bf6a2c8` |
| `settlement.router.version` | `VERSION_METHOD_ID` | `0x71a4c9062e58d3fb` |
| — outbound delivery | `SETTLEMENT_DELIVERY_METHOD_ID` | `0x1f2e3d4c5b6a7988` |
| — owner payout | `PAYOUT_METHOD_ID` | `0x4c2d0105ca924509` |
| — reserved upgrade | `CODE_UPGRADE_METHOD_ID` (SDK) | `0xc2c6c51bc610249d` |

Event classes are fixed the same way in `src/wire.rs`:
`ROUTED_EVENT_CLASS`, `SETTLED_EVENT_CLASS`, `ROUTE_EVENT_CLASS`,
`WITHDRAWN_EVENT_CLASS`.

## How to confirm the ids against your stand build

Run this once after `./scripts/install.sh` from the SDK distribution, before
trusting a deployment:

```bash
export AVM_SDK_HOME="${AVM_SDK_HOME:-$HOME/.local/share/avm-sdk}"
export AVM_STATE="${PWD}/probe.state"
export AVM_PROFILE="${AVM_SDK_HOME}/current/profiles/wasmtime-release-profile.v1"
export DAPP_ID="1111111111111111111111111111111111111111111111111111111111111111"

avm-local-stand init --state "$AVM_STATE" --wasmtime-profile "$AVM_PROFILE" \
  --dapp-id "$DAPP_ID" > init.json

# Build the router, deploy it, then call the version probe. The probe is
# read-only in effect, so it is the cheapest id to test.
avm-contract-build build --manifest-path Cargo.toml
ARTIFACTS=target/avm

# Deploy (see docs/using-the-stand.md for the two-phase funded deploy), then:
avm-local-stand call --state "$AVM_STATE" --wasmtime-profile "$AVM_PROFILE" \
  --address "$CONTRACT_ADDRESS" \
  --method settlement.router.version \
  --args-json null --identity user \
  --value-main 0 --extra-currencies '' \
  --block-time 1800000010 --created-at 1800000009 \
  --expire-at 1800003609 --fuel-fee-shells 1000
```

Interpret the result:

- **status 0** — the name resolved to `VERSION_METHOD_ID`. The table above
  matches your stand build; the other ids come from the same source and are
  equally good.
- **status 1606 (`invalid_version_probe`)** — the id resolved, but the call
  shape was wrong. Check that `--value-main` is 0 and `--args-json` is `null`.
- **status 1600 (`invalid_settlement`)** — the contract fell through to the
  unknown-method arm, so the resolved id is *not* `VERSION_METHOD_ID`. Edit
  `VERSION_METHOD_ID` in `src/wire.rs` to the id your stand sends and rebuild.
- **a runtime-level rejection rather than a contract status** — the ABI does not
  declare that method name; check `avm-contract.toml`.

To find the id empirically instead of by interpretation, temporarily add a
catch-all that reports the received id in an event field, call any method, and
read the id back from `avm-local-stand events`.

## Rules that must keep holding

- Never reuse one id for two ABI methods. `src/wire.rs` has a unit test
  (`identifiers_are_unique`) that fails the build if a duplicate is introduced.
- Never use `0`. The canonical decoder rejects a zero callback handler id, and
  `0` is not a valid dispatch target.
- Never use a value in `1..1000` for a status code; the SDK reserves that range
  for the runtime. Contract codes here start at `1600`.
- Keep `CODE_UPGRADE_METHOD_ID` imported from the SDK rather than redefining it,
  so a future SDK change cannot silently desynchronize the upgrade path.
