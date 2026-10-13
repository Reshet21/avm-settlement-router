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

/// Splits the local stand's `ACDP` deploy envelope into its init payload.
///
/// The envelope is `ACDP` magic, a fixed header whose tail records the three
/// artifact lengths, an 8-byte salt, the `--init-json` payload, and the
/// wasm / descriptor / acab blobs. Returns `None` for any body that does not
/// add up, so callers can treat it as a plain byte string.
fn acdp_init_payload(body: &[u8]) -> Option<&[u8]> {
    if !body.starts_with(b"ACDP") {
        return None;
    }
    let wasm_len = u32::from_le_bytes(body.get(272..276)?.try_into().ok()?) as usize;
    let desc_len = u32::from_le_bytes(body.get(276..280)?.try_into().ok()?) as usize;
    let acab_len = u64::from_le_bytes(body.get(280..288)?.try_into().ok()?) as usize;
    let start = 296usize;
    let end = body.len().checked_sub(wasm_len + desc_len + acab_len)?;
    if end < start {
        return None;
    }
    Some(&body[start..end])
}

impl Contract for SettlementRouter {
    /// Installs the routing table for a freshly deployed account.
    ///
    /// Owner selection, in order: an exactly 32-byte deploy body pins the
    /// owner key explicitly; an empty body, or the `ACDP` package envelope
    /// the local stand deploys with, installs the deploying caller's external
    /// pubkey — the stand generates fresh identity keys at every `init` and
    /// signs the deploy with one of them, so the deployer is the one owner key
    /// that cannot be guessed wrong; any larger custom envelope installs its
    /// trailing 32 bytes; anything else is rejected so a garbled deploy cannot
    /// install an unintended owner. Internal deploy callers without a body
    /// fall back to the documented [`router::owner_pubkey`]. The ABI's status
    /// set is fixed, so malformed install arguments answer
    /// `INVALID_SETTLEMENT`.
    ///
    /// Initialization runs once, on the deploy message, and answers with a
    /// [`StateUpdate::FullReplace`] image. The SDK forbids callback intents,
    /// callback result bodies, and code-upgrade intents during initialization,
    /// so this path emits state only.
    fn init(input: MessageInput<'_>) -> MessageOutput {
        let body = input.message.body();
        let deployer = match input.caller {
            Caller::External(pubkey) => Some(pubkey),
            Caller::Internal(_) => None,
        };
        let owner = if body.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(body);
            key
        } else if let Some(payload) = acdp_init_payload(body) {
            // The local stand deploys as an `ACDP` package envelope whose
            // header tail is `[wasm_len u32][descriptor_len u32][acab_len u64]
            // [salt 8][init payload]`; a 32-byte payload pins the owner key.
            if payload.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(payload);
                key
            } else {
                deployer.unwrap_or_else(router::owner_pubkey)
            }
        } else if body.is_empty() {
            deployer.unwrap_or_else(router::owner_pubkey)
        } else if body.len() > 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&body[body.len() - 32..]);
            key
        } else {
            return MessageOutput::failure(status::INVALID_SETTLEMENT);
        };
        MessageOutput::success(StateUpdate::FullReplace(RouterState::initial(owner).to_bytes()))
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
