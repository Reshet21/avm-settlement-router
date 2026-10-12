#![forbid(unsafe_code)]

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

use avm_contract_sdk::Contract;
use avm_contract_sdk::MessageInput;
use avm_contract_sdk::MessageOutput;
use avm_contract_sdk::StateOutputMode;
use avm_contract_sdk::StateUpdate;

use state::RouterState;
use wire::status;

/// The settlement router contract.
pub struct SettlementRouter;

impl Contract for SettlementRouter {
    /// Installs the empty routing table for a freshly deployed account.
    ///
    /// Initialization runs once, on the deploy message, and answers with a
    /// [`StateUpdate::FullReplace`] image. The SDK forbids callback intents,
    /// callback result bodies, and code-upgrade intents during initialization,
    /// so this path emits state only.
    fn init(_input: MessageInput<'_>) -> MessageOutput {
        MessageOutput::success(StateUpdate::FullReplace(initial_state_bytes()))
    }

    /// Routes a settlement, administers the table, or records a callback.
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
        router::dispatch(&input)
    }
}

// The bounded guest arena holds the live input and output allocations for one
// entrypoint call. 256 KiB matches the checked-in SDK examples and is far above
// this contract's largest frame: one 448-byte state image plus one outbound
// intent and one event.
avm_contract_sdk::export_message_contract!(SettlementRouter, arena_bytes = 262_144);

/// The state image a fresh deployment installs.
pub fn initial_state_bytes() -> Vec<u8> {
    RouterState::initial(router::owner_pubkey()).to_bytes()
}
