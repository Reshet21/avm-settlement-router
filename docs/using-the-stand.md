# Using The Local Stand

This walkthrough deploys the settlement router into `avm-local-stand` and drives
one settlement end to end. It assumes the SDK distribution is installed:

```bash
# from a gosh-sh/avm-sdk checkout
./scripts/install.sh
export PATH="${HOME}/.local/bin:${PATH}"
```

All commands emit JSON on stdout. The stand never reads wall-clock time, so
every timestamp below is an explicit logical value.

## Environment

```bash
export AVM_SDK_HOME="${AVM_SDK_HOME:-$HOME/.local/share/avm-sdk}"
export AVM_STATE="${PWD}/developer.state"
export AVM_PROFILE="${AVM_SDK_HOME}/current/profiles/wasmtime-release-profile.v1"
export DAPP_ID="1111111111111111111111111111111111111111111111111111111111111111"
export ARTIFACTS="${PWD}/target/avm"
```

## Build

```bash
avm-contract-build build --manifest-path Cargo.toml
```

The command compiles twice, rejects non-reproducible output, validates
`avm-contract.toml` against the WASM, and writes `contract.wasm`,
`contract.acab`, and `contract.descriptor` under `target/avm`.

## Initialize And Deploy

The first unfunded deploy is intentionally side-effect free and returns the
derived address as a structured precondition, so the address is known before any
value moves.

```bash
avm-local-stand init \
  --state "$AVM_STATE" \
  --wasmtime-profile "$AVM_PROFILE" \
  --dapp-id "$DAPP_ID" > init.json

set +e
avm-local-stand deploy \
  --state "$AVM_STATE" \
  --wasmtime-profile "$AVM_PROFILE" \
  --wasm "$ARTIFACTS/contract.wasm" \
  --acab "$ARTIFACTS/contract.acab" \
  --descriptor "$ARTIFACTS/contract.descriptor" \
  --salt-hex 736574746c656d656e74 \
  --init-json null \
  --block-time 1800000000 \
  --created-at 1800000000 \
  --expire-at 1800003600 \
  --fuel-fee-shells 1000 > funding-required.json
deploy_status=$?
set -e

test "$deploy_status" -eq 2
test "$(jq -r .error funding-required.json)" = funding_required
export CONTRACT_ADDRESS="$(jq -r .address funding-required.json)"

avm-local-stand fund-user \
  --state "$AVM_STATE" \
  --wasmtime-profile "$AVM_PROFILE" \
  --address "$CONTRACT_ADDRESS" \
  --shells 100000

avm-local-stand deploy \
  --state "$AVM_STATE" \
  --wasmtime-profile "$AVM_PROFILE" \
  --wasm "$ARTIFACTS/contract.wasm" \
  --acab "$ARTIFACTS/contract.acab" \
  --descriptor "$ARTIFACTS/contract.descriptor" \
  --salt-hex 736574746c656d656e74 \
  --init-json null \
  --block-time 1800000000 \
  --created-at 1800000000 \
  --expire-at 1800003600 \
  --fuel-fee-shells 1000 > deploy.json
```

`init` installs the empty routing table: `routed_count = 0`, no routes,
`last_status = AWAITING_CALLBACK`. The owner key comes from the deploy itself:
the stand signs the deploy message with one of its generated identities (the
`developer` key) and `init` installs that pubkey as the owner. An exactly
32-byte deploy body (`--init-json "0x<32-byte hex>"`) pins an explicit key
instead. The stand generates fresh identity keys at every `init` and reports
them in its JSON output under `identities`; `scripts/stand-e2e.sh` probes
which identity answers as owner (a bodyless `withdraw` call answers `1608`
to the owner and `1603` to anyone else) and records it in the report.

The scripted version of this whole walkthrough is `scripts/stand-e2e.sh`; the
`stand-e2e` CI job runs it against the real stand binaries on every push and
posts `stand-e2e-report.md` to the pull request.

## Install A Route

Route administration is owner-only. Call it with the identity the deploy
installed as owner (the `stand-e2e` report names it; for a stand deploy that
is `--identity developer`, the identity that signed the deploy). If the call
answers `1603`, the owner key and the identity disagree — the `stand-e2e`
report shows exactly which key the stand generated and which one the deploy
installed. The body is
`route_key[8] | dapp_id[32] | account_id[32] | min_value[16]`, 88 bytes:

```bash
ROUTE_KEY_HEX="$(printf 'route-01' | xxd -p)"          # 8 bytes
BENE_DAPP_HEX="$DAPP_ID"                              # 32 bytes
BENE_ACCT_HEX="5555555555555555555555555555555555555555555555555555555555555555"
MIN_VALUE_HEX="$(printf '%032x' 100 | sed 's/\(..\)/\1 /g' | tr ' ' '\n' | tac | tr -d '\n')"

BODY_HEX="${ROUTE_KEY_HEX}${BENE_DAPP_HEX}${BENE_ACCT_HEX}${MIN_VALUE_HEX}"
test "${#BODY_HEX}" -eq 176    # 88 bytes

avm-local-stand call \
  --state "$AVM_STATE" \
  --wasmtime-profile "$AVM_PROFILE" \
  --address "$CONTRACT_ADDRESS" \
  --method settlement.route.set \
  --args-json "\"$BODY_HEX\"" \
  --identity package-owner \
  --value-main 0 --extra-currencies '' \
  --block-time 1800000020 --created-at 1800000019 \
  --expire-at 1800003619 --fuel-fee-shells 1000
```

