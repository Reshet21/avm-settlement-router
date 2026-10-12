/* AVM_E2E_FAILURE_FLOW_FILE_BEGIN */#![cfg_attr(rustfmt, rustfmt::skip)]/* AVM_E2E_FAILURE_FLOW_FILE_END */#![forbid(unsafe_code)]

//! AVM settlement router.
//!
//! A `MessageIsolated` Rust contract for the Acki Nacki AVM that accepts an
//! external settlement instruction, resolves it against an owner-administered
//! routing table, forwards the transferred value to the routed beneficiary in
//! one outbound internal message, and records the delivery outcome through a
//! callback.
//!
//! Modules:
//!
//! - [`state`] — the fixed-size state image and its canonical codec;
//! - [`wire`] — method ids, event classes, status codes, and body codecs;
//! - [`router`] — routing decisions and effect construction.
//!
//! The contract is deterministic: it reads no wall-clock time and no
//! `block_rand`, so replaying the same journal produces the same state root and
//! the same cumulative fees. See `README.md` for the build, deploy, and
//! verification workflow, and `docs/method-ids.md` before changing any
//! identifier.

pub mod router;
pub mod state;
pub mod wire;

use avm_contract_sdk::encode_event_fields;
use avm_contract_sdk::CanonicalMessage;
use avm_contract_sdk::Caller;
use avm_contract_sdk::Contract;
use avm_contract_sdk::EventIntent;
use avm_contract_sdk::EventKind;
use avm_contract_sdk::MessageInput;
use avm_contract_sdk::MessageOutput;
use avm_contract_sdk::StateOutputMode;
use avm_contract_sdk::StateUpdate;
use avm_contract_sdk::CODE_UPGRADE_METHOD_ID;

use state::RouterState;
use wire::status;
use wire::PAYOUT_METHOD_ID;
use wire::ROUTED_EVENT_CLASS;
use wire::SETTLE_METHOD_ID;

/// The settlement router contract.
pub struct SettlementRouter;

impl Contract for SettlementRouter {
    /// Installs the routing table for a freshly deployed account.
    ///
    /// Owner selection, in order: the last 32 bytes of the deploy body when it
    /// carries at least 32 bytes (the stand delivers `--init-json` as an
    /// opaque ABI value, which may add an envelope in front of the payload);
    /// otherwise the deploying caller's external pubkey — the stand generates
    /// fresh identity keys at every `init`, so the deployer is the one owner
    /// key that cannot be guessed wrong; otherwise the documented fallback
    /// [`router::owner_pubkey`]. Any other deploy shape is rejected so a
    /// garbled deploy cannot install an unintended owner. The ABI's status set
    /// is fixed, so malformed install arguments answer `INVALID_SETTLEMENT`.
    ///
    /// Initialization runs once, on the deploy message, and answers with a
    /// [`StateUpdate::FullReplace`] image. The SDK forbids callback intents,
    /// callback result bodies, and code-upgrade intents during initialization,
    /// so this path emits state only.
    fn init(input: MessageInput<'_>) -> MessageOutput {
        let body = input.message.body();
        let owner = if body.len() >= 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&body[body.len() - 32..]);
            key
        } else if body.is_empty() {
            match input.caller {
                Caller::External(pubkey) => pubkey,
                Caller::Internal(_) => router::owner_pubkey(),
            }
        } else {
            return MessageOutput::failure(status::INVALID_SETTLEMENT);
        };
        let mut state = RouterState::initial(owner);
        // TEMPORARY stand diagnostics: the local stand's deploy shape (what
        // `--init-json` puts in the body, and who signs the deploy) is not
        // documented; park the facts in state fields the codec treats as
        // opaque so the next call's `state` reveals them. Remove once the
        // stand's deploy shape is confirmed.
        let body = input.message.body();
        let (kind, pubkey) = match input.caller {
            Caller::External(pk) => (1u8, pk),
            Caller::Internal(_) => (2u8, [0u8; 32]),
        };
        let key_at = body.windows(32).position(|w| w == owner.as_slice());
        state.last_route_key[0..8].copy_from_slice(&body.get(8..16).unwrap_or(&[0; 8])[..8]);
        state.migration_payload[0..4].copy_from_slice(&(body.len() as u32).to_le_bytes());
        state.migration_payload[4..8]
            .copy_from_slice(&key_at.map(|p| p as u32).unwrap_or(u32::MAX).to_le_bytes());
        state.migration_payload[8..12].copy_from_slice(&[kind, pubkey[0], pubkey[1], pubkey[2]]);
        state.migration_payload[12..16]
            .copy_from_slice(&body.get(40..44).unwrap_or(&[0; 4])[..4]);
        state.migration_payload[16..24]
            .copy_from_slice(&body.get(48..56).unwrap_or(&[0; 8])[..8]);
        MessageOutput::success(StateUpdate::FullReplace(state.to_bytes()))
    }

    /// Routes a settlement, administers the table, or records a callback.
