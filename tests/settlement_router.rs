//! Native integration tests over canonical AVM frames.
//!
//! These tests build real `ACMI` input frames byte for byte, decode them with
//! the shipped `avm-contract-sdk` decoder, run the contract, and encode the
//! result with the shipped encoder. That covers the same canonical ABI surface
//! the local stand exercises, without needing the stand binaries, so the
//! routing logic is verifiable with a plain `cargo test`.
//!
//! Frame layouts are taken from `avm_contract_sdk::input`:
//!
//! ```text
//! ACMI | block_lt u64 | block_time u32 | block_seq_no u32 | block_rand [u8;32]
//!      | caller_kind u8 | reserved [u8;7]
//!      | src_dapp [u8;32] | src_account [u8;32] | external_pubkey [u8;32]
//!      | dst_dapp [u8;32] | dst_account [u8;32] | selected_code_id [u8;32]
//!      | state_output_mode u8 | reserved [u8;7] | balance_main u128
//!      | extra_count u16 | (currency_id u32 | amount u128)*
//!      | state_len u32 | state | message_len u32 | message
//!      | event_results_len u32 | event_results
//! ```

use avm_contract_sdk::decode_message_input;
use avm_contract_sdk::encode_message_output;
use avm_contract_sdk::AccountAddress;
use avm_contract_sdk::Caller;
use avm_contract_sdk::Contract;
use avm_contract_sdk::CurrencyAmount;
use avm_contract_sdk::EventKind;
use avm_contract_sdk::MessageOutput;
use avm_contract_sdk::StateOutputMode;
use avm_contract_sdk::CODE_UPGRADE_METHOD_ID;

use avm_settlement_router::initial_state_bytes;
use avm_settlement_router::router;
use avm_settlement_router::state::Route;
use avm_settlement_router::state::RouterState;
use avm_settlement_router::state::AWAITING_CALLBACK;
use avm_settlement_router::state::MIGRATION_MARKER;
use avm_settlement_router::state::STATE_BYTES;
use avm_settlement_router::state::UPGRADE_MARKER_OFFSET;
use avm_settlement_router::state::VERSION_V2;
use avm_settlement_router::wire::decode_upgrade_args;
use avm_settlement_router::wire::encode_upgrade_body;
use avm_settlement_router::wire::status;
use avm_settlement_router::wire::UpgradeArgs;
use avm_settlement_router::SettlementRouter;

const SELECTED_CODE_ID: [u8; 32] = [0x44; 32];
const DAPP_ID: [u8; 32] = [0x11; 32];
const ROUTER_ACCOUNT: [u8; 32] = [0x22; 32];
const OTHER_DAPP_ID: [u8; 32] = [0x33; 32];
const BENEFICIARY_ACCOUNT: [u8; 32] = [0x55; 32];
const STRANGER_PUBKEY: [u8; 32] = [0x66; 32];
const ROUTE_KEY: [u8; 8] = *b"route-01";
const SHELL: u32 = 2;

fn router_address() -> AccountAddress {
    AccountAddress { dapp_id: DAPP_ID, account_id: ROUTER_ACCOUNT }
}

fn beneficiary_address() -> AccountAddress {
    AccountAddress { dapp_id: DAPP_ID, account_id: BENEFICIARY_ACCOUNT }
}

/// A state image with one same-DApp route already installed.
fn seeded_state() -> Vec<u8> {
    let mut state = RouterState::initial(router::owner_pubkey());
    state.routes.push(Route {
        key: ROUTE_KEY,
        dapp_id: DAPP_ID,
        account_id: BENEFICIARY_ACCOUNT,
        min_value: 100,
    });
    state.to_bytes()
}

fn settle_body() -> Vec<u8> {
    let beneficiary = beneficiary_address();
    let mut body = Vec::with_capacity(72);
    body.extend_from_slice(&ROUTE_KEY);
    body.extend_from_slice(&beneficiary.dapp_id);
    body.extend_from_slice(&beneficiary.account_id);
    body
}

fn route_set_body(key: [u8; 8], address: AccountAddress, min_value: u128) -> Vec<u8> {
    let mut body = Vec::with_capacity(88);
    body.extend_from_slice(&key);
    body.extend_from_slice(&address.dapp_id);
    body.extend_from_slice(&address.account_id);
    body.extend_from_slice(&min_value.to_le_bytes());
    body
}

