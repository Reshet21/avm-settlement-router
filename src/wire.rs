//! Canonical wire identifiers and fixed-size body codecs.
//!
//! Every identifier in this module is frozen test data. The public identities,
//! route keys, amounts, and event classes are deterministic fixtures chosen so
//! the contract behaves identically on every replay, which is what the AVM
//! stand's `replay` command and the SDK release gate compare.
//!
//! # Method identifiers
//!
//! AVM dispatches on the `u64` `method_id` carried in the message header, and
//! the platform derives it from the method name:
//!
//! ```text
//! method_id = hash_u64_le(name) = u64::from_le_bytes(blake3(name)[..8])
//! ```
//!
//! The same rule derives `event_class` from the event name, and it produces the
//! reserved upgrade id `0xc2c6c51bc610249d` from `"avm.code_upgrade"`. Every
//! constant below is that hash of its documented name — verified against the
//! SDK's own fixtures (`causal.a.version`, `causal.a.forwarded`,
//! `causal.a.state`) and re-checked on every CI run by
//! `scripts/check_consistency.py`, so a name and its number cannot drift apart
//! unnoticed. `docs/method-ids.md` has the full derivation and evidence.

use crate::state::MIGRATION_PAYLOAD_LEN;

/// Byte length of a settlement route key.
pub const ROUTE_KEY_BYTES: usize = 8;

/// `settlement.router.settle` body: `route_key[8] | beneficiary_dapp[32] |
/// beneficiary_account[32]`.
pub const SETTLE_ARGS_BYTES: usize = ROUTE_KEY_BYTES + 64;

/// `settlement.router.route.set` body: `route_key[8] | dest_dapp[32] |
/// dest_account[32] | min_value[16]`.
pub const ROUTE_SET_ARGS_BYTES: usize = ROUTE_KEY_BYTES + 64 + 16;

/// `settlement.router.route.clear` body: `route_key[8]`.
pub const ROUTE_CLEAR_ARGS_BYTES: usize = ROUTE_KEY_BYTES;

/// `settlement.router.completed` callback body: `route_key[8]`.
pub const CALLBACK_BODY_BYTES: usize = ROUTE_KEY_BYTES;

/// Reserved code-upgrade body: `"ACUG" | target_selected_code_id[32] |
/// expected_code_hash[32] | payload_len[4] | migration_payload[24]`.
pub const CODE_UPGRADE_HEADER_BYTES: usize = 72;

/// Total reserved code-upgrade body length.
pub const CODE_UPGRADE_BODY_BYTES: usize = CODE_UPGRADE_HEADER_BYTES + MIGRATION_PAYLOAD_LEN;

/// Magic prefix of a reserved code-upgrade body.
pub const CODE_UPGRADE_MAGIC: [u8; 4] = *b"ACUG";

/// `hash_u64_le("settlement.router.settle")` — forward value to a routed
/// beneficiary.
pub const SETTLE_METHOD_ID: u64 = 0x82d7_ea8f_0e15_a928;

/// `hash_u64_le("settlement.router.completed")` — callback handler for a
/// routed settlement.
pub const CALLBACK_METHOD_ID: u64 = 0x7ce9_d7f5_15c0_5256;

/// `hash_u64_le("settlement.router.route.set")` — install or update one route
/// (owner only).
pub const ROUTE_SET_METHOD_ID: u64 = 0xae48_8411_c32f_cee2;

/// `hash_u64_le("settlement.router.route.clear")` — remove one route (owner
/// only).
pub const ROUTE_CLEAR_METHOD_ID: u64 = 0xb0a9_ef6d_b4c2_017c;

/// `hash_u64_le("settlement.router.withdraw")` — drain retained value to the
/// owner.
pub const WITHDRAW_METHOD_ID: u64 = 0x3ad0_2292_939e_ff9c;

/// `hash_u64_le("settlement.router.version")` — external empty-body version
/// probe.
pub const VERSION_METHOD_ID: u64 = 0x8fab_f502_3447_277c;

/// `hash_u64_le("settlement.beneficiary.receive")` — method the router calls on
/// the routed beneficiary. It belongs to the destination contract's ABI, so it
/// is derived here from its canonical name rather than declared in
/// `avm-contract.toml`.
pub const SETTLEMENT_DELIVERY_METHOD_ID: u64 = 0x0ef6_cc14_d234_ec6f;

/// `hash_u64_le("settlement.owner.payout")` — method the router calls when
/// draining retained value to the owner account.
pub const PAYOUT_METHOD_ID: u64 = 0xf507_acdc_f6f5_c790;