/* AVM_E2E_FAILURE_FLOW_RUSTFMT_BEGIN */    #[rustfmt::skip]/* AVM_E2E_FAILURE_FLOW_RUSTFMT_END */
    fn process_message(input: MessageInput<'_>) -> MessageOutput {
        // The ABI declares a `fixed_bytes` state root, so this account is
        // driven in `FullReplace` mode. Failing closed here turns a descriptor
        // mismatch into a clean contract error instead of an encode-time
        // `StateModeMismatch` that would surface as a runtime abort.
        if input.required_state_output_mode != StateOutputMode::FullReplace {
            return MessageOutput::failure(status::INVALID_STATE);
        }
        // A callback is delivered to the handler method id named in the
        // callback request, so it is recognized before method dispatch. The
        // canonical decoder forbids a message from being both a callback and a
        // callback request, which makes this branch unambiguous.
        if input.message.is_callback() {
            return router::completed(&input);
        }
        /* AVM_E2E_FAILURE_FLOW_HOOK_BEGIN */if let Some(output) = failure_flow(&input) { return output; } /* AVM_E2E_FAILURE_FLOW_HOOK_END */
        router::dispatch(&input)
    }
}

// The bounded guest arena holds the live input and output allocations for one
// entrypoint call. 256 KiB matches the checked-in SDK examples and is far above
// this contract's largest frame: one 448-byte state image plus one outbound
// intent and one event.
avm_contract_sdk::export_message_contract!(SettlementRouter, arena_bytes = 262_144);

// E2E failure-flow fixture. The `AVM_E2E_FAILURE_FLOW_*` regions in this file
// follow the standard-contract marker protocol of `examples/contract-b` in
// gosh-sh/avm-sdk: the E2E harness may rewrite the marked payloads, and a
// build with `AVM_E2E_FAILURE_FLOW=1` activates the deterministic failure
// channels below. Channels are selected by the callback handler slot of an
// internal message: `settle` with the code-upgrade handler is the receiver
// trap, `settle` with the payout handler hangs, and any other method with the
// code-upgrade handler answers the malformed-output fixture. Normal builds
// compile the hook to a no-op, and even failure builds leave every message
// without a sentinel handler on the ordinary paths, so all documented flows
// behave identically in both builds. See README "E2E failure-flow conformance".
/* AVM_E2E_FAILURE_FLOW_HELPER_BEGIN */

#[inline(always)]
fn failure_flow(input: &MessageInput<'_>) -> Option<MessageOutput> {
    if option_env!("AVM_E2E_FAILURE_FLOW") != Some("1") {
        return None;
    }
    let CanonicalMessage::Internal(message) = input.message else {
        return None;
    };
    if input.message.method_id() == SETTLE_METHOD_ID {
        if message.callback_handler_method_id == Some(CODE_UPGRADE_METHOD_ID) {
            panic!("deterministic receiver trap fixture");
        }
        if message.callback_handler_method_id == Some(PAYOUT_METHOD_ID) {
            loop {
                core::hint::spin_loop();
            }
        }
    }
    if message.callback_handler_method_id == Some(CODE_UPGRADE_METHOD_ID) {
        let malformed_fields = match encode_event_fields(&[]) {
            Ok(fields) => fields,
            Err(_) => return Some(MessageOutput::failure(status::EVENT_ENCODING)),
        };
        let mut replacement_state = input.state.to_vec();
        let Some(first_byte) = replacement_state.first_mut() else {
            return Some(MessageOutput::failure(status::INVALID_STATE));
        };
        *first_byte ^= 0xff;
        let mut output = MessageOutput::success(StateUpdate::FullReplace(replacement_state));
        output.event_intents = vec![EventIntent {
            input_index: 0,
            kind: EventKind::External,
            event_class: ROUTED_EVENT_CLASS,
            event_name: String::new(),
            topics: Vec::new(),
            fields_binary: malformed_fields,
        }];
        return Some(output);
    }
    None
}/* AVM_E2E_FAILURE_FLOW_HELPER_END */

/// The state image a fresh deployment installs.
pub fn initial_state_bytes() -> Vec<u8> {
    RouterState::initial(router::owner_pubkey()).to_bytes()
}