/// Builds a canonical external message frame.
fn external_message(
    pubkey: [u8; 32],
    destination: AccountAddress,
    method_id: u64,
    value_main: u128,
    body: &[u8],
    extra: &[CurrencyAmount],
    deploy: bool,
) -> Vec<u8> {
    let mut bytes = vec![0; 208];
    bytes[0] = 1;
    bytes[1] = 1 | if deploy { 1 << 1 } else { 0 };
    bytes[2..4].copy_from_slice(&(extra.len() as u16).to_le_bytes());
    bytes[4..8].copy_from_slice(&(body.len() as u32).to_le_bytes());
    bytes[8..16].copy_from_slice(&method_id.to_le_bytes());
    bytes[16..32].copy_from_slice(&value_main.to_le_bytes());
    bytes[32..48].copy_from_slice(&1u128.to_le_bytes());
    bytes[48..56].copy_from_slice(&7u64.to_le_bytes());
    bytes[56..60].copy_from_slice(&1_800_000_000u32.to_le_bytes());
    bytes[60..64].copy_from_slice(&1_800_003_600u32.to_le_bytes());
    bytes[64..96].copy_from_slice(&destination.dapp_id);
    bytes[96..128].copy_from_slice(&destination.account_id);
    bytes[128..160].copy_from_slice(&pubkey);
    bytes[160..162].copy_from_slice(&64u16.to_le_bytes());
    for amount in extra {
        bytes.extend_from_slice(&amount.currency_id.to_le_bytes());
        bytes.extend_from_slice(&amount.amount.to_le_bytes());
    }
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(&[0x77; 64]);
    bytes
}

/// Builds a canonical internal message frame.
#[allow(clippy::too_many_arguments)]
fn internal_message(
    source: AccountAddress,
    destination: AccountAddress,
    method_id: u64,
    value_main: u128,
    body: &[u8],
    extra: &[CurrencyAmount],
    callback: Option<([u8; 32], u32)>,
    callback_request: Option<u64>,
    deploy: bool,
) -> Vec<u8> {
    let mut bytes = vec![0; 192];
    let mut flags = 0u8;
    if deploy {
        flags |= 1 << 1;
    }
    if callback.is_some() {
        flags |= 1;
    }
    if callback_request.is_some() {
        flags |= 1 << 2;
    }
    bytes[0] = 0;
    bytes[1] = flags;
    bytes[2..4].copy_from_slice(&(extra.len() as u16).to_le_bytes());
    bytes[4..8].copy_from_slice(&(body.len() as u32).to_le_bytes());
    bytes[8..16].copy_from_slice(&method_id.to_le_bytes());
    bytes[16..32].copy_from_slice(&value_main.to_le_bytes());
    // A success callback must carry a non-zero reserve, otherwise the canonical
    // decoder rejects the frame as a non-canonical value.
    bytes[32..48].copy_from_slice(&1_000u128.to_le_bytes());
    bytes[48..56].copy_from_slice(&9u64.to_le_bytes());
    bytes[56..60].copy_from_slice(&1_800_000_010u32.to_le_bytes());
    bytes[64..96].copy_from_slice(&source.dapp_id);
    bytes[96..128].copy_from_slice(&source.account_id);
    bytes[128..160].copy_from_slice(&destination.dapp_id);
    bytes[160..192].copy_from_slice(&destination.account_id);
    if let Some((cause_msg_hash, status_code)) = callback {
        bytes.extend_from_slice(&cause_msg_hash);
        bytes.extend_from_slice(&destination.dapp_id);
        bytes.extend_from_slice(&destination.account_id);
        bytes.extend_from_slice(&status_code.to_le_bytes());
    }
    if let Some(handler_method_id) = callback_request {
        bytes.extend_from_slice(&handler_method_id.to_le_bytes());
    }
    for amount in extra {
        bytes.extend_from_slice(&amount.currency_id.to_le_bytes());
        bytes.extend_from_slice(&amount.amount.to_le_bytes());
    }
    bytes.extend_from_slice(body);
    bytes
}