/// `hash_u64_le("settlement.router.routed")`.
pub const ROUTED_EVENT_CLASS: u64 = 0xf45f_52c6_ebbc_5b2e;

/// `hash_u64_le("settlement.router.settled")`.
pub const SETTLED_EVENT_CLASS: u64 = 0x182e_4064_462d_e927;

/// `hash_u64_le("settlement.router.route_changed")`.
pub const ROUTE_EVENT_CLASS: u64 = 0x0b07_6b3d_59a4_889c;

/// `hash_u64_le("settlement.router.withdrawn")`.
pub const WITHDRAWN_EVENT_CLASS: u64 = 0xf322_0038_13d7_882c;

/// Admin key authorized to change the routing table — the fallback owner.
///
/// The value is the stand's `package-owner` identity public key, the only
/// stand identity key that is publicly known and exercised by a release-gated
/// fixture (`examples/contract-a-v1` uses it as `PACKAGE_OWNER_PUBKEY` for its
/// code-upgrade path). Owner methods therefore run on the stand with
/// `--identity package-owner`.
///
/// `init` may override it: a deploy whose message body is exactly 32 bytes
/// installs those bytes as the owner key instead of this constant. The stand's
/// `init` command reports the generated identity public keys, so a deployment
/// can pin the owner explicitly and does not depend on this fallback.
pub const OWNER_PUBKEY: [u8; 32] = [
    0x81, 0x39, 0x77, 0x0e, 0xa8, 0x7d, 0x17, 0x5f, 0x56, 0xa3, 0x54, 0x66, 0xc3, 0x4c, 0x7e, 0xcc,
    0xcb, 0x8d, 0x8a, 0x91, 0xb4, 0xee, 0x37, 0xa2, 0x5d, 0xf6, 0x0f, 0x5b, 0x8f, 0xc9, 0xb3, 0x94,
];

/// Ordinary SHELL carried as extra currency.
///
/// AVM stores transferable SHELL as `currency_id = 2`. It is value the
/// destination receives, and it is distinct from `fuel_fee_shells`, which is a
/// compute reserve the destination does not receive.
pub const SHELL_CURRENCY_ID: u32 = 2;

/// Reserve proposed on cross-DApp outbound intents.
///
/// A same-DApp local-free route commits `fuel_fee_shells = 0` and never debits
/// the reserve, while a route separated by a DApp split needs one. Proposing a
/// reserve on every outbound intent therefore keeps a single intent valid
/// across a future split without charging the common local path. Callbacks are
/// always chargeable, so a settlement that requests one must reserve for it.
pub const CROSS_DAPP_FUEL_RESERVE: u128 = 1_000;

/// ABI status codes. The SDK reserves 1..1000 for the runtime, so contract
/// codes must be 0 or at least 1000.
pub mod status {
    /// Settlement arguments, route table state, or beneficiary are invalid.
    pub const INVALID_SETTLEMENT: u32 = 1600;
    /// No route matches the requested key.
    pub const UNKNOWN_ROUTE: u32 = 1601;
    /// The retained state image is malformed or exhausted.
    pub const INVALID_STATE: u32 = 1602;
    /// The caller is not authorized for this method.
    pub const UNAUTHORIZED: u32 = 1603;
    /// Callback context or result body is malformed.
    pub const INVALID_CALLBACK: u32 = 1604;
    /// Route table arguments are invalid or the table is full.
    pub const INVALID_ROUTE_ARGS: u32 = 1605;
    /// The version probe requires one external empty-body call.
    pub const INVALID_VERSION_PROBE: u32 = 1606;
    /// Code upgrade authorization or migration payload is invalid.
    pub const INVALID_CODE_UPGRADE: u32 = 1607;
    /// Withdrawal has nothing to drain.
    pub const NOTHING_TO_WITHDRAW: u32 = 1608;
    /// An event field list could not be canonically encoded.
    pub const EVENT_ENCODING: u32 = 1609;
    /// A routed settlement is still awaiting its callback.
    pub const SETTLEMENT_PENDING: u32 = 1610;
}

/// Decoded `settlement.router.settle` arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettleArgs {
    /// Routing table key selecting the beneficiary partition.
    pub route_key: [u8; ROUTE_KEY_BYTES],
    /// DApp partition the caller wants the settlement delivered to.
    pub beneficiary_dapp: [u8; 32],
    /// Account the caller wants the settlement delivered to.
    pub beneficiary_account: [u8; 32],
}

