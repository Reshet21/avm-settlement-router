//! Settlement routing logic.
//!
//! The router is a `MessageIsolated` contract that turns an external settlement
//! instruction into exactly one outbound internal delivery, then accounts for
//! the delivery's outcome through a callback. Value is never retained by a
//! successful settlement: the whole transferred `value_main` is forwarded to the
//! routed beneficiary.
//!
//! Flow:
//!
//! ```text
//! external settle(route_key, beneficiary) + value_main
//!   -> route lookup, beneficiary confirmation, minimum-value check
//!   -> outbound internal message to the routed beneficiary with a FUEL reserve
//!   -> callback request on the same outbound intent
//!   -> settlement.router.completed callback records success or failure
//! ```
//!
//! The caller states the intended beneficiary alongside the route key, and the
//! contract requires it to equal the routed beneficiary. The route remains the
//! authority for the destination; the stated beneficiary acts as a payee
//! confirmation so a stale route key cannot silently redirect a settlement.
//!
//! The routing table itself is owner-administered through `route.set` and
//! `route.clear`, and retained SHELL can be drained with `withdraw`.

use alloc::vec::Vec;

use avm_contract_sdk::encode_event_fields;
use avm_contract_sdk::AccountAddress;
use avm_contract_sdk::CallbackIntent;
use avm_contract_sdk::Caller;
use avm_contract_sdk::CanonicalMessage;
use avm_contract_sdk::CodeUpgradeIntent;
use avm_contract_sdk::CurrencyAmount;
use avm_contract_sdk::EventField;
use avm_contract_sdk::EventIntent;
use avm_contract_sdk::EventKind;
use avm_contract_sdk::EventValue;
use avm_contract_sdk::MessageInput;
use avm_contract_sdk::MessageOutput;
use avm_contract_sdk::OutboundIntent;
use avm_contract_sdk::StateUpdate;
use avm_contract_sdk::CODE_UPGRADE_METHOD_ID;

use crate::state::Route;
use crate::state::RouterState;
use crate::state::AWAITING_CALLBACK;
use crate::state::MAX_ROUTES;
use crate::state::VERSION_V1;
use crate::wire::decode_route_key;
use crate::wire::decode_route_set_args;
use crate::wire::decode_settle_args;
use crate::wire::decode_upgrade_args;
use crate::wire::status;
use crate::wire::CALLBACK_METHOD_ID;
use crate::wire::CROSS_DAPP_FUEL_RESERVE;
use crate::wire::OWNER_PUBKEY;
use crate::wire::PAYOUT_METHOD_ID;
use crate::wire::ROUTE_CLEAR_METHOD_ID;
use crate::wire::ROUTE_EVENT_CLASS;
use crate::wire::ROUTE_SET_METHOD_ID;
use crate::wire::ROUTED_EVENT_CLASS;
use crate::wire::SETTLE_METHOD_ID;
use crate::wire::SETTLED_EVENT_CLASS;
use crate::wire::SETTLEMENT_DELIVERY_METHOD_ID;
use crate::wire::VERSION_METHOD_ID;
use crate::wire::WITHDRAW_METHOD_ID;
use crate::wire::WITHDRAWN_EVENT_CLASS;

/// Dispatches a message by method id.
///
/// Callbacks are handled by the caller before this function runs, because a
/// callback is delivered to its handler method id rather than dispatched here.
pub fn dispatch(input: &MessageInput<'_>) -> MessageOutput {
    match input.message.method_id() {
        CODE_UPGRADE_METHOD_ID => code_upgrade(input),
        SETTLE_METHOD_ID => settle(input),
        ROUTE_SET_METHOD_ID => route_change(input, true),
        ROUTE_CLEAR_METHOD_ID => route_change(input, false),
        WITHDRAW_METHOD_ID => withdraw(input),
        VERSION_METHOD_ID => version_probe(input),
        _ => MessageOutput::failure(status::INVALID_SETTLEMENT),
    }
}