/// Assembles a full `ACMI` frame around an already encoded canonical message.
#[allow(clippy::too_many_arguments)]
fn message_input(
    caller_kind: u8,
    src_dapp: [u8; 32],
    src_account: [u8; 32],
    external_pubkey: [u8; 32],
    destination: AccountAddress,
    balance_main: u128,
    extra: &[CurrencyAmount],
    state: &[u8],
    message: &[u8],
) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"ACMI");
    bytes.extend_from_slice(&9u64.to_le_bytes());
    bytes.extend_from_slice(&1_800_000_000u32.to_le_bytes());
    bytes.extend_from_slice(&11u32.to_le_bytes());
    bytes.extend_from_slice(&[0x12; 32]);
    bytes.push(caller_kind);
    bytes.extend_from_slice(&[0; 7]);
    bytes.extend_from_slice(&src_dapp);
    bytes.extend_from_slice(&src_account);
    bytes.extend_from_slice(&external_pubkey);
    bytes.extend_from_slice(&destination.dapp_id);
    bytes.extend_from_slice(&destination.account_id);
    bytes.extend_from_slice(&SELECTED_CODE_ID);
    bytes.push(StateOutputMode::FullReplace as u8);
    bytes.extend_from_slice(&[0; 7]);
    bytes.extend_from_slice(&balance_main.to_le_bytes());
    bytes.extend_from_slice(&(extra.len() as u16).to_le_bytes());
    for amount in extra {
        bytes.extend_from_slice(&amount.currency_id.to_le_bytes());
        bytes.extend_from_slice(&amount.amount.to_le_bytes());
    }
    bytes.extend_from_slice(&(state.len() as u32).to_le_bytes());
    bytes.extend_from_slice(state);
    bytes.extend_from_slice(&(message.len() as u32).to_le_bytes());
    bytes.extend_from_slice(message);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes
}

/// Builds the frame for an external call signed by `pubkey`.
#[allow(clippy::too_many_arguments)]
fn external_input(
    pubkey: [u8; 32],
    method_id: u64,
    value_main: u128,
    body: &[u8],
    balance_main: u128,
    account_extra: &[CurrencyAmount],
    message_extra: &[CurrencyAmount],
    state: &[u8],
) -> Vec<u8> {
    let destination = router_address();
    let message =
        external_message(pubkey, destination, method_id, value_main, body, message_extra, false);
    message_input(
        1,
        [0; 32],
        [0; 32],
        pubkey,
        destination,
        balance_main,
        account_extra,
        state,
        &message,
    )
}

/// Builds the frame for an internal delivery from `source`.
fn internal_input(
    source: AccountAddress,
    method_id: u64,
    value_main: u128,
    body: &[u8],
    callback: Option<([u8; 32], u32)>,
    callback_request: Option<u64>,
    state: &[u8],
) -> Vec<u8> {
    let destination = router_address();
    let message = internal_message(
        source,
        destination,
        method_id,
        value_main,
        body,
        &[],
        callback,
        callback_request,
        false,
    );
    message_input(
        0,
        source.dapp_id,
        source.account_id,
        [0; 32],
        destination,
        0,
        &[],
        state,
        &message,
    )
}

/// Runs the contract on a frame and asserts the canonical output encodes.
fn run(bytes: &[u8]) -> MessageOutput {
    let input = decode_message_input(bytes).expect("canonical ACMI must decode");
    let output = SettlementRouter::process_message(input);
    if output.status_code == 0 {
        let encoded = encode_message_output(&output).expect("canonical ACMO must encode");
        assert_eq!(&encoded[..4], b"ACMO");
        assert_eq!(u32::from_le_bytes(encoded[4..8].try_into().unwrap()), 0);
    }
    output
}

fn state_of(output: &MessageOutput) -> Vec<u8> {
    match output.state_update.as_ref().expect("success carries a state update") {
        avm_contract_sdk::StateUpdate::FullReplace(bytes) => bytes.clone(),
        avm_contract_sdk::StateUpdate::DataPatchList(_) => panic!("router never patches"),
    }
}

#[test]
fn init_installs_the_declared_state_image() {
    let destination = router_address();
    let message = external_message(
        router::owner_pubkey(),
        destination,
        0,
        0,
        &[],
        &[],
        true,
    );
    let bytes = message_input(
        1,
        [0; 32],
        [0; 32],
        router::owner_pubkey(),
        destination,
        0,
        &[],
        &[],
        &message,
    );
    let input = decode_message_input(&bytes).expect("canonical deploy ACMI must decode");
    assert!(input.message.is_deploy());

    let output = SettlementRouter::init(input);
    assert_eq!(output.status_code, 0);
    assert!(output.callback_intents.is_empty());
    assert!(output.callback_result_body.is_empty());
    assert!(output.code_upgrade_intent.is_none());
    assert_eq!(state_of(&output), initial_state_bytes());
    assert_eq!(state_of(&output).len(), STATE_BYTES);

    let decoded = RouterState::from_bytes(&state_of(&output)).expect("initial image decodes");
    assert_eq!(decoded.routed_count, 0);
    assert_eq!(decoded.completed_count, 0);
    assert_eq!(decoded.failed_count, 0);
    assert_eq!(decoded.owner_pubkey, router::owner_pubkey());
    assert!(decoded.routes.is_empty());
}