/// Decodes settlement arguments, returning `None` on any length or shape error.
pub fn decode_settle_args(body: &[u8]) -> Option<SettleArgs> {
    if body.len() != SETTLE_ARGS_BYTES {
        return None;
    }
    Some(SettleArgs {
        route_key: body.get(..ROUTE_KEY_BYTES)?.try_into().ok()?,
        beneficiary_dapp: body.get(ROUTE_KEY_BYTES..40)?.try_into().ok()?,
        beneficiary_account: body.get(40..SETTLE_ARGS_BYTES)?.try_into().ok()?,
    })
}

/// Decoded `settlement.router.route.set` arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteSetArgs {
    /// Routing table key to install or update.
    pub route_key: [u8; ROUTE_KEY_BYTES],
    /// Beneficiary DApp partition for this route.
    pub dapp_id: [u8; 32],
    /// Beneficiary account for this route.
    pub account_id: [u8; 32],
    /// Minimum `value_main` a settlement for this route must carry.
    pub min_value: u128,
}

/// Decodes route-install arguments.
pub fn decode_route_set_args(body: &[u8]) -> Option<RouteSetArgs> {
    if body.len() != ROUTE_SET_ARGS_BYTES {
        return None;
    }
    let value_offset = ROUTE_KEY_BYTES + 64;
    Some(RouteSetArgs {
        route_key: body.get(..ROUTE_KEY_BYTES)?.try_into().ok()?,
        dapp_id: body.get(ROUTE_KEY_BYTES..40)?.try_into().ok()?,
        account_id: body.get(40..value_offset)?.try_into().ok()?,
        min_value: u128::from_le_bytes(
            body.get(value_offset..ROUTE_SET_ARGS_BYTES)?.try_into().ok()?,
        ),
    })
}

/// Decodes a route key from a clear or callback body.
pub fn decode_route_key(body: &[u8]) -> Option<[u8; ROUTE_KEY_BYTES]> {
    if body.len() != ROUTE_KEY_BYTES {
        return None;
    }
    body.try_into().ok()
}

/// Decoded reserved code-upgrade body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpgradeArgs {
    /// Published code package the account should select.
    pub target_selected_code_id: [u8; 32],
    /// Code hash the runtime must match before switching.
    pub expected_code_hash: [u8; 32],
    /// Deterministic migration payload applied to the state image.
    pub migration_payload: [u8; MIGRATION_PAYLOAD_LEN],
}

/// Decodes a reserved code-upgrade body.
pub fn decode_upgrade_args(body: &[u8]) -> Option<UpgradeArgs> {
    if body.len() != CODE_UPGRADE_BODY_BYTES {
        return None;
    }
    if body.get(..4)? != CODE_UPGRADE_MAGIC {
        return None;
    }
    let len_offset = CODE_UPGRADE_HEADER_BYTES - 4;
    let declared_len =
        u32::from_le_bytes(body.get(len_offset..CODE_UPGRADE_HEADER_BYTES)?.try_into().ok()?);
    if declared_len != u32::try_from(MIGRATION_PAYLOAD_LEN).unwrap_or(u32::MAX) {
        return None;
    }
    Some(UpgradeArgs {
        target_selected_code_id: body.get(4..36)?.try_into().ok()?,
        expected_code_hash: body.get(36..68)?.try_into().ok()?,
        migration_payload: body
            .get(CODE_UPGRADE_HEADER_BYTES..CODE_UPGRADE_BODY_BYTES)?
            .try_into()
            .ok()?,
    })
}

