#!/usr/bin/env bash
# Stand end-to-end scenario for the settlement router.
#
# Requires `avm-local-stand` and `avm-contract-build` (see
# docs/using-the-stand.md), jq, and the built contract artifacts under
# target/avm. Run:
#
#     avm-contract-build build --manifest-path Cargo.toml
#     bash scripts/stand-e2e.sh
#
# Every command and its output is recorded in stand-e2e-report.md. The
# scenario: init -> deploy router -> deploy beneficiary -> route.set (owner
# identity) -> settle (user identity) -> delivery callback lands -> second
# settle proves the account is not stuck -> inspect -> replay roots match.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPORT="${STAND_REPORT:-$ROOT/stand-e2e-report.md}"
WORK="$(mktemp -d)"
STATE="$WORK/developer.state"
PROFILE="${AVM_PROFILE:-${AVM_SDK_HOME:-$HOME/.local/share/avm-sdk}/current/profiles/wasmtime-release-profile.v1}"
ARTIFACTS="${ARTIFACTS:-$ROOT/target/avm}"
DAPP_ID="1111111111111111111111111111111111111111111111111111111111111111"
ROUTE_KEY_HEX="726f7574652d3031" # "route-01"
MIN_VALUE=100
SETTLE_VALUE=500
NOW=1800000000

failures=0

log() { printf '%s\n' "$*" >>"$REPORT"; }
heading() { printf '\n## %s\n' "$*" | tee -a "$REPORT"; }

run() {
    printf '\n```text\n$ %s\n' "$*" >>"$REPORT"
    local out status
    set +e
    out="$("$@" 2>&1)"
    status=$?
    set -e
    printf '%s\n[exit %d]\n```\n' "$out" "$status" >>"$REPORT"
    printf '%s' "$out"
    return $status
}

need() {
    if ! run "$@"; then
        printf 'FAILED: %s\n' "$*" | tee -a "$REPORT"
        failures=$((failures + 1))
    fi
    return 0
}

call_raw() { # method identity   (args encoding comes from ARGS_JSON)
    local method="$1" identity="$2"
    avm-local-stand call --state "$STATE" --wasmtime-profile "$PROFILE" \
        --address "$ROUTER" --method "$method" --args-json "${ARGS_JSON:-null}" \
        --identity "$identity" --value-main "${VALUE:-0}" --extra-currencies '' \
        --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) --expire-at $((NOW + STEP + 3600)) \
        --fuel-fee-shells 1000
}

args_json_for() { # hex-body -> JSON argument in the discovered encoding
    if [ "${HEX_STYLE:-0x}" = "0x" ]; then
        printf '"0x%s"' "$1"
    else
        printf '"%s"' "$1"
    fi
}

printf '# Stand E2E report\n' >"$REPORT"
log "date: $(date -u +%FT%TZ)"

heading "Preflight"
need test -f "$ARTIFACTS/contract.wasm"
need test -f "$ARTIFACTS/contract.acab"
need test -f "$ARTIFACTS/contract.descriptor"
need test -f "$PROFILE"
ls -l "$ARTIFACTS" >>"$REPORT" 2>&1 || true

heading "init"
run avm-local-stand init --state "$STATE" --wasmtime-profile "$PROFILE" --dapp-id "$DAPP_ID" >"$WORK/init.json"
cat "$WORK/init.json" >>"$REPORT"

heading "deploy the router (funding round trip)"
STEP=10
set +e
deploy_out=$(avm-local-stand deploy --state "$STATE" --wasmtime-profile "$PROFILE" \
    --wasm "$ARTIFACTS/contract.wasm" --acab "$ARTIFACTS/contract.acab" \
    --descriptor "$ARTIFACTS/contract.descriptor" --salt-hex 726f757465723031 \
    --init-json null --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) \
    --expire-at $((NOW + STEP + 3600)) --fuel-fee-shells 1000 2>&1)
deploy_status=$?
set -e
log '```text'
log "$deploy_out"
log "[exit $deploy_status]"
log '```'
ROUTER="$(printf '%s' "$deploy_out" | jq -r '.address // empty')"
if [ -z "$ROUTER" ]; then
    printf 'FAILED: deploy did not report the contract address\n' | tee -a "$REPORT"
    exit 1
fi
log "router address: $ROUTER"
need avm-local-stand fund-user --state "$STATE" --wasmtime-profile "$PROFILE" \
    --address "$ROUTER" --shells 100000
STEP=20
need avm-local-stand deploy --state "$STATE" --wasmtime-profile "$PROFILE" \
    --wasm "$ARTIFACTS/contract.wasm" --acab "$ARTIFACTS/contract.acab" \
    --descriptor "$ARTIFACTS/contract.descriptor" --salt-hex 726f757465723031 \
    --init-json null --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) \
    --expire-at $((NOW + STEP + 3600)) --fuel-fee-shells 1000 >>"$WORK/deploy.json"
cat "$WORK/deploy.json" >>"$REPORT"

heading "deploy the beneficiary (a second router instance)"
STEP=30
set +e
bene_out=$(avm-local-stand deploy --state "$STATE" --wasmtime-profile "$PROFILE" \
    --wasm "$ARTIFACTS/contract.wasm" --acab "$ARTIFACTS/contract.acab" \
    --descriptor "$ARTIFACTS/contract.descriptor" --salt-hex 62656e652d3031 \
    --init-json null --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) \
    --expire-at $((NOW + STEP + 3600)) --fuel-fee-shells 1000 2>&1)
