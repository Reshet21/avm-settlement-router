# avm-settlement-router

A Rust contract for the **Acki Nacki AVM** that routes settlement value to an
owner-administered routing table.

Built against [`gosh-sh/avm-sdk`](https://github.com/gosh-sh/avm-sdk) as a
candidate for the SDK's contract test set.

```text
external settle(route_key, beneficiary) + value_main
  -> routing-table lookup, beneficiary confirmation, minimum-value check
  -> one outbound internal message to the routed beneficiary, with a FUEL reserve
  -> callback request on the same outbound intent
  -> settlement.router.completed records delivery or bounce
```

The router is a pass-through: a successful settlement retains nothing. The whole
transferred `value_main` leaves in the outbound intent.

## What it exercises

The contract is deliberately built to cover the SDK's canonical surface rather
than to be a minimal fixture:

| SDK feature | Where |
| --- | --- |
| `MessageIsolated` guest exports | `export_message_contract!` in `src/lib.rs` |
| `FullReplace` state over a `fixed_bytes` ABI root | `src/state.rs`, 448-byte image |
| Outbound internal messages with `value_main` | `settle`, `withdraw` in `src/router.rs` |
| Extra currency (SHELL, `currency_id = 2`) handling | `withdraw` |
| `fuel_fee_shells` reserves across a DApp split | `CROSS_DAPP_FUEL_RESERVE` |
| Callback requests and callback dispatch | `settle` / `completed` |
| External and internal events with canonical fields | all four event classes |
| Owner authorization by pubkey | route administration, withdraw, upgrade |
| Reserved code upgrade with a state migration | `code_upgrade`, `CODE_UPGRADE_METHOD_ID` |
| Fail-closed error handling with codes ≥ 1000 | `wire::status` |

## Layout

```text
Cargo.toml              workspace root + contract crate (cdylib + rlib)
avm-contract.toml       canonical ABI: state root, types, methods, errors
rust-toolchain.toml     pinned to 1.93.0 + wasm32-unknown-unknown
src/lib.rs              Contract impl, mode guard, guest exports
src/state.rs            fixed-size state image and its codec
src/wire.rs             method ids, event classes, status codes, body codecs
src/router.rs           routing decisions and effect construction
tests/settlement_router.rs   native tests over canonical ACMI frames
beneficiary/             settlement receiver fixture for the delivery round trip
scripts/stand-e2e.sh     on-stand end-to-end scenario
scripts/assert_delivery.py   asserts settled=true / status_code=0 / payout
vendor/avm-contract-sdk      the SDK crate, vendored verbatim (MIT)
docs/method-ids.md      READ THIS before deploying
docs/using-the-stand.md deploy / call / upgrade walkthrough
.github/workflows/contract.yml   fmt, clippy, tests, wasm build, reproducibility
```

The SDK is vendored so the contract builds with **zero crates.io dependencies**
and no network access. The SDK's own documentation sanctions this: *"If the copy
is moved outside this repository, update the `avm-contract-sdk` path dependency
to its installed or vendored location."* `vendor/` carries the SDK's MIT
`LICENSE.md`, and CI diffs the vendored source against the published
distribution so it cannot drift.

## Build

Requires the SDK commands, installed from a `gosh-sh/avm-sdk` checkout with an
authenticated `gh`:

```bash
./scripts/install.sh          # in the avm-sdk checkout
export PATH="${HOME}/.local/bin:${PATH}"

avm-contract-build build --manifest-path Cargo.toml
```

Artifacts land in `target/avm`: `contract.wasm`, `contract.acab`,
`contract.descriptor`. The command builds twice and rejects non-reproducible
output.

Direct source checks, matching the gates the SDK applies to its own examples:

```bash
cargo +1.93.0 fmt --all -- --check
cargo +1.93.0 clippy --all-targets -- -D warnings
cargo +1.93.0 clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo +1.93.0 test
cargo +1.93.0 build --release --target wasm32-unknown-unknown
```

## Test

`cargo test` runs three layers, none of which need the stand binaries:

1. **Codec unit tests** (`src/state.rs`, `src/wire.rs`) — state round-trips,
   malformed-image rejection, body codecs, identifier uniqueness.
2. **Vendored SDK tests** (`vendor/avm-contract-sdk/tests/canonical_io.rs`) —
   the canonical `ACMI`/`ACMO`/`ACBI`/`ACBO` byte-level guarantees.
   `scripts/derive_ids.py --check` additionally re-derives every method id and
   event class from its ABI name (`hash_u64_le` = BLAKE3-8LE).
3. **Contract integration tests** (`tests/settlement_router.rs`) — build real
   canonical `ACMI` frames byte for byte, decode them with the shipped SDK
   decoder, run the contract, and encode the result with the shipped encoder.
   Every success is asserted to encode as canonical `ACMO`; every failure is
   asserted to carry no effects at all.

Covered behaviours: deployment init, route install/update/clear, table bounds,
settlement forwarding with full value transfer, unknown route, beneficiary
mismatch, minimum value, self-payment, one-settlement-in-flight, delivered
callbacks, bounced callbacks, callback replay rejection, withdrawal of main
value and SHELL, authorization refusals, version probe, code upgrade with
migration, upgrade replay refusal, corrupt-state fail-closed, descriptor mode
mismatch, and the E2E failure-flow sentinel channels.

## E2E failure-flow conformance

The contract follows the standard-contract marker protocol from
`examples/contract-b` in [gosh-sh/avm-sdk](https://github.com/gosh-sh/avm-sdk),
so an E2E harness can rewrite the marked payloads and drive deterministic
failures:

- `AVM_E2E_FAILURE_FLOW_FILE_*`, `AVM_E2E_FAILURE_FLOW_RUSTFMT_*`,
  `AVM_E2E_FAILURE_FLOW_HOOK_*`, and `AVM_E2E_FAILURE_FLOW_HELPER_*` marker
  pairs in `src/lib.rs` delimit the payloads the harness may rewrite;
- building with `AVM_E2E_FAILURE_FLOW=1` activates the `failure_flow` helper;
  in every other build the hook compiles to a no-op.

Channels are selected by the callback handler slot of an internal message (the
SDK's `contract-a` forwarder produces exactly such messages):

| method | callback handler | `AVM_E2E_FAILURE_FLOW=1` behaviour |
| --- | --- | --- |
| `settlement.router.settle` | `avm.code_upgrade` | `panic!` — deterministic receiver trap |
| `settlement.router.settle` | `settlement.owner.payout` | never returns — deterministic timeout |
| any other method | `avm.code_upgrade` | malformed-output fixture: empty event name, topics, and fields under the `settlement.router.routed` class, first state byte flipped |
| anything without a sentinel handler | — | identical to a normal build |

CI runs the whole suite in both builds and asserts the sentinel channels side
by side; the hang channel is never exercised in tests because it never returns.

## Verification status

- **Proven by CI** (GitHub Actions, pinned Rust 1.93.0, run `contract` on every
  push): `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` on
  host and `wasm32-unknown-unknown`, 52 tests passing — 12 vendored SDK
  canonical-ABI tests, 29 router integration tests over canonical `ACMI`
  frames (including a full router → beneficiary → router delivery round trip
  over the router's real `settle` intent), 11 codec unit tests — plus a
  `wasm32-unknown-unknown` release build
  (50 KiB), a guest-export check, and a build-reproducibility check. The same
  integration suite also passes with `AVM_E2E_FAILURE_FLOW=1` (the failure-flow
  fixture build), which is compiled for `wasm32-unknown-unknown` as well. The
  run's summary is posted to the pull request with the exact counts and the
  wasm SHA-256.
- **Proven by design review against the SDK source**: canonical frame layouts
  and encoder validation rules. Real defects were found and fixed this way; see
  [Design notes](#design-notes).
- **Proven by derivation**: every `method_id` and `event_class` is
  `hash_u64_le(name)` — the first eight bytes of BLAKE3(name) little-endian —
  the same rule the platform uses (`CODE_UPGRADE_METHOD_ID` is
  `hash_u64_le("avm.code_upgrade")`). The rule reproduces the SDK's own fixture
  constants exactly, and `scripts/derive_ids.py --check` re-derives every
  constant from its ABI name on each CI run. See
  [`docs/method-ids.md`](docs/method-ids.md).
- **On-stand end-to-end** (`deploy` / `fund-user` / `route.set` / `settle` /
  callback / `inspect` / `replay`): executed by the `stand-e2e` CI job against
  the released `avm-local-stand` and `avm-contract-build` binaries on every
  push, with the full command log posted to the pull request as
  `stand-e2e-report.md`. The scenario asserts the delivery *outcome*, not just
  `status: ok` per command: `scripts/assert_delivery.py` checks
  `settlement.router.settled` with `settled: true` and `status_code: 0`,
  `completed_count` growth with `failed_count = 0`, and the beneficiary's
  `settlement.beneficiary.payout` event with the settled value. The authoring
  sandbox cannot download those binaries, so the job — not the author — is what
  closes this gap.

## Design notes

Two decisions came from constraints that are easy to get wrong:

**Callback bodies are constrained by the canonical decoder.** A callback with a
non-zero status must carry an *empty* body — the decoder rejects any other
shape — while a successful delivery may report the receiver's result body and
return the unused fuel reserve as transferred value. A route key therefore
cannot be reported back in the body of a bounced delivery, and it is taken from
retained state either way: `last_route_key` names the one settlement the
callback may complete.

**One settlement in flight.** `last_status` uses `AWAITING_CALLBACK = u32::MAX`
as a sentinel rather than `0`. AVM status codes are `0` or `≥ 1000`, so the
sentinel cannot collide. Without it, a delivered callback would leave
`last_status = 0` and be indistinguishable from "never called back", letting a
second settlement silently overwrite the pending route key and a replayed
callback double-count.

## Before using this in an application

This is a deterministic fixture, not an audited application contract. Per the
SDK's own guidance on its examples:

- The `OWNER_PUBKEY` fallback applies only to fixtures whose deploy has
  neither a 32-byte pin body nor an external deployer. On the stand the deploy
  body is the `ACDP` package envelope; `init` parses it and a 32-byte
  `--init-json` payload (`--init-json '"0x<64-hex>"'`) pins the owner key, an
  empty payload falls back to the deploy signer's identity key (the `developer`
  identity, whose call account the stand does not provide), and any other body
  length falls back to the trailing 32 bytes — `scripts/stand-e2e.sh` verifies
  owner authorization on the stand. For an application, pin your own owner
  explicitly or replace the fallback.
- Replace every fixed route key and amount; to change a method or event,
  rename it and let `hash_u64_le` pick the new id — never hand-pick one.
- Read [`docs/method-ids.md`](docs/method-ids.md) — ids are name-derived
  (`hash_u64_le`), so renaming a method means recomputing its constant; the
  consistency check enforces this.
- `withdraw` addresses the owner inside the router's own `dapp_id`, so it
  assumes the package-owner account is the package-derived account for that
  partition. Confirm that against your deployment topology.
- `beneficiary/` is a second, independent fixture contract that plays the
  routed beneficiary on the local stand (the stand allows one deploy per
  package, and the router refuses a route to its own account). It is not part
  of the router's deliverable.

## License

MIT, matching the SDK distribution. The vendored SDK crate retains its own MIT
license at `vendor/avm-contract-sdk/LICENSE.md`.