/// Encodes a reserved code-upgrade body.
pub fn encode_upgrade_body(args: &UpgradeArgs) -> Vec<u8> {
    let mut body = Vec::with_capacity(CODE_UPGRADE_BODY_BYTES);
    body.extend_from_slice(&CODE_UPGRADE_MAGIC);
    body.extend_from_slice(&args.target_selected_code_id);
    body.extend_from_slice(&args.expected_code_hash);
    body.extend_from_slice(&u32::try_from(MIGRATION_PAYLOAD_LEN).unwrap_or(u32::MAX).to_le_bytes());
    body.extend_from_slice(&args.migration_payload);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_lengths_match_their_declared_sizes() {
        assert_eq!(SETTLE_ARGS_BYTES, 72);
        assert_eq!(ROUTE_SET_ARGS_BYTES, 88);
        assert_eq!(CODE_UPGRADE_BODY_BYTES, 96);
    }

    #[test]
    fn settle_args_round_trip_and_reject_bad_lengths() {
        let mut body = [0u8; SETTLE_ARGS_BYTES];
        body[..8].copy_from_slice(&[3; 8]);
        body[8..40].copy_from_slice(&[5; 32]);
        body[40..].copy_from_slice(&[6; 32]);
        let args = decode_settle_args(&body).expect("well-formed body");
        assert_eq!(args.route_key, [3; 8]);
        assert_eq!(args.beneficiary_dapp, [5; 32]);
        assert_eq!(args.beneficiary_account, [6; 32]);
        assert_eq!(decode_settle_args(&body[..SETTLE_ARGS_BYTES - 1]), None);
        assert_eq!(decode_settle_args(&[]), None);
    }

    #[test]
    fn route_set_args_round_trip() {
        let mut body = [0u8; ROUTE_SET_ARGS_BYTES];
        body[..8].copy_from_slice(&[1; 8]);
        body[8..40].copy_from_slice(&[2; 32]);
        body[40..72].copy_from_slice(&[3; 32]);
        body[72..].copy_from_slice(&77u128.to_le_bytes());
        let args = decode_route_set_args(&body).expect("well-formed body");
        assert_eq!(args.route_key, [1; 8]);
        assert_eq!(args.dapp_id, [2; 32]);
        assert_eq!(args.account_id, [3; 32]);
        assert_eq!(args.min_value, 77);
        assert_eq!(decode_route_set_args(&body[..72]), None);
    }

    #[test]
    fn upgrade_body_round_trips_and_rejects_bad_magic() {
        let args = UpgradeArgs {
            target_selected_code_id: [12; 32],
            expected_code_hash: [13; 32],
            migration_payload: [0xaa; MIGRATION_PAYLOAD_LEN],
        };
        let body = encode_upgrade_body(&args);
        assert_eq!(body.len(), CODE_UPGRADE_BODY_BYTES);
        assert_eq!(decode_upgrade_args(&body), Some(args));

        let mut bad_magic = body.clone();
        bad_magic[..4].copy_from_slice(b"ACUH");
        assert_eq!(decode_upgrade_args(&bad_magic), None);

        let mut bad_len = body.clone();
        bad_len[68..72].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(decode_upgrade_args(&bad_len), None);

        assert_eq!(decode_upgrade_args(&body[..CODE_UPGRADE_BODY_BYTES - 1]), None);
    }

    #[test]
    fn route_key_decoding_is_length_exact() {
        assert_eq!(decode_route_key(&[4; ROUTE_KEY_BYTES]), Some([4; 8]));
        assert_eq!(decode_route_key(&[4; ROUTE_KEY_BYTES + 1]), None);
        assert_eq!(decode_route_key(&[]), None);
    }

    #[test]
    fn status_codes_stay_out_of_the_reserved_runtime_range() {
        for code in [
            status::INVALID_SETTLEMENT,
            status::UNKNOWN_ROUTE,
            status::INVALID_STATE,
            status::UNAUTHORIZED,
            status::INVALID_CALLBACK,
            status::INVALID_ROUTE_ARGS,
            status::INVALID_VERSION_PROBE,
            status::INVALID_CODE_UPGRADE,
            status::NOTHING_TO_WITHDRAW,
            status::EVENT_ENCODING,
            status::SETTLEMENT_PENDING,
        ] {
            assert!(code == 0 || code >= 1000, "status {code} collides with the runtime range");
        }
    }

    #[test]
    fn identifiers_are_unique() {
        let methods = [
            SETTLE_METHOD_ID,
            CALLBACK_METHOD_ID,
            ROUTE_SET_METHOD_ID,
            ROUTE_CLEAR_METHOD_ID,
            WITHDRAW_METHOD_ID,
            VERSION_METHOD_ID,
            SETTLEMENT_DELIVERY_METHOD_ID,
            PAYOUT_METHOD_ID,
        ];
        for (index, method) in methods.iter().enumerate() {
            assert_ne!(*method, 0);
            assert!(!methods[..index].contains(method), "duplicate method id {method:#x}");
        }
        let classes =
            [ROUTED_EVENT_CLASS, SETTLED_EVENT_CLASS, ROUTE_EVENT_CLASS, WITHDRAWN_EVENT_CLASS];
        for (index, class) in classes.iter().enumerate() {
            assert!(!classes[..index].contains(class), "duplicate event class {class:#x}");
        }
    }
}
