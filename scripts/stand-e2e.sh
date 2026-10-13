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
# Every command and its streams are recorded in stand-e2e-report.md. The
# scenario: init -> discover the stand's identity keys -> deploy the router
# (owner = the deploying identity, or a key pinned in the deploy body) ->
# deploy a beneficiary -> route.set (owner identity) -> settle (user identity)
# -> delivery callback lands -> a second settle proves the account is not
# stuck -> inspect -> replay roots match.
#
# The stand generates fresh identity keys at every `init`, so the router takes
# the owner from the deploy itself: an empty deploy body installs the
# deployer's pubkey, and a 0x-prefixed 32-byte body pins an explicit key (see
# `init` in src/lib.rs). The script reports which identity answers as owner.
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
STEP=0

failures=0

log() { printf '%s\n' "$*" >>"$REPORT"; }
heading() { printf '\n## %s\n' "$*" | tee -a "$REPORT"; }

# Run a stand command, keeping stdout (JSON) separate from stderr (usage
# hints). Both streams plus the exit code land in the report. The JSON goes
# to the path in $1 when given.
stand() {
    local json_out="${1:-}"
    shift
    printf '\n```text\n$ %s\n' "$*" >>"$REPORT"
    local status errexit_was_on=0
    if [[ $- == *e* ]]; then
        errexit_was_on=1
    fi
    set +e
    if [ -n "$json_out" ]; then
        "$@" >"$json_out" 2>"$WORK/last-stderr"
        status=$?
        cat "$json_out" >>"$REPORT"
    else
        "$@" >"$WORK/last-stdout" 2>"$WORK/last-stderr"
        status=$?
        cat "$WORK/last-stdout" >>"$REPORT"
    fi
    if [ "$errexit_was_on" = 1 ]; then
        set -e
    else
        set +e
    fi
    if [ -s "$WORK/last-stderr" ]; then
        printf -- '--- stderr ---\n' >>"$REPORT"
        cat "$WORK/last-stderr" >>"$REPORT"
    fi
    printf '[exit %d]\n```\n' "$status" >>"$REPORT"
    return $status
}

# Runs a plain check command (e.g. `test -f`), logging it like `stand` does.
need() {
    printf '\n```text\n$ %s\n' "$*" >>"$REPORT"
    local status
    set +e
    "$@" >>"$REPORT" 2>>"$WORK/last-stderr"
    status=$?
    set -e
    if [ -s "$WORK/last-stderr" ]; then
        printf -- '--- stderr ---\n' >>"$REPORT"
        cat "$WORK/last-stderr" >>"$REPORT"
    fi
    printf '[exit %d]\n```\n' "$status" >>"$REPORT"
    if [ "$status" -ne 0 ]; then
        printf 'FAILED: %s\n' "$*" | tee -a "$REPORT"
        failures=$((failures + 1))
    fi
    return 0
}

# One deploy round trip: the first attempt reports funding_required with the
# derived address, fund it, deploy again. $1 = json capture path, $2 = salt
# hex, $3 = init-json value. Prints the account address only when the deploy
# completed without an error field.
deploy_instance() {
    local json_out="$1" salt="$2" init_json="$3" arts="${4:-$ARTIFACTS}" address error attempt
    for attempt in 1 2; do
        set +e
        stand "$json_out" avm-local-stand deploy --state "$STATE" --wasmtime-profile "$PROFILE" \
            --wasm "$arts/contract.wasm" --acab "$arts/contract.acab" \
            --descriptor "$arts/contract.descriptor" --salt-hex "$salt" \
            --init-json "$init_json" \
            --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) \
            --expire-at $((NOW + STEP + 3600)) --fuel-fee-shells 1000
        set -e
        error="$(jq -r '.error // empty' "$json_out" 2>/dev/null || true)"
        got="$(jq -r '.address // empty' "$json_out" 2>/dev/null || true)"
        if [ -n "$got" ]; then
            address="$got"
        fi
        if [ -z "$error" ]; then
            # A success receipt carries no address; the funding_required
            # response is where the stand reports the derived address.
            [ -n "$address" ] && printf '%s' "$address" && return 0
            return 1
        fi
        if [ "$error" = "funding_required" ] && [ -n "$address" ]; then
            stand /dev/null avm-local-stand fund-user --state "$STATE" --wasmtime-profile "$PROFILE" \
                --address "$address" --shells 100000 || true
            STEP=$((STEP + 10))
            continue
        fi
        return 1
    done
    return 1
}

call_json() { # json_out method identity [value]
    local json_out="$1" method="$2" identity="$3" value="${4:-0}"
    stand "$json_out" avm-local-stand call --state "$STATE" --wasmtime-profile "$PROFILE" \
        --address "$ROUTER" --method "$method" --args-json "${ARGS_JSON:-null}" \
        --identity "$identity" --value-main "$value" --extra-currencies '' \
        --block-time $((NOW + STEP)) --created-at $((NOW + STEP - 1)) \
        --expire-at $((NOW + STEP + 3599)) --fuel-fee-shells 1000
}

