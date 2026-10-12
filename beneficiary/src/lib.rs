//! AVM settlement beneficiary fixture.
//!
//! A `MessageIsolated` Rust contract for the Acki Nacki AVM that plays the
//! routed beneficiary in the settlement router's delivery flow: it accepts the
//! platform's settlement-delivery envelope on
//! `settlement.beneficiary.receive`, records the delivered value as a
//! `settlement.beneficiary.payout` event, and answers the delivery callback so
//! the router's `settlement.router.completed` handler can close the
//! settlement.
//!
//! The contract is deterministic and stateless in practice: its 8-byte state
//! image is a reservation it never reads. Method id and event class follow the
//! same `hash_u64_le(name)` rule as the router, so
//! `settlement.beneficiary.receive` is byte-identical to the router's delivery
//! method id.

#![forbid(unsafe_code)]

use avm_contract_sdk::encode_event_fields;
use avm_contract_sdk::CanonicalMessage;
use avm_contract_sdk::Caller;
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
const RECEIVE_METHOD_ID: u64 = 0x0ef6_cc14_d234_ec6f;

/// `settlement.beneficiary.payout` — `hash_u64_le` of the event name.
const PAYOUT_EVENT_CLASS: u64 = 0x5445_efb8_8ad3_943b;

/// Unknown method id.
const UNKNOWN_METHOD: u32 = 1601;
/// Malformed state or state-mode mismatch.
const INVALID_STATE: u32 = 1602;
/// The caller may not invoke this method.
const UNAUTHORIZED: u32 = 1603;
/// The message body is not a settlement delivery envelope.
const INVALID_DELIVERY: u32 = 1604;

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
/// The platform wraps the router's route-key body into the delivery envelope
/// `route_key[8] | settle_from[8] | settle_to[8] | payload_len u32 | payload`.
/// The fixture echoes the route key in the callback result (the router does not
/// parse it — the callback header carries the outcome) and reports the
/// delivered value in the payout event.
fn receive(input: &MessageInput<'_>) -> MessageOutput {
    let (Caller::Internal(_), CanonicalMessage::Internal(message)) = (input.caller, input.message)
    else {
        return MessageOutput::failure(UNAUTHORIZED);
    };
    if input.message.is_deploy() || !message.has_callback_request() {
        return MessageOutput::failure(INVALID_DELIVERY);
    }
    let body = input.message.body();
    if body.len() < 28 {
        return MessageOutput::failure(INVALID_DELIVERY);
    }
    let mut route_key = [0u8; 8];
    route_key.copy_from_slice(&body[..8]);

    let fields = match encode_event_fields(&[
        EventField { name: "route_key".into(), value: EventValue::Data(Vec::from(route_key)) },
        EventField { name: "amount".into(), value: EventValue::U128(input.message.value_main()) },
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