`min_value` is little-endian, which is why the byte order is reversed above.

## Route A Settlement

```bash
SETTLE_HEX="${ROUTE_KEY_HEX}${BENE_DAPP_HEX}${BENE_ACCT_HEX}"
test "${#SETTLE_HEX}" -eq 144    # 72 bytes

avm-local-stand call \
  --state "$AVM_STATE" \
  --wasmtime-profile "$AVM_PROFILE" \
  --address "$CONTRACT_ADDRESS" \
  --method settlement.router.settle \
  --args-json "\"$SETTLE_HEX\"" \
  --identity user \
  --value-main 500 --extra-currencies '' \
  --block-time 1800000030 --created-at 1800000029 \
  --expire-at 1800003629 --fuel-fee-shells 2000
```

Expect status 0, one outbound internal message to the routed beneficiary
carrying the full `value_main = 500`, a callback request, and a
`settlement.router.routed` external event. `routed_count` becomes 1 and
`last_status` becomes `AWAITING_CALLBACK` (`u32::MAX`).

The `--fuel-fee-shells 2000` reserve on the *call* funds the router's own
execution. The router separately proposes `CROSS_DAPP_FUEL_RESERVE` on the
outbound intent, because the callback leg is always chargeable. See the SDK's
`docs/fuel-and-fees.md`: a same-DApp local-free route commits
`fuel_fee_shells = 0` and never debits that reserve.

## Inspect The Callback

The beneficiary's execution determines whether the callback reports success or a
bounce. With no receiver contract deployed at the beneficiary address, the
delivery fails and the router records it as a bounce.

```bash
avm-local-stand inspect --state "$AVM_STATE" --wasmtime-profile "$AVM_PROFILE"
avm-local-stand events  --state "$AVM_STATE" --wasmtime-profile "$AVM_PROFILE"
```

Look for `settlement.router.settled` with `settled: false` and
`status_code` equal to the delivery failure, plus `failed_count = 1`. Deploy a
receiver at the beneficiary address to exercise the success path, which reports
`settled: true`, `status_code: 0`, and `completed_count = 1`.

## Withdraw And Replay

```bash
avm-local-stand call \
  --state "$AVM_STATE" \
  --wasmtime-profile "$AVM_PROFILE" \
  --address "$CONTRACT_ADDRESS" \
  --method settlement.router.withdraw \
  --args-json null \
  --identity package-owner \
  --value-main 0 --extra-currencies '' \
  --block-time 1800000040 --created-at 1800000039 \
  --expire-at 1800003639 --fuel-fee-shells 1000

avm-local-stand replay --state "$AVM_STATE" --wasmtime-profile "$AVM_PROFILE"
```

`replay` re-executes the accepted input journal and compares the returned roots
and cumulative fees. Because the router reads no `block_rand` and no wall-clock
time, a replay must match exactly. A mismatch means nondeterminism was
introduced and the contract is not fit for the SDK test set.

## Upgrade

```bash
avm-local-stand publish-version \
  --state "$AVM_STATE" --wasmtime-profile "$AVM_PROFILE" \
  --wasm "$ARTIFACTS/contract.wasm" \
  --acab "$ARTIFACTS/contract.acab" \
  --descriptor "$ARTIFACTS/contract.descriptor" \
  --block-time 1800000050 --created-at 1800000049 \
  --expire-at 1800003649 --fuel-fee-shells 1000 > published.json

SELECTED_CODE_ID="$(jq -r .selected_code_id published.json)"
MIGRATION_HEX="$(printf 'settlement-router-v2-mig' | xxd -p)"   # 24 bytes

avm-local-stand upgrade \
  --state "$AVM_STATE" --wasmtime-profile "$AVM_PROFILE" \
  --address "$CONTRACT_ADDRESS" \
  --selected-code-id "$SELECTED_CODE_ID" \
  --migration-payload-hex "$MIGRATION_HEX" \
  --identity package-owner \
  --block-time 1800000060 --created-at 1800000059 \
  --expire-at 1800003659 --fuel-fee-shells 1000
```

The router accepts one upgrade: after the state image carries version 2, further
upgrades are refused with status `1607`. The migration marker `SRV2` is stamped
at state offset 72.

## Deploy envelope and stand limits (observed)

The stand always deploys with an `ACDP` package envelope as the body: a header
whose tail is `[wasm_len u32][descriptor_len u32][acab_len u64][salt 8]`, then
the `--init-json` payload, then the wasm / descriptor / acab blobs. `init`
parses that layout: a 32-byte payload pins the owner key, an empty payload
falls back to the deploy signer's identity key, anything else falls back to the
tail-32 rule.

Three stand behaviours matter when scripting scenarios:

- **one deploy per package** — a second `deploy` of the same wasm is rejected
  with `selection is already attached immutably to the package`, so a scenario
  that needs two accounts (router + beneficiary) must build two packages. The
  `beneficiary/` fixture contract is exactly that second package: it accepts
  `settlement.beneficiary.receive`, emits `settlement.beneficiary.payout`, and
  answers the delivery callback;
- **bodyless calls need a local call account** — a bodyless `--args-json null`
  call from the owner identity fails with `local call account is absent`
  (whether the account exists is decided per generated key and cannot be
  relied on), while body-bearing calls (`--args-json '"0x..."'`) always run.
  The scenario therefore discovers the owner with a real `route.set`, not with
  a bodyless probe;
- **`--owner-id` only influences address derivation** — the deploy signer is
  always the `developer` identity. The owner key is pinned explicitly with
  `--init-json '"0x<64-hex>"'`.