#[test]
fn settle_forwards_the_full_value_and_requests_a_callback() {
    let bytes = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &seeded_state(),
    );
    let output = run(&bytes);

    assert_eq!(output.status_code, 0);
    assert_eq!(output.outbound_intents.len(), 1);
    let intent = &output.outbound_intents[0];
    assert_eq!(intent.destination, beneficiary_address());
    // The router is a pass-through: nothing is retained from a settlement.
    assert_eq!(intent.value_main, 500);
    assert_eq!(intent.input_index, 0);
    assert_eq!(intent.body, ROUTE_KEY);
    assert!(intent.extra.is_empty());
    assert!(
        intent.fuel_fee_shells > 0,
        "a callback is chargeable, so the delivery must reserve FUEL for it"
    );

    assert_eq!(output.callback_intents.len(), 1);
    assert_eq!(output.callback_intents[0].outbound_index, 0);
    assert_eq!(
        output.callback_intents[0].handler_method_id,
        avm_settlement_router::wire::CALLBACK_METHOD_ID
    );

    assert_eq!(output.event_intents.len(), 1);
    assert_eq!(output.event_intents[0].kind, EventKind::External);
    assert_eq!(output.event_intents[0].event_name, "settlement.router.routed");
    assert_eq!(output.event_intents[0].topics, vec![ROUTE_KEY.to_vec()]);

    let state = RouterState::from_bytes(&state_of(&output)).expect("image decodes");
    assert_eq!(state.routed_count, 1);
    assert_eq!(state.last_route_key, ROUTE_KEY);
    assert_eq!(state.last_status, AWAITING_CALLBACK, "settlement must be in flight");
    assert_eq!(state.completed_count, 0);
}

#[test]
fn settle_rejects_an_unknown_route_and_a_beneficiary_mismatch() {
    let unknown = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &[*b"nope-001", &BENEFICIARY_ACCOUNT[..], &DAPP_ID[..]].concat(),
        0,
        &[],
        &[],
        &seeded_state(),
    );
    assert_eq!(run(&unknown).status_code, status::UNKNOWN_ROUTE);

    // Route key is valid but the stated beneficiary is a different account.
    let mut mismatched = settle_body();
    mismatched[40..72].copy_from_slice(&STRANGER_PUBKEY);
    let bytes = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &mismatched,
        0,
        &[],
        &[],
        &seeded_state(),
    );
    assert_eq!(run(&bytes).status_code, status::INVALID_SETTLEMENT);
}

#[test]
fn settle_rejects_value_below_the_route_minimum() {
    let bytes = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        99,
        &settle_body(),
        0,
        &[],
        &[],
        &seeded_state(),
    );
    assert_eq!(run(&bytes).status_code, status::INVALID_SETTLEMENT);
}

#[test]
fn settle_rejects_an_internal_caller_and_transferred_extra_currency() {
    let source = AccountAddress { dapp_id: OTHER_DAPP_ID, account_id: STRANGER_PUBKEY };
    let bytes = internal_input(
        source,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        None,
        None,
        &seeded_state(),
    );
    assert_eq!(run(&bytes).status_code, status::UNAUTHORIZED);

    let with_extra = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[CurrencyAmount { currency_id: SHELL, amount: 5 }],
        &seeded_state(),
    );
    assert_eq!(run(&with_extra).status_code, status::INVALID_SETTLEMENT);
}

#[test]
fn settle_rejects_a_malformed_body_and_a_self_beneficiary() {
    let short = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &ROUTE_KEY,
        0,
        &[],
        &[],
        &seeded_state(),
    );
    assert_eq!(run(&short).status_code, status::INVALID_SETTLEMENT);

    let mut self_pay = settle_body();
    self_pay[8..40].copy_from_slice(&DAPP_ID);
    self_pay[40..72].copy_from_slice(&ROUTER_ACCOUNT);
    let bytes = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &self_pay,
        0,
        &[],
        &[],
        &seeded_state(),
    );
    assert_eq!(run(&bytes).status_code, status::INVALID_SETTLEMENT);
}