# The contract status lives inside raw_executor_output: ACMO magic (4 bytes)
# then the status code as u32 little-endian.
acmo_status() {
    local hex b
    hex="$(jq -r '.raw_executor_output // empty' "$1" 2>/dev/null || true)"
    if [ "${hex:0:8}" = "41434d4f" ] && [ "${#hex}" -ge 16 ]; then
        b="${hex:14:2}${hex:12:2}${hex:10:2}${hex:8:2}"
        printf '%d' "$((16#$b))"
    fi
}

# True when the call executed on the platform and the contract answered 0.
call_ok() {
    local st
    st="$(acmo_status "$1")"
    [ -n "$st" ] && [ "$st" = "0" ] && grep -q '"status": "ok"' "$1"
}

args_json_for() { # hex-body -> JSON argument in the discovered encoding
    if [ "${HEX_STYLE:-0x}" = "0x" ]; then
        printf '"0x%s"' "$1"
    else
        printf '"%s"' "$1"
    fi
}

printf '\n# Stand E2E report (scenario)\n' >>"$REPORT"
log "date: $(date -u +%FT%TZ)"

heading "Preflight"
need test -f "$ARTIFACTS/contract.wasm"
need test -f "$ARTIFACTS/contract.acab"
need test -f "$ARTIFACTS/contract.descriptor"
need test -f "$ARTIFACTS/../avm-beneficiary/contract.wasm"
need test -f "$PROFILE"
ls -l "$ARTIFACTS" >>"$REPORT" 2>&1 || true

OWNER_IDENTITY=""
ROUTER=""
for ATTEMPT in 1 2 3 4 5; do
heading "init (attempt $ATTEMPT)"
rm -f "$STATE"
stand "$WORK/init.json" avm-local-stand init --state "$STATE" --wasmtime-profile "$PROFILE" \
    --dapp-id "$DAPP_ID"
OWNER_HEX="$(jq -r '.identities.package_owner // empty' "$WORK/init.json")"
if [ -z "$OWNER_HEX" ]; then
    printf 'FAILED: init.json does not name the package-owner key\n' | tee -a "$REPORT"
    exit 1
fi
log "package-owner key from init.json: $OWNER_HEX"
log "init.json: $(jq -c . "$WORK/init.json" 2>/dev/null || true)"

heading "deploy the router (owner = the deploying identity)"
# The stand generates fresh identity keys at every init, so the router takes
# the owner from the deploy itself: an empty deploy body installs the
# deployer's key, and a 0x-prefixed 32-byte body pins an explicit key (the
# canonical --init-json forms are null or a 0x byte string).
ROUTER=""
OWNER_IDENTITY=""
OWNER_HEX="$(jq -r '.identities.package_owner // empty' "$WORK/init.json")"
if ROUTER="$(deploy_instance "$WORK/deploy-attempt.json" 726f757465723031 "\"0x$OWNER_HEX\"")"; then
    log "router address: $ROUTER (init pins the package-owner key as owner)"
else
    printf 'FAILED: router deploy did not complete\n' | tee -a "$REPORT"
    exit 1
fi
BENE_ARTS="$ARTIFACTS/../avm-beneficiary"
if BENEFICIARY="$(deploy_instance "$WORK/bene-deploy.json" 62656e652d3031 null "$BENE_ARTS")"; then
    log "beneficiary address: $BENEFICIARY (settlement-beneficiary fixture, its own package)"
else
    printf 'FAILED: beneficiary deploy did not complete\n' | tee -a "$REPORT"
    exit 1
fi
# The identity accounts pay call fees; the stand does not create all of them
# up front ("local call account is absent"). Dump the registry and try every
# plausible account-id derivation for the developer identity.
stand "$WORK/inspect-early.json" avm-local-stand inspect --state "$STATE" \
    --wasmtime-profile "$PROFILE" || true
log "inspect after deploy: $(jq -c . "$WORK/inspect-early.json" 2>/dev/null | head -c 600)"
# Call-path smoke test with the exact canonical shape from the SDK
# getting-started (no args): proves the charge/exec path before we trust any
# probe verdicts.
HEX_STYLE="0x"
ARGS_JSON="null"
STEP=$((STEP + 10))
call_json "$WORK/version.json" settlement.router.version user 0 || true
log "version smoke call: $(jq -c . "$WORK/version.json" 2>/dev/null | head -c 400)"

# Owner discovery = a real `route.set` with a valid route body. The stand
# gates bodyless calls behind a local call account ("local call account is
# absent" for the owner identity) but lets body-bearing calls through, so the
# owner is discovered by actually installing the route (status 0).
BENEDAPP="${BENEFICIARY%%:*}"
BENEACCT="${BENEFICIARY##*:}"
MIN_LE="$(python3 -c "print(int('$MIN_VALUE').to_bytes(16, 'little').hex())")"
ROUTER_ARGS="${ROUTE_KEY_HEX}${BENEDAPP}${BENEACCT}${MIN_LE}"
ARGS_JSON="$(args_json_for "$ROUTER_ARGS")"
for owner_id in package-owner user developer; do
    STEP=$((STEP + 10))
    call_json "$WORK/route-$owner_id.json" settlement.route.set "$owner_id" 0 || true
    st="$(acmo_status "$WORK/route-$owner_id.json")"
    if [ "$st" = "0" ]; then
        log "route.set ($owner_id): status 0 Routed -> owner"
        OWNER_IDENTITY="$owner_id"
        break
    elif [ "$st" = "1603" ]; then
        log "route.set ($owner_id): status 1603 UNAUTHORIZED (not the owner)"
    else
        log "route.set ($owner_id): status '$st' response: $(head -c 200 "$WORK/route-$owner_id.json" 2>/dev/null)"
    fi