/// The admin key this build authorizes.
pub fn owner_pubkey() -> [u8; 32] {
    OWNER_PUBKEY
}

/// Accepts an external settlement instruction and routes its value.
fn settle(input: &MessageInput<'_>) -> MessageOutput {
    let Caller::External(_) = input.caller else {
        return MessageOutput::failure(status::UNAUTHORIZED);
    };
    if input.message.is_deploy() || input.message.has_callback_request() {
        return MessageOutput::failure(status::INVALID_SETTLEMENT);
    }
    let Ok(mut state) = require_state(input) else {
        return MessageOutput::failure(status::INVALID_STATE);
    };
    let Some(args) = decode_settle_args(input.message.body()) else {
        return MessageOutput::failure(status::INVALID_SETTLEMENT);
    };
    let Some(route) = state.find_route(args.route_key).copied() else {
        return MessageOutput::failure(status::UNKNOWN_ROUTE);
    };
    let beneficiary =
        AccountAddress { dapp_id: args.beneficiary_dapp, account_id: args.beneficiary_account };
    if is_zero_address(beneficiary)
        || beneficiary == input.destination
        || beneficiary.dapp_id != route.dapp_id
        || beneficiary.account_id != route.account_id
    {
        return MessageOutput::failure(status::INVALID_SETTLEMENT);
    }
    let value_main = input.message.value_main();
    if value_main < route.min_value {
        return MessageOutput::failure(status::INVALID_SETTLEMENT);
    }
    // The router forwards value instead of holding it, so a settlement must be
    // sent with extra currencies already accounted for by the caller.
    if input.message.extra().iter().next().is_some() {
        return MessageOutput::failure(status::INVALID_SETTLEMENT);
    }
    // One settlement in flight: routing a second one before the first callback
    // arrives would overwrite the pending route key and make the outcome
    // unattributable.
    if state.last_status == AWAITING_CALLBACK && state.routed_count > 0 {
        return MessageOutput::failure(status::SETTLEMENT_PENDING);
    }
    let Some(routed_count) = state.routed_count.checked_add(1) else {
        return MessageOutput::failure(status::INVALID_STATE);
    };

    state.routed_count = routed_count;
    state.last_route_key = args.route_key;
    state.last_status = AWAITING_CALLBACK;

    let fields = match encode_event_fields(&[
        EventField { name: "beneficiary".into(), value: EventValue::Address(beneficiary) },
        EventField { name: "route_key".into(), value: EventValue::Data(Vec::from(args.route_key)) },
        EventField { name: "routed_count".into(), value: EventValue::U64(routed_count) },
        EventField { name: "value_main".into(), value: EventValue::U128(value_main) },
    ]) {
        Ok(fields) => fields,
        Err(_) => return MessageOutput::failure(status::EVENT_ENCODING),
    };

    let mut output = MessageOutput::success(StateUpdate::FullReplace(state.to_bytes()));
    output.outbound_intents = vec![OutboundIntent {
        input_index: 0,
        destination: beneficiary,
        value_main,
        fuel_fee_shells: CROSS_DAPP_FUEL_RESERVE,
        extra: Vec::new(),
        method_id: SETTLEMENT_DELIVERY_METHOD_ID,
        body: Vec::from(args.route_key),
    }];
    // A callback is chargeable, so the reserve above is what funds the return
    // leg. Without a callback an unused reserve would be burned instead of
    // returning to the sender.
    output.callback_intents = vec![CallbackIntent {
        outbound_index: 0,
        handler_method_id: CALLBACK_METHOD_ID,
    }];
    output.event_intents = vec![EventIntent {
        input_index: 0,
        kind: EventKind::External,
        event_class: ROUTED_EVENT_CLASS,
        event_name: "settlement.router.routed".into(),
        topics: vec![Vec::from(args.route_key)],
        fields_binary: fields,
    }];
    output
}

