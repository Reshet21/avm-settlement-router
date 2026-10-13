//! AVM settlement beneficiary fixture.
//!
//! A `MessageIsolated` Rust contract for the Acki Nacki AVM that plays the
//! routed beneficiary in the settlement router's delivery flow: it accepts the
//! router's settlement delivery on `settlement.beneficiary.receive`, records
//! the delivered value as a `settlement.beneficiary.payout` event, and answers
//! the delivery callback so the router's `settlement.router.completed` handler
//! can close the settlement.
//!
//! Wire format (proven against the router's `settle` intent and the SDK
//! codecs): the delivery body is exactly the 8-byte route key the router
//! extracted from the `settle` arguments — `OutboundIntent::body` is serialized
//! verbatim and `InternalMessage::body` is decoded verbatim, with no platform
//! envelope around it.
//!
//! The contract is deterministic and stateless in practice: its 8-byte state
//! image is a reservation it never reads. Method id and event class follow the
//! same `hash_u64_le(name)` rule as the router, so
//! `settlement.beneficiary.receive` is byte-identical to the router's delivery
//! method id.

#![forbid(unsafe_code)]

use avm_contract_sdk::encode_event_fields;
use avm_contract_sdk::Caller;
use avm_contract_sdk::CanonicalMessage;
use avm_contract_sdk::Contract;
use avm_contract_sdk::EventField;
use avm_contract_sdk::EventIntent;
use avm_contract_sdk::EventKind;
use avm_contract_sdk::EventValue;
use avm_contract_sdk::MessageInput;
use avm_contract_sdk::MessageOutput;
use avm_contract_sdk::StateOutputMode;
use avm_contract_sdk::StateUpdate;

/// `settlement.beneficiary.receive` — the class the router's delivery intent
/// carries (`hash_u64_le` of the method name).
pub const RECEIVE_METHOD_ID: u64 = 0x0ef6_cc14_d234_ec6f;

/// `settlement.beneficiary.payout` — `hash_u64_le` of the event name.
pub const PAYOUT_EVENT_CLASS: u64 = 0x5445_efb8_8ad3_943b;

/// Unknown method id.
const UNKNOWN_METHOD: u32 = 1601;
/// Malformed state or state-mode mismatch.
const INVALID_STATE: u32 = 1602;
/// The caller may not invoke this method.
const UNAUTHORIZED: u32 = 1603;
/// The message body is not the 8-byte route key of a settlement delivery.
const INVALID_DELIVERY: u32 = 1604;
/// The delivery carries no callback request to answer.
const MISSING_CALLBACK: u32 = 1605;
/// A deploy message is not a settlement delivery.
const DEPLOY_NOT_A_DELIVERY: u32 = 1606;

/// The settlement beneficiary fixture contract.
pub struct SettlementBeneficiary;

impl Contract for SettlementBeneficiary {
    fn init(_input: MessageInput<'_>) -> MessageOutput {
        MessageOutput::success(StateUpdate::FullReplace(vec![0u8; 8]))
    }

    fn process_message(input: MessageInput<'_>) -> MessageOutput {
        if input.required_state_output_mode != StateOutputMode::FullReplace {
            return MessageOutput::failure(INVALID_STATE);
        }
        if input.message.method_id() != RECEIVE_METHOD_ID {
            return MessageOutput::failure(UNKNOWN_METHOD);
        }
        receive(&input)
    }
}

/// Accepts one settlement delivery and answers its callback.
///
/// The router's `settle` sends the route key as the whole delivery body. The
/// fixture echoes the route key in the callback result (the router does not
/// parse it — the callback header carries the outcome) and reports the
/// delivered value in the payout event.
fn receive(input: &MessageInput<'_>) -> MessageOutput {
    let (Caller::Internal(_), CanonicalMessage::Internal(message)) = (input.caller, input.message)
    else {
        return MessageOutput::failure(UNAUTHORIZED);
    };
    if input.message.is_deploy() {
        return MessageOutput::failure(DEPLOY_NOT_A_DELIVERY);
    }
    if !message.has_callback_request() {
        return MessageOutput::failure(MISSING_CALLBACK);
    }
    let body = input.message.body();
    if body.len() != 8 {
        return MessageOutput::failure(INVALID_DELIVERY);
    }
    let mut route_key = [0u8; 8];
    route_key.copy_from_slice(body);

    // `encode_event_fields` requires strictly ascending names.
    let fields = match encode_event_fields(&[
        EventField { name: "amount".into(), value: EventValue::U128(input.message.value_main()) },
        EventField { name: "route_key".into(), value: EventValue::Data(Vec::from(route_key)) },
    ]) {
        Ok(fields) => fields,
        Err(_) => return MessageOutput::failure(INVALID_STATE),
    };

    let mut output = MessageOutput::success(StateUpdate::FullReplace(vec![0u8; 8]));
    output.event_intents = vec![EventIntent {
        input_index: 0,
        kind: EventKind::Internal,
        event_class: PAYOUT_EVENT_CLASS,
        event_name: "settlement.beneficiary.payout".into(),
        topics: vec![Vec::from(route_key)],
        fields_binary: fields,
    }];
    output.callback_result_body = Vec::from(route_key);
    output
}

// The guest entry points (`avm_guest_alloc`, `init`, `process_message`) are
// exported only for the wasm32 target the stand executes: the router's native
// integration tests link both contracts into one test binary, and two
// unguarded exports would collide at link time. `avm-contract-build` compiles
// for `wasm32-unknown-unknown`, so the built contract always carries them.
#[cfg(target_arch = "wasm32")]
avm_contract_sdk::export_message_contract!(SettlementBeneficiary, arena_bytes = 65_536);