#[test]
fn callback_records_a_completed_settlement_and_rejects_a_replay() {
    let routed = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &seeded_state(),
    );
    let pending = state_of(&run(&routed));

    // A delivered settlement reports a zero status, which the canonical decoder
    // only accepts with an empty body.
    let delivered = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &[],
        Some(([0xab; 32], 0)),
        None,
        &pending,
    );
    let output = run(&delivered);
    assert_eq!(output.status_code, 0);
    // No callback was requested on this delivery, so no result body is allowed.
    assert!(output.callback_result_body.is_empty());
    assert!(output.outbound_intents.is_empty());
    assert!(output.code_upgrade_intent.is_none());
    assert_eq!(output.event_intents.len(), 1);
    assert_eq!(output.event_intents[0].kind, EventKind::Internal);
    assert_eq!(output.event_intents[0].event_name, "settlement.router.settled");
    assert_eq!(output.event_intents[0].topics, vec![ROUTE_KEY.to_vec()]);

    let settled = state_of(&output);
    let state = RouterState::from_bytes(&settled).expect("image decodes");
    assert_eq!(state.routed_count, 1);
    assert_eq!(state.completed_count, 1);
    assert_eq!(state.failed_count, 0);
    assert_eq!(state.last_status, 0);

    // The in-flight sentinel is cleared, so the same callback cannot run twice.
    let replay = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &[],
        Some(([0xab; 32], 0)),
        None,
        &settled,
    );
    assert_eq!(run(&replay).status_code, status::INVALID_CALLBACK);

    // A delivered callback may not smuggle a body.
    let with_body = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &ROUTE_KEY,
        Some(([0xab; 32], 0)),
        None,
        &pending,
    );
    assert_eq!(run(&with_body).status_code, status::INVALID_CALLBACK);
}

#[test]
fn callback_records_a_bounced_settlement_and_blocks_a_replay() {
    let routed = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &seeded_state(),
    );
    let pending = state_of(&run(&routed));

    // A bounced settlement carries a non-zero status and must report the route
    // key of the pending settlement back in its body.
    let bounced = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &ROUTE_KEY,
        Some(([0xcd; 32], 1600)),
        None,
        &pending,
    );
    let output = run(&bounced);
    assert_eq!(output.status_code, 0);
    let recorded = RouterState::from_bytes(&state_of(&output)).expect("image decodes");
    assert_eq!(recorded.completed_count, 0);
    assert_eq!(recorded.failed_count, 1);
    assert_eq!(recorded.last_status, 1600);

    let replay = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &ROUTE_KEY,
        Some(([0xcd; 32], 1600)),
        None,
        &state_of(&output),
    );
    assert_eq!(run(&replay).status_code, status::INVALID_CALLBACK);

    // A bounce for another route is not attributable to this settlement.
    let wrong_key = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        b"route-99",
        Some(([0xcd; 32], 1600)),
        None,
        &pending,
    );
    assert_eq!(run(&wrong_key).status_code, status::INVALID_CALLBACK);

    // A bounce with an empty body cannot be attributed at all.
    let no_key = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &[],
        Some(([0xcd; 32], 1600)),
        None,
        &pending,
    );
    assert_eq!(run(&no_key).status_code, status::INVALID_CALLBACK);
}

#[test]
fn only_one_settlement_may_be_in_flight() {
    let routed = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &seeded_state(),
    );
    let pending = state_of(&run(&routed));

    // Routing again before the callback arrives would overwrite the pending
    // route key, so it is refused.
    let second = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &pending,
    );
    assert_eq!(run(&second).status_code, status::SETTLEMENT_PENDING);

    // A callback with nothing in flight is refused too.
    let premature = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &[],
        Some(([0xab; 32], 0)),
        None,
        &seeded_state(),
    );
    assert_eq!(run(&premature).status_code, status::INVALID_CALLBACK);

    // Once the callback lands, the next settlement routes normally.
    let delivered = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &[],
        Some(([0xab; 32], 0)),
        None,
        &pending,
    );
    let settled = state_of(&run(&delivered));
    let third = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        700,
        &settle_body(),
        0,
        &[],
        &[],
        &settled,
    );
    let output = run(&third);
    assert_eq!(output.status_code, 0);
    assert_eq!(output.outbound_intents[0].value_main, 700);
    let state = RouterState::from_bytes(&state_of(&output)).expect("image decodes");
    assert_eq!(state.routed_count, 2);
    assert_eq!(state.completed_count, 1);
    assert_eq!(state.last_status, AWAITING_CALLBACK);
}