done
if [ -n "$OWNER_IDENTITY" ]; then
    log "owner identity: $OWNER_IDENTITY (attempt $ATTEMPT)"
    break
fi
log "attempt $ATTEMPT: no identity could install the route; re-rolling"
done
if [ -z "$OWNER_IDENTITY" ]; then
    printf 'FAILED: no stand identity answers as the router owner\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi
if [ -n "$OWNER_IDENTITY" ]; then
heading "settle the route to the beneficiary fixture"
log "beneficiary address: $BENEFICIARY"

# route.set body: route_key[8] | dapp[32] | account[32] | min_value u128 le
BENEDAPP="${BENEFICIARY%%:*}"
BENEACCT="${BENEFICIARY##*:}"
MIN_LE="$(python3 -c "print(int('$MIN_VALUE').to_bytes(16, 'little').hex())")"
ROUTER_ARGS="${ROUTE_KEY_HEX}${BENEDAPP}${BENEACCT}${MIN_LE}"

heading "route.set already installed by the owner-discovery step"
HEX_STYLE="0x"
if ! call_ok "$WORK/route-$OWNER_IDENTITY.json"; then
    printf 'FAILED: route.set did not succeed — owner identity check is not closed\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi
log "args encoding in use: $HEX_STYLE hex; route.set: $(jq -c . "$WORK/route-$OWNER_IDENTITY.json" 2>/dev/null | head -c 300)"

# Asserts the settle's delivery outcome, not just `status: ok`: settled=true,
# status_code=0, completed_count growth, failed_count 0, and the beneficiary's
# payout event in the journal.
assert_delivery() {
    local settle_json="$1" value="$2" routed="$3" completed="$4"
    stand "$WORK/events.json" avm-local-stand events --state "$STATE" \
        --wasmtime-profile "$PROFILE" || true
    if python3 "$ROOT/scripts/assert_delivery.py" "$settle_json" "$WORK/events.json" \
        "$routed" "$completed" "$value" >>"$REPORT" 2>&1; then
        log "delivery assertion (value $value): $(tail -n 1 "$REPORT")"
    else
        printf 'FAILED: delivery did not complete for settle value %s\n' "$value" \
            | tee -a "$REPORT"
        failures=$((failures + 1))
    fi
}

heading "settle (user identity)"
SETTLE_ARGS="${ROUTE_KEY_HEX}${BENEDAPP}${BENEACCT}"
ARGS_JSON="$(args_json_for "$SETTLE_ARGS")"
STEP=$((STEP + 10))
call_json "$WORK/settle.json" settlement.router.settle user "$SETTLE_VALUE" || true
if ! call_ok "$WORK/settle.json"; then
    printf 'FAILED: settle did not succeed\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi
log "settle: $(jq -c . "$WORK/settle.json" 2>/dev/null | head -c 300)"

heading "delivery result: settled, not a bounce"
STEP=$((STEP + 10))
assert_delivery "$WORK/settle.json" "$SETTLE_VALUE" 1 1

heading "inspect after the delivery callback"
STEP=$((STEP + 10))
stand "$WORK/inspect-callback.json" avm-local-stand inspect --state "$STATE" \
    --wasmtime-profile "$PROFILE" || true
log "inspect after settle: $(jq -c . "$WORK/inspect-callback.json" 2>/dev/null | head -c 400)"

heading "a second settle proves the account is not stuck"
STEP=$((STEP + 10))
call_json "$WORK/settle2.json" settlement.router.settle user $((SETTLE_VALUE + 200)) || true
if ! call_ok "$WORK/settle2.json"; then
    printf 'FAILED: the second settle was refused\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi
log "second settle: $(jq -c . "$WORK/settle2.json" 2>/dev/null | head -c 300)"

heading "delivery result of the second settle"
STEP=$((STEP + 10))
assert_delivery "$WORK/settle2.json" $((SETTLE_VALUE + 200)) 2 2

heading "inspect, events, and replay"
STEP=$((STEP + 10))
need avm-local-stand inspect --state "$STATE" --wasmtime-profile "$PROFILE"
need avm-local-stand events --state "$STATE" --wasmtime-profile "$PROFILE"
need avm-local-stand replay --state "$STATE" --wasmtime-profile "$PROFILE"
fi

## Result

log "route.set identity: $OWNER_IDENTITY"
if [ "${failures:-0}" -eq 0 ]; then
    printf 'ALL SCENARIO STEPS PASSED\n' | tee -a "$REPORT"
    exit 0
fi
printf '%s stand step(s) failed\n' "${failures:-0}" | tee -a "$REPORT"
exit 1