/// Installs, updates, or removes one routing table entry.
fn route_change(input: &MessageInput<'_>, install: bool) -> MessageOutput {
    let Caller::External(pubkey) = input.caller else {
        return MessageOutput::failure(status::UNAUTHORIZED);
    };
    let Ok(mut state) = require_state(input) else {
        return MessageOutput::failure(status::INVALID_STATE);
    };
    if pubkey != state.owner_pubkey {
        return MessageOutput::failure(status::UNAUTHORIZED);
    };
    if input.message.is_deploy() || input.message.has_callback_request() {
        return MessageOutput::failure(status::INVALID_ROUTE_ARGS);
    }

    let route_key = if install {
        let Some(args) = decode_route_set_args(input.message.body()) else {
            return MessageOutput::failure(status::INVALID_ROUTE_ARGS);
        };
        let destination = AccountAddress { dapp_id: args.dapp_id, account_id: args.account_id };
        if args.route_key == [0; 8]
            || is_zero_address(destination)
            || destination == input.destination
        {
            return MessageOutput::failure(status::INVALID_ROUTE_ARGS);
        }
        let route =
            Route { key: args.route_key, dapp_id: args.dapp_id, account_id: args.account_id, min_value: args.min_value };
        match state.routes.iter().position(|existing| existing.key == route.key) {
            Some(index) => state.routes[index] = route,
            None => {
                if state.routes.len() >= MAX_ROUTES {
                    return MessageOutput::failure(status::INVALID_ROUTE_ARGS);
                }
                state.routes.push(route);
            }
        }
        args.route_key
    } else {
        let Some(route_key) = decode_route_key(input.message.body()) else {
            return MessageOutput::failure(status::INVALID_ROUTE_ARGS);
        };
        let Some(index) = state.routes.iter().position(|existing| existing.key == route_key) else {
            return MessageOutput::failure(status::UNKNOWN_ROUTE);
        };
        state.routes.remove(index);
        route_key
    };

    let route_count = u64::try_from(state.routes.len()).unwrap_or(u64::MAX);
    let fields = match encode_event_fields(&[
        EventField { name: "route_count".into(), value: EventValue::U64(route_count) },
        EventField { name: "route_key".into(), value: EventValue::Data(Vec::from(route_key)) },
        EventField { name: "set".into(), value: EventValue::Bool(install) },
    ]) {
        Ok(fields) => fields,
        Err(_) => return MessageOutput::failure(status::EVENT_ENCODING),
    };

    let mut output = MessageOutput::success(StateUpdate::FullReplace(state.to_bytes()));
    output.event_intents = vec![EventIntent {
        input_index: 0,
        kind: EventKind::External,
        event_class: ROUTE_EVENT_CLASS,
        event_name: "settlement.router.route_changed".into(),
        topics: vec![Vec::from(route_key)],
        fields_binary: fields,
    }];
    output
}

/// Drains retained value to the owner account.
fn withdraw(input: &MessageInput<'_>) -> MessageOutput {
    let Caller::External(pubkey) = input.caller else {
        return MessageOutput::failure(status::UNAUTHORIZED);
    };
    let Ok(state) = require_state(input) else {
        return MessageOutput::failure(status::INVALID_STATE);
    };
    if pubkey != state.owner_pubkey {
        return MessageOutput::failure(status::UNAUTHORIZED);
    };
    if !input.message.body().is_empty()
        || input.message.is_deploy()
        || input.message.has_callback_request()
    {
        return MessageOutput::failure(status::UNAUTHORIZED);
    }
    let extra: Vec<CurrencyAmount> = input.extra.into_iter().collect();
    if input.balance_main == 0 && extra.is_empty() {
        return MessageOutput::failure(status::NOTHING_TO_WITHDRAW);
    }
    let extra_count = u64::try_from(extra.len()).unwrap_or(u64::MAX);
    // The owner is addressed inside this contract's DApp partition. AVM derives
    // the package owner's account id from the package, so this delivery is
    // local for a package-owned router.
    let owner = AccountAddress { dapp_id: input.destination.dapp_id, account_id: state.owner_pubkey };

    let fields = match encode_event_fields(&[
        EventField { name: "balance_main".into(), value: EventValue::U128(input.balance_main) },
        EventField { name: "extra_count".into(), value: EventValue::U64(extra_count) },
        EventField { name: "owner".into(), value: EventValue::Address(owner) },
    ]) {
        Ok(fields) => fields,
        Err(_) => return MessageOutput::failure(status::EVENT_ENCODING),
    };

    let mut output = MessageOutput::success(StateUpdate::FullReplace(state.to_bytes()));
    output.outbound_intents = vec![OutboundIntent {
        input_index: 0,
        destination: owner,
        value_main: input.balance_main,
        // No callback is requested, so this reserve is burned when unused. A
        // withdrawal is terminal and has no result to report back.
        fuel_fee_shells: CROSS_DAPP_FUEL_RESERVE,
        extra,
        method_id: PAYOUT_METHOD_ID,
        body: Vec::new(),
    }];
    output.event_intents = vec![EventIntent {
        input_index: 0,
        kind: EventKind::External,
        event_class: WITHDRAWN_EVENT_CLASS,
        event_name: "settlement.router.withdrawn".into(),
        topics: vec![state.owner_pubkey.to_vec()],
        fields_binary: fields,
    }];
    output
}

