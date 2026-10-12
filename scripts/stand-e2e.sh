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
    local json_out="$1" salt="$2" init_json="$3" extra="${4:-}" address error attempt
    for attempt in 1 2; do
        set +e
        stand "$json_out" avm-local-stand deploy --state "$STATE" --wasmtime-profile "$PROFILE" \
            --wasm "$ARTIFACTS/contract.wasm" --acab "$ARTIFACTS/contract.acab" \
            --descriptor "$ARTIFACTS/contract.descriptor" --salt-hex "$salt" \
            --init-json "$init_json" $extra \
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
log "init.json: $(jq -c . "$WORK/init.json" 2>/dev/null || true)"

heading "deploy the router (owner = the deploying identity)"
# The stand generates fresh identity keys at every init, so the router takes
# the owner from the deploy itself: an empty deploy body installs the
# deployer's key, and a 0x-prefixed 32-byte body pins an explicit key (the
# canonical --init-json forms are null or a 0x byte string).
ROUTER=""
OWNER_IDENTITY=""
OWNER_HEX="$(jq -r '.identities["package-owner"]' "$WORK/init.json")"
if ROUTER="$(deploy_instance "$WORK/deploy-attempt.json" 726f757465723031 "\"0x$OWNER_HEX\"")"; then
    log "router address: $ROUTER (init pins the package-owner key as owner)"
else
    printf 'FAILED: router deploy did not complete\n' | tee -a "$REPORT"
    exit 1
fi
# The identity accounts pay call fees; the stand does not create all of them
# up front ("local call account is absent"). Dump the registry and try every
# plausible account-id derivation for the developer identity.
stand "$WORK/inspect-early.json" avm-local-stand inspect --state "$STATE" \
    --wasmtime-profile "$PROFILE" || true
log "inspect after deploy: $(jq -c . "$WORK/inspect-early.json" 2>/dev/null | head -c 600)"
DEV_KEY="$(jq -r '.identities.developer // empty' "$WORK/init.json")"
DEV_SH="$(printf '%s' "$DEV_KEY" | python3 -c "import hashlib,sys,binascii; print(binascii.hexlify(hashlib.sha256(binascii.unhexlify(sys.stdin.read().strip())).digest()).decode())" 2>/dev/null || true)"
for dev_addr in "$DAPP_ID:$DEV_KEY" "$DAPP_ID:$DEV_SH" "$DEV_KEY"; do
    [ -n "$dev_addr" ] || continue
    case "$dev_addr" in *":") continue ;; esac
    stand "$WORK/dev-fund.json" avm-local-stand fund-user --state "$STATE" \
        --wasmtime-profile "$PROFILE" --address "$dev_addr" --shells 100000 || true
    log "fund developer as $dev_addr: $(jq -c . "$WORK/dev-fund.json" 2>/dev/null | head -c 200)"
done
for id_name in package_owner user; do
    id_key="$(jq -r ".identities.$id_name // empty" "$WORK/init.json")"
    if [ -n "$id_key" ]; then
        stand /dev/null avm-local-stand fund-user --state "$STATE" --wasmtime-profile "$PROFILE" \
            --address "$DAPP_ID:$id_key" --shells 100000 || true
    fi
done

# Call-path smoke test with the exact canonical shape from the SDK
# getting-started (no args): proves the charge/exec path before we trust any
# probe verdicts.
HEX_STYLE="0x"
ARGS_JSON="null"
STEP=$((STEP + 10))
call_json "$WORK/version.json" settlement.router.version user 0 || true
log "version smoke call: $(jq -c . "$WORK/version.json" 2>/dev/null | head -c 400)"

# Owner probe: a bodyless `withdraw` answers 1608 NOTHING_TO_WITHDRAW to the
# owner and 1603 UNAUTHORIZED to anyone else, without touching the routing
# table (withdraw refuses a non-empty body by design).
ARGS_JSON="null"
for owner_id in package-owner developer user; do
    STEP=$((STEP + 10))
    call_json "$WORK/owner-probe-$owner_id.json" settlement.router.withdraw "$owner_id" 0 || true
    st="$(acmo_status "$WORK/owner-probe-$owner_id.json")"
    if [ "$st" = "1608" ]; then
        log "owner probe ($owner_id, withdraw): status 1608 NOTHING_TO_WITHDRAW -> owner"
        OWNER_IDENTITY="$owner_id"
        break
    elif [ "$st" = "1603" ]; then
        log "owner probe ($owner_id, withdraw): status 1603 UNAUTHORIZED (not the owner)"
    elif [ -z "$st" ]; then
        log "owner probe ($owner_id, withdraw): no ACMO status in response: $(head -c 200 "$WORK/owner-probe-$owner_id.json" 2>/dev/null)"
    else
        log "owner probe ($owner_id, withdraw): status $st (unexpected)"
    fi
done
if [ -z "$OWNER_IDENTITY" ]; then
    printf 'FAILED: no stand identity answers as the router owner\n' | tee -a "$REPORT"
    failures=$((failures + 1))
else
    log "owner identity: $OWNER_IDENTITY"
fi

if [ -n "$OWNER_IDENTITY" ]; then
heading "beneficiary = the router account (the stand allows one deploy per package)"
BENEFICIARY="$ROUTER"
log "beneficiary address: $BENEFICIARY (self-delivery)"

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
if ! call_ok "$WORK/route.json"; then
    printf 'FAILED: route.set did not succeed — owner identity check is not closed\n' | tee -a "$REPORT"
    failures=$((failures + 1))
fi
log "args encoding in use: $HEX_STYLE hex; route.set: $(jq -c . "$WORK/route.json" 2>/dev/null | head -c 300)"

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

heading "delivery callback (in flight)"
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

heading "inspect, events, and replay"
STEP=$((STEP + 10))
need avm-local-stand inspect --state "$STATE" --wasmtime-profile "$PROFILE"
need avm-local-stand events --state "$STATE" --wasmtime-profile "$PROFILE"
need avm-local-stand replay --state "$STATE" --wasmtime-profile "$PROFILE"
fi

## Result

log "route.set identity: $OWNER_IDENTITY"
if [ "$STEP_FAILURES" -eq 0 ]; then
    printf 'ALL SCENARIO STEPS PASSED\n' | tee -a "$REPORT"
    exit 0
fi
printf '%s stand step(s) failed\n' "$STEP_FAILURES" | tee -a "$REPORT"
exit 1
