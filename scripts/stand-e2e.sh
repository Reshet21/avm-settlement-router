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
    local status
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
    set -e
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
    local json_out="$1" salt="$2" init_json="$3" address error attempt
    for attempt in 1 2; do
        set +e
        stand "$json_out" avm-local-stand deploy --state "$STATE" --wasmtime-profile "$PROFILE" \
            --wasm "$ARTIFACTS/contract.wasm" --acab "$ARTIFACTS/contract.acab" \
            --descriptor "$ARTIFACTS/contract.descriptor" --salt-hex "$salt" \
            --init-json "$init_json" --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) \
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
        --block-time $((NOW + STEP)) --created-at $((NOW + STEP)) \
        --expire-at $((NOW + STEP + 3600)) --fuel-fee-shells 1000
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
need test -f "$PROFILE"
ls -l "$ARTIFACTS" >>"$REPORT" 2>&1 || true

heading "init"
stand "$WORK/init.json" avm-local-stand init --state "$STATE" --wasmtime-profile "$PROFILE" \
    --dapp-id "$DAPP_ID"
OWNER_HEX="$(jq -r '.identities.package_owner // empty' "$WORK/init.json")"
if [ -z "$OWNER_HEX" ]; then
    printf 'FAILED: init.json does not name the package-owner key\n' | tee -a "$REPORT"
    exit 1
fi
log "package-owner key from init.json: $OWNER_HEX"

heading "deploy the router (owner = the deploying identity)"
# The stand generates fresh identity keys at every init, so the router takes
# the owner from the deploy itself: an empty deploy body installs the
# deployer's key, and a 0x-prefixed 32-byte body pins an explicit key (the
# canonical --init-json forms are null or a 0x byte string).
ROUTER=""
OWNER_IDENTITY=""
INIT_ENCODING="null"
if ROUTER="$(deploy_instance "$WORK/deploy-attempt.json" 726f757465723031 null)"; then
    log "router address: $ROUTER (empty deploy body: owner = deployer identity)"
else
    INIT_ENCODING="\"0x$OWNER_HEX\""
    if ROUTER="$(deploy_instance "$WORK/deploy-attempt.json" 70696e2d3031 "$INIT_ENCODING")"; then
        log "router address: $ROUTER (owner key pinned via --init-json $INIT_ENCODING)"
    else
        printf 'FAILED: router deploy did not complete\n' | tee -a "$REPORT"
        exit 1
    fi
fi
# Owner probe: `withdraw` answers 1702 OWNER_CURRENCY_NOT_FOUND to the owner
# and 1603 UNAUTHORIZED to anyone else, without touching the routing table.
HEX_STYLE="0x"
ARGS_JSON="$(args_json_for "$(printf '%096x' 0)")"
for owner_id in package-owner developer user; do
    call_json "$WORK/owner-probe-$owner_id.json" settlement.router.withdraw "$owner_id" 0 || true
    probe_status="$(jq -r '.status_code // empty' "$WORK/owner-probe-$owner_id.json" 2>/dev/null || true)"
    log "owner probe ($owner_id, withdraw): status ${probe_status:-command-failed}"
    if [ "$probe_status" = "1702" ]; then
        OWNER_IDENTITY="$owner_id"
        break
    fi
done
if [ -z "$OWNER_IDENTITY" ]; then
    printf 'FAILED: no stand identity answers as the router owner\n' | tee -a "$REPORT"
    exit 1
fi
log "owner identity: $OWNER_IDENTITY"

heading "deploy the beneficiary (a second router instance)"
STEP=$((STEP + 10))
if ! BENEFICIARY="$(deploy_instance "$WORK/bene-deploy.json" 62656e652d3031 null)"; then
    printf 'FAILED: beneficiary deploy did not report an address\n' | tee -a "$REPORT"
    exit 1
fi
log "beneficiary address: $BENEFICIARY"

# route.set body: route_key[8] | dapp[32] | account[32] | min_value u128 le
BENEDAPP="${BENEFICIARY%%:*}"
BENEACCT="${BENEFICIARY##*:}"
MIN_LE="$(python3 -c "print(int('$MIN_VALUE').to_bytes(16, 'little').hex())")"
ROUTER_ARGS="${ROUTE_KEY_HEX}${BENEDAPP}${BENEACCT}${MIN_LE}"

heading "route.set (owner identity)"
HEX_STYLE="0x"
ARGS_JSON="$(args_json_for "$ROUTER_ARGS")"
STEP=$((STEP + 10))
if ! call_json "$WORK/route.json" settlement.route.set "$OWNER_IDENTITY" 0; then
    log 'the 0x-hex encoding was refused; trying bare hex'
    HEX_STYLE="bare"
    ARGS_JSON="$(args_json_for "$ROUTER_ARGS")"
    call_json "$WORK/route.json" settlement.route.set "$OWNER_IDENTITY" 0 || true
fi
if jq -e '(.status_code // 0) != 0 or .error' "$WORK/route.json" >/dev/null 2>&1; then
    printf 'FAILED: route.set did not succeed — owner identity check is not closed\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi
log "args encoding in use: $HEX_STYLE hex"

heading "settle (user identity)"
SETTLE_ARGS="${ROUTE_KEY_HEX}${BENEDAPP}${BENEACCT}"
ARGS_JSON="$(args_json_for "$SETTLE_ARGS")"
STEP=$((STEP + 10))
call_json "$WORK/settle.json" settlement.router.settle user "$SETTLE_VALUE" || true
if jq -e '(.status_code // 0) != 0 or .error' "$WORK/settle.json" >/dev/null 2>&1; then
    printf 'FAILED: settle did not succeed\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi

heading "delivery callback lands"
for attempt in 1 2 3; do
    STEP=$((STEP + 10))
    stand "$WORK/inspect-$attempt.json" avm-local-stand inspect --state "$STATE" \
        --wasmtime-profile "$PROFILE" || true
    if jq -e '(.. | objects | .last_status? // empty) != 4294967295' \
        "$WORK/inspect-$attempt.json" >/dev/null 2>&1; then
        log "callback settled on attempt $attempt"
        break
    fi
    if [ "$attempt" = 3 ]; then
        printf 'FAILED: the in-flight sentinel never cleared — callback did not land\n' | tee -a "$REPORT"
        failures=$((failures + 1))
    fi
done

heading "a second settle proves the account is not stuck"
STEP=$((STEP + 10))
call_json "$WORK/settle2.json" settlement.router.settle user $((SETTLE_VALUE + 200)) || true
if jq -e '(.status_code // 0) != 0 or .error' "$WORK/settle2.json" >/dev/null 2>&1; then
    printf 'FAILED: the second settle was refused\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi

heading "inspect, events, and replay"
STEP=$((STEP + 10))
need avm-local-stand inspect --state "$STATE" --wasmtime-profile "$PROFILE"
need avm-local-stand events --state "$STATE" --wasmtime-profile "$PROFILE"
need avm-local-stand replay --state "$STATE" --wasmtime-profile "$PROFILE"

heading "Result"
if [ "$failures" -eq 0 ]; then
    log "all stand steps passed"
else
    log "$failures stand step(s) failed"
fi
exit "$failures"