#[test]
fn callback_needs_an_internal_caller_and_no_transferred_value() {
    let routed = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &seeded_state(),
    );
    let pending = state_of(&run(&routed));

    let external = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        0,
        &[],
        0,
        &[],
        &[],
        &pending,
    );
    assert_eq!(run(&external).status_code, status::INVALID_CALLBACK);

    let value_carrying = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::CALLBACK_METHOD_ID,
        7,
        &[],
        Some(([0xab; 32], 0)),
        None,
        &pending,
    );
    assert_eq!(run(&value_carrying).status_code, status::INVALID_CALLBACK);
}

#[test]
fn owner_can_install_update_and_clear_routes() {
    let owner = router::owner_pubkey();
    let empty = initial_state_bytes();

    let install = external_input(
        owner,
        avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
        0,
        &route_set_body(ROUTE_KEY, beneficiary_address(), 100),
        0,
        &[],
        &[],
        &empty,
    );
    let output = run(&install);
    assert_eq!(output.status_code, 0);
    assert!(output.outbound_intents.is_empty());
    assert_eq!(output.event_intents[0].event_name, "settlement.router.route_changed");
    let after_install = state_of(&output);
    let state = RouterState::from_bytes(&after_install).expect("image decodes");
    assert_eq!(state.routes.len(), 1);
    assert_eq!(state.routes[0].min_value, 100);

    // Installing the same key again updates the entry instead of appending.
    let update = external_input(
        owner,
        avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
        0,
        &route_set_body(ROUTE_KEY, beneficiary_address(), 250),
        0,
        &[],
        &[],
        &after_install,
    );
    let updated = RouterState::from_bytes(&state_of(&run(&update))).expect("image decodes");
    assert_eq!(updated.routes.len(), 1);
    assert_eq!(updated.routes[0].min_value, 250);

    let clear = external_input(
        owner,
        avm_settlement_router::wire::ROUTE_CLEAR_METHOD_ID,
        0,
        &ROUTE_KEY,
        0,
        &[],
        &[],
        &updated.to_bytes(),
    );
    let cleared = run(&clear);
    assert_eq!(cleared.status_code, 0);
    let state = RouterState::from_bytes(&state_of(&cleared)).expect("image decodes");
    assert!(state.routes.is_empty());
    assert_eq!(state_of(&cleared).len(), STATE_BYTES);

    // Clearing an absent route is an unknown-route error, not a silent success.
    let again = external_input(
        owner,
        avm_settlement_router::wire::ROUTE_CLEAR_METHOD_ID,
        0,
        &ROUTE_KEY,
        0,
        &[],
        &[],
        &state.to_bytes(),
    );
    assert_eq!(run(&again).status_code, status::UNKNOWN_ROUTE);
}

#[test]
fn route_administration_rejects_a_stranger_and_a_self_destination() {
    let install = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
        0,
        &route_set_body(ROUTE_KEY, beneficiary_address(), 100),
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&install).status_code, status::UNAUTHORIZED);

    let owner = router::owner_pubkey();
    let self_route = external_input(
        owner,
        avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
        0,
        &route_set_body(ROUTE_KEY, router_address(), 100),
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&self_route).status_code, status::INVALID_ROUTE_ARGS);

    let zero_key = external_input(
        owner,
        avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
        0,
        &route_set_body([0; 8], beneficiary_address(), 100),
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&zero_key).status_code, status::INVALID_ROUTE_ARGS);
}

#[test]
fn route_table_is_bounded_by_max_routes() {
    let owner = router::owner_pubkey();
    let mut state = initial_state_bytes();
    for slot in 0..avm_settlement_router::state::MAX_ROUTES as u8 {
        let address = AccountAddress { dapp_id: DAPP_ID, account_id: [slot + 1; 32] };
        let bytes = external_input(
            owner,
            avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
            0,
            &route_set_body([slot + 1; 8], address, 1),
            0,
            &[],
            &[],
            &state,
        );
        let output = run(&bytes);
        assert_eq!(output.status_code, 0, "slot {slot} must install");
        state = state_of(&output);
    }
    assert_eq!(RouterState::from_bytes(&state).expect("image decodes").routes.len(), 4);

    let overflow = external_input(
        owner,
        avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
        0,
        &route_set_body([9; 8], AccountAddress { dapp_id: DAPP_ID, account_id: [9; 32] }, 1),
        0,
        &[],
        &[],
        &state,
    );
    assert_eq!(run(&overflow).status_code, status::INVALID_ROUTE_ARGS);
}