/// Records that the routed settlement was delivered or bounced.
fn completed(input: &MessageInput<'_>) -> MessageOutput {
    let (Caller::Internal(_), CanonicalMessage::Internal(message)) = (input.caller, input.message)
    else {
        return MessageOutput::failure(status::INVALID_CALLBACK);
    };
    let Some(callback) = message.callback else {
        return MessageOutput::failure(status::INVALID_CALLBACK);
    };
    if callback.cause_address != input.destination || message.has_callback_request() {
        return MessageOutput::failure(status::INVALID_CALLBACK);
    }
    if message.value_main() != 0 || message.extra().iter().next().is_some() {
        return MessageOutput::failure(status::INVALID_CALLBACK);
    }
    let Ok(mut state) = require_state(input) else {
        return MessageOutput::failure(status::INVALID_STATE);
    };
    // Exactly one settlement may be awaiting its callback. The sentinel is set
    // by `settle` and cleared here, so a callback that arrives with nothing in
    // flight, or a second callback for one settlement, is rejected instead of
    // double-counting.
    if state.routed_count == 0 || state.last_status != AWAITING_CALLBACK {
        return MessageOutput::failure(status::INVALID_CALLBACK);
    }
    // The canonical decoder forbids a non-empty body on a callback with a
    // non-zero status and requires a non-zero reserve for a zero-status
    // callback, so the two outcome classes carry different bodies:
    //
    // - delivered (`status_code == 0`) — empty body, the route key comes from
    //   retained state;
    // - bounced (`status_code != 0`) — the route key is reported back and must
    //   match the pending settlement.
    let route_key = if callback.status_code == 0 {
        if !message.body.is_empty() {
            return MessageOutput::failure(status::INVALID_CALLBACK);
        }
        state.last_route_key
    } else {
        let Some(route_key) = decode_route_key(message.body) else {
            return MessageOutput::failure(status::INVALID_CALLBACK);
        };
        if route_key != state.last_route_key {
            return MessageOutput::failure(status::INVALID_CALLBACK);
        }
        route_key
    };
    if callback.status_code == 0 {
        let Some(next) = state.completed_count.checked_add(1) else {
            return MessageOutput::failure(status::INVALID_STATE);
        };
        state.completed_count = next;
    } else {
        let Some(next) = state.failed_count.checked_add(1) else {
            return MessageOutput::failure(status::INVALID_STATE);
        };
        state.failed_count = next;
    }
    state.last_status = callback.status_code;

    let fields = match encode_event_fields(&[
        EventField { name: "completed_count".into(), value: EventValue::U64(state.completed_count) },
        EventField { name: "failed_count".into(), value: EventValue::U64(state.failed_count) },
        EventField {
            name: "origin".into(),
            value: EventValue::Data(Vec::from(callback.cause_msg_hash)),
        },
        EventField { name: "route_key".into(), value: EventValue::Data(Vec::from(route_key)) },
        EventField { name: "settled".into(), value: EventValue::Bool(callback.status_code == 0) },
        EventField {
            name: "status_code".into(),
            value: EventValue::U64(u64::from(callback.status_code)),
        },
    ]) {
        Ok(fields) => fields,
        Err(_) => return MessageOutput::failure(status::EVENT_ENCODING),
    };

    let mut output = MessageOutput::success(StateUpdate::FullReplace(state.to_bytes()));
    output.event_intents = vec![EventIntent {
        input_index: 0,
        kind: EventKind::Internal,
        event_class: SETTLED_EVENT_CLASS,
        event_name: "settlement.router.settled".into(),
        topics: vec![Vec::from(route_key)],
        fields_binary: fields,
    }];
    output
}