bene_status=$?
set -e
log '```text'
log "$bene_out"
log "[exit $bene_status]"
log '```'
BENEFICIARY="$(printf '%s' "$bene_out" | jq -r '.address // empty')"
if [ -z "$BENEFICIARY" ]; then
    printf 'FAILED: beneficiary deploy did not report an address\n' | tee -a "$REPORT"
    exit 1
fi
log "beneficiary address: $BENEFICIARY"
need avm-local-stand fund-user --state "$STATE" --wasmtime-profile "$PROFILE" \
    --address "$BENEFICIARY" --shells 100000
STEP=40
need avm-local-stand deploy --state "$STATE" --wasmtime-profile "$PROFILE" \
    --wasm "$ARTIFACTS/contract.wasm" --acab "$ARTIFACTS/contract.acab" \
    --descriptor "$ARTIFACTS/contract.descriptor" --salt-hex 62656e652d3031 \
    --init-json null --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) \
    --expire-at $((NOW + STEP + 3600)) --fuel-fee-shells 1000 >>"$WORK/bene-deploy.json"
cat "$WORK/bene-deploy.json" >>"$REPORT"

# route.set body: route_key[8] | dapp[32] | account[32] | min_value u128 le
BENEDAPP="${BENEFICIARY%%:*}"
BENEACCT="${BENEFICIARY##*:}"
MIN_LE="$(python3 - "$MIN_VALUE" <<'PY'
import sys
print(int(sys.argv[1]).to_bytes(16, "little").hex())
PY
)"
ROUTER_ARGS="${ROUTE_KEY_HEX}${BENEDAPP}${BENEACCT}${MIN_LE}"

heading "route.set (owner identity)"
HEX_STYLE="0x"
ARGS_JSON="$(args_json_for "$ROUTER_ARGS")"
STEP=50
if call_raw settlement.router.route.set package-owner >"$WORK/route.json" 2>&1; then
    cat "$WORK/route.json" >>"$REPORT"
    if jq -e '.error' "$WORK/route.json" >/dev/null 2>&1; then
        log 'the 0x-hex encoding was refused; trying bare hex'
        HEX_STYLE="bare"
        ARGS_JSON="$(args_json_for "$ROUTER_ARGS")"
        need call_raw settlement.router.route.set package-owner >"$WORK/route.json"
        cat "$WORK/route.json" >>"$REPORT"
    fi
else
    cat "$WORK/route.json" >>"$REPORT" 2>/dev/null || true
    HEX_STYLE="bare"
    ARGS_JSON="$(args_json_for "$ROUTER_ARGS")"
    need call_raw settlement.router.route.set package-owner >"$WORK/route.json"
    cat "$WORK/route.json" >>"$REPORT"
fi
if jq -e '.status_code? // 0 | select(. != 0)' "$WORK/route.json" >/dev/null 2>&1 \
    || jq -e '.error' "$WORK/route.json" >/dev/null 2>&1; then
    printf 'FAILED: route.set did not succeed — owner identity check is not closed\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi
log "args encoding in use: $HEX_STYLE hex"

heading "settle (user identity)"
SETTLE_ARGS="${ROUTE_KEY_HEX}${BENEDAPP}${BENEACCT}"
ARGS_JSON="$(args_json_for "$SETTLE_ARGS")"
STEP=60
VALUE=$SETTLE_VALUE
need call_raw settlement.router.settle user >"$WORK/settle.json"
cat "$WORK/settle.json" >>"$REPORT"

heading "delivery callback lands"
STEP=70
for attempt in 1 2 3; do
    run avm-local-stand inspect --state "$STATE" --wasmtime-profile "$PROFILE" >"$WORK/inspect-$attempt.json" || true
    cat "$WORK/inspect-$attempt.json" >>"$REPORT"
    if jq -e '(.status.last_status? // .last_status? // 0) != 4294967295' "$WORK/inspect-$attempt.json" >/dev/null 2>&1; then
        log "callback settled on attempt $attempt"
        break
    fi
    if [ "$attempt" = 3 ]; then
        printf 'FAILED: the in-flight sentinel never cleared — callback did not land\n' | tee -a "$REPORT"
        failures=$((failures + 1))
    fi
done

heading "a second settle proves the account is not stuck"
STEP=90
VALUE=$((SETTLE_VALUE + 200))
need call_raw settlement.router.settle user >"$WORK/settle2.json"
cat "$WORK/settle2.json" >>"$REPORT"
if jq -e '.status_code? // 0 | select(. != 0)' "$WORK/settle2.json" >/dev/null 2>&1 \
    || jq -e '.error' "$WORK/settle2.json" >/dev/null 2>&1; then
    printf 'FAILED: the second settle was refused\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi

heading "inspect and replay"
STEP=110
need avm-local-stand inspect --state "$STATE" --wasmtime-profile "$PROFILE" | tee -a "$REPORT"
need avm-local-stand replay --state "$STATE" --wasmtime-profile "$PROFILE" | tee -a "$REPORT"
need avm-local-stand events --state "$STATE" --wasmtime-profile "$PROFILE" | tee -a "$REPORT"

heading "Result"
if [ "$failures" -eq 0 ]; then
    log "all stand steps passed"
else
    log "$failures stand step(s) failed"
fi
exit "$failures"