#[test]
fn withdraw_drains_value_and_extra_currency_to_the_owner() {
    let owner = router::owner_pubkey();
    let held = [CurrencyAmount { currency_id: SHELL, amount: 40 }];
    let bytes = external_input(
        owner,
        avm_settlement_router::wire::WITHDRAW_METHOD_ID,
        0,
        &[],
        900,
        &held,
        &[],
        &initial_state_bytes(),
    );
    let output = run(&bytes);
    assert_eq!(output.status_code, 0);
    assert_eq!(output.outbound_intents.len(), 1);
    let intent = &output.outbound_intents[0];
    assert_eq!(intent.destination.dapp_id, DAPP_ID);
    assert_eq!(intent.destination.account_id, owner);
    assert_eq!(intent.value_main, 900);
    assert_eq!(intent.extra, held.to_vec());
    assert_eq!(intent.method_id, avm_settlement_router::wire::PAYOUT_METHOD_ID);
    assert!(intent.body.is_empty());
    assert_eq!(output.event_intents[0].event_name, "settlement.router.withdrawn");
    // The state image is unchanged apart from being rewritten.
    assert_eq!(state_of(&output), initial_state_bytes());
}

#[test]
fn withdraw_rejects_a_stranger_a_body_and_an_empty_balance() {
    let owner = router::owner_pubkey();
    let stranger = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::WITHDRAW_METHOD_ID,
        0,
        &[],
        900,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&stranger).status_code, status::UNAUTHORIZED);

    let with_body = external_input(
        owner,
        avm_settlement_router::wire::WITHDRAW_METHOD_ID,
        0,
        &[1, 2, 3],
        900,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&with_body).status_code, status::UNAUTHORIZED);

    let empty = external_input(
        owner,
        avm_settlement_router::wire::WITHDRAW_METHOD_ID,
        0,
        &[],
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&empty).status_code, status::NOTHING_TO_WITHDRAW);
}

#[test]
fn version_probe_answers_only_an_external_empty_call() {
    let probe = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::VERSION_METHOD_ID,
        0,
        &[],
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    let output = run(&probe);
    assert_eq!(output.status_code, 0);
    assert_eq!(state_of(&output), initial_state_bytes());

    let with_body = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::VERSION_METHOD_ID,
        0,
        &[0],
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&with_body).status_code, status::INVALID_VERSION_PROBE);

    let with_value = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::VERSION_METHOD_ID,
        5,
        &[],
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&with_value).status_code, status::INVALID_VERSION_PROBE);

    let internal = internal_input(
        beneficiary_address(),
        avm_settlement_router::wire::VERSION_METHOD_ID,
        0,
        &[],
        None,
        None,
        &initial_state_bytes(),
    );
    assert_eq!(run(&internal).status_code, status::INVALID_VERSION_PROBE);
}

#[test]
fn code_upgrade_requires_the_owner_and_emits_an_intent() {
    let owner = router::owner_pubkey();
    let args = UpgradeArgs {
        target_selected_code_id: [0x99; 32],
        expected_code_hash: [0x88; 32],
        migration_payload: [0x5a; 24],
    };
    let body = encode_upgrade_body(&args);
    assert_eq!(decode_upgrade_args(&body), Some(args));

    let bytes = external_input(
        owner,
        CODE_UPGRADE_METHOD_ID,
        0,
        &body,
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    let output = run(&bytes);
    assert_eq!(output.status_code, 0);
    let intent = output.code_upgrade_intent.as_ref().expect("upgrade intent");
    assert_eq!(intent.target_selected_code_id, args.target_selected_code_id);
    assert_eq!(intent.expected_code_hash, args.expected_code_hash);
    assert!(output.outbound_intents.is_empty());

    let migrated = state_of(&output);
    assert_eq!(migrated.len(), STATE_BYTES);
    let state = RouterState::from_bytes(&migrated).expect("migrated image still decodes");
    assert_eq!(state.version, VERSION_V2);
    assert_eq!(&migrated[UPGRADE_MARKER_OFFSET..UPGRADE_MARKER_OFFSET + 4], &MIGRATION_MARKER);
    assert_eq!(&state.migration_payload[4..], &[0x5a; 20]);

    // A second upgrade from the migrated image is refused.
    let again = external_input(owner, CODE_UPGRADE_METHOD_ID, 0, &body, 0, &[], &[], &migrated);
    assert_eq!(run(&again).status_code, status::INVALID_CODE_UPGRADE);

    let stranger = external_input(
        STRANGER_PUBKEY,
        CODE_UPGRADE_METHOD_ID,
        0,
        &body,
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&stranger).status_code, status::INVALID_CODE_UPGRADE);
}