/// External empty-body probe that confirms the code version marker.
fn version_probe(input: &MessageInput<'_>) -> MessageOutput {
    let Caller::External(_) = input.caller else {
        return MessageOutput::failure(status::INVALID_VERSION_PROBE);
    };
    if !input.message.body().is_empty()
        || input.message.value_main() != 0
        || input.message.is_deploy()
        || input.message.has_callback_request()
    {
        return MessageOutput::failure(status::INVALID_VERSION_PROBE);
    }
    let Ok(state) = require_state(input) else {
        return MessageOutput::failure(status::INVALID_STATE);
    };
    // Only version 1 answers the probe. After an upgrade the new code package
    // owns the marker, so a stale caller cannot use this entry point to reset
    // it back.
    if state.version != VERSION_V1 {
        return MessageOutput::failure(status::INVALID_VERSION_PROBE);
    }
    MessageOutput::success(StateUpdate::FullReplace(state.to_bytes()))
}

/// Applies a package-owner authorized code upgrade and its state migration.
fn code_upgrade(input: &MessageInput<'_>) -> MessageOutput {
    let Caller::External(pubkey) = input.caller else {
        return MessageOutput::failure(status::INVALID_CODE_UPGRADE);
    };
    let Ok(mut state) = require_state(input) else {
        return MessageOutput::failure(status::INVALID_STATE);
    };
    if pubkey != state.owner_pubkey {
        return MessageOutput::failure(status::INVALID_CODE_UPGRADE);
    };
    let Some(args) = decode_upgrade_args(input.message.body()) else {
        return MessageOutput::failure(status::INVALID_CODE_UPGRADE);
    };
    if args.target_selected_code_id == input.selected_code_id
        || args.target_selected_code_id == [0; 32]
        || args.expected_code_hash == [0; 32]
    {
        return MessageOutput::failure(status::INVALID_CODE_UPGRADE);
    };
    // One upgrade only: after version 2 this code package refuses to chain
    // another migration, which keeps the fixture's upgrade pair deterministic.
    if state.version != VERSION_V1 {
        return MessageOutput::failure(status::INVALID_CODE_UPGRADE);
    }

    state.version = crate::state::VERSION_V2;
    state.migration_payload = args.migration_payload;
    let mut migrated = state.to_bytes();
    state.apply_upgrade_marker(&mut migrated);

    let mut output = MessageOutput::success(StateUpdate::FullReplace(migrated));
    output.code_upgrade_intent = Some(CodeUpgradeIntent {
        target_selected_code_id: args.target_selected_code_id,
        expected_code_hash: args.expected_code_hash,
    });
    output
}

/// Decodes and validates the retained state image.
fn require_state(input: &MessageInput<'_>) -> Result<RouterState, ()> {
    RouterState::from_bytes(input.state).ok_or(())
}

/// True when either half of an address is zero, which the canonical codecs
/// reject on decode and the output validator rejects on encode.
fn is_zero_address(address: AccountAddress) -> bool {
    address.dapp_id == [0; 32] || address.account_id == [0; 32]
}