#[test]
fn code_upgrade_rejects_the_current_code_id_and_zero_hashes() {
    let owner = router::owner_pubkey();
    let same_code = UpgradeArgs {
        target_selected_code_id: SELECTED_CODE_ID,
        expected_code_hash: [0x88; 32],
        migration_payload: [0x5a; 24],
    };
    let bytes = external_input(
        owner,
        CODE_UPGRADE_METHOD_ID,
        0,
        &encode_upgrade_body(&same_code),
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&bytes).status_code, status::INVALID_CODE_UPGRADE);

    let zero_hash = UpgradeArgs {
        target_selected_code_id: [0x99; 32],
        expected_code_hash: [0; 32],
        migration_payload: [0x5a; 24],
    };
    let bytes = external_input(
        owner,
        CODE_UPGRADE_METHOD_ID,
        0,
        &encode_upgrade_body(&zero_hash),
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&bytes).status_code, status::INVALID_CODE_UPGRADE);
}

#[test]
fn an_unknown_method_and_a_corrupt_state_fail_closed() {
    let unknown = external_input(
        STRANGER_PUBKEY,
        0xdead_beef_cafe_0001,
        0,
        &[],
        0,
        &[],
        &[],
        &initial_state_bytes(),
    );
    assert_eq!(run(&unknown).status_code, status::INVALID_SETTLEMENT);

    let mut corrupt = seeded_state();
    corrupt[57] = 3;
    let bytes = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &corrupt,
    );
    assert_eq!(run(&bytes).status_code, status::INVALID_STATE);

    let truncated = external_input(
        STRANGER_PUBKEY,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        0,
        &[],
        &[],
        &seeded_state()[..STATE_BYTES - 1],
    );
    assert_eq!(run(&truncated).status_code, status::INVALID_STATE);
}

#[test]
fn failures_carry_no_effects() {
    for bytes in [
        external_input(
            STRANGER_PUBKEY,
            avm_settlement_router::wire::ROUTE_SET_METHOD_ID,
            0,
            &route_set_body(ROUTE_KEY, beneficiary_address(), 1),
            0,
            &[],
            &[],
            &initial_state_bytes(),
        ),
        external_input(
            STRANGER_PUBKEY,
            avm_settlement_router::wire::WITHDRAW_METHOD_ID,
            0,
            &[],
            10,
            &[],
            &[],
            &initial_state_bytes(),
        ),
    ] {
        let input = decode_message_input(&bytes).expect("frame decodes");
        assert!(matches!(input.caller, Caller::External(_)));
        let output = SettlementRouter::process_message(input);
        assert_ne!(output.status_code, 0);
        assert!(output.state_update.is_none());
        assert!(output.outbound_intents.is_empty());
        assert!(output.callback_intents.is_empty());
        assert!(output.event_intents.is_empty());
        assert!(output.callback_result_body.is_empty());
        assert!(output.code_upgrade_intent.is_none());
        // The SDK encoder must accept the failure shape as canonical.
        let encoded = encode_message_output(&output).expect("failure encodes");
        assert_eq!(&encoded[..4], b"ACMO");
    }
}

#[test]
fn a_non_full_replace_descriptor_requirement_is_refused() {
    let destination = router_address();
    let message = external_message(
        STRANGER_PUBKEY,
        destination,
        avm_settlement_router::wire::SETTLE_METHOD_ID,
        500,
        &settle_body(),
        &[],
        false,
    );
    let mut bytes = message_input(
        1,
        [0; 32],
        [0; 32],
        STRANGER_PUBKEY,
        destination,
        0,
        &[],
        &seeded_state(),
        &message,
    );
    // state_output_mode sits after the 32-byte selected_code_id.
    let mode_offset = 4 + 8 + 4 + 4 + 32 + 1 + 7 + 32 + 32 + 32 + 32 + 32 + 32;
    bytes[mode_offset] = StateOutputMode::DataPatchList as u8;
    let input = decode_message_input(&bytes).expect("frame decodes");
    assert_eq!(input.required_state_output_mode, StateOutputMode::DataPatchList);
    let output = SettlementRouter::process_message(input);
    assert_eq!(output.status_code, status::INVALID_STATE);
}
