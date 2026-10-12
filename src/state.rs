//! Fixed-size router state image and its canonical byte codec.
//!
//! The settlement router keeps a single fixed-size state image so every
//! successful execution can answer with `StateUpdate::FullReplace` over the
//! same length. That keeps the `state_schema_id` in `avm-contract.toml`
//! honest: the ABI declares `fixed_bytes` with `len = STATE_BYTES`, and the
//! contract never emits an image of any other length.
//!
//! All integers are little-endian, matching the canonical AVM wire codecs.
//!
//! Layout (448 bytes):
//!
//! ```text
//!   0..   8  routed_count       u64    settlements accepted and forwarded
//!   8..  16  completed_count    u64    callbacks observed with status_code 0
//!  16..  24  failed_count       u64    callbacks observed with status_code != 0
//!  24..  56  owner_pubkey       [u8;32] admin key, authorizes route changes
//!  56..  57  version            u8     code version marker
//!  57..  58  route_count        u8     populated route slots
//!  58..  60  reserved           zero
//!  60..  64  last_status        u32    `AWAITING_CALLBACK`, or the status of
//!                                       the most recent callback
//!  64..  72  last_route_key     [u8;8] route key of the most recent settlement
//!  72..  96  migration_payload  [u8;24] retained by a v1 -> v2 code upgrade;
//!                               bytes 72..76 carry `MIGRATION_MARKER`
//!  96.. 448  routes             4 slots x 88 bytes, see [`Route`]
//! ```
//!
//! A route slot is `key[8] | dapp_id[32] | account_id[32] | min_value[16]`, so
//! it holds a full [`AccountAddress`](avm_contract_sdk::AccountAddress) plus a
//! 128-bit minimum. Cross-DApp routing is a first-class case for a settlement
//! router, which is why the slot keeps the beneficiary `dapp_id` rather than
//! inheriting the router's partition. An empty slot is all zeros, and
//! `route_count` must equal the number of non-empty slots.

/// Total length of the canonical state image: a 96-byte header plus
/// `MAX_ROUTES * ROUTE_BYTES`.
pub const STATE_BYTES: usize = 448;

/// Number of route slots carried in the state image.
pub const MAX_ROUTES: usize = 4;

/// Byte length of one route slot: `key[8] | dapp_id[32] | account_id[32] |
/// min_value[16]`.
pub const ROUTE_BYTES: usize = 88;

/// Offset of `dapp_id` inside a route slot.
pub const SLOT_DAPP_OFFSET: usize = 8;

/// Offset of `account_id` inside a route slot.
pub const SLOT_ACCOUNT_OFFSET: usize = SLOT_DAPP_OFFSET + 32;

/// Offset of `min_value` inside a route slot.
pub const SLOT_MIN_VALUE_OFFSET: usize = SLOT_ACCOUNT_OFFSET + 32;

/// The slot layout must add up exactly, or the codec would silently disagree
/// with the ABI's declared `fixed_bytes` length.
const _: () = assert!(SLOT_MIN_VALUE_OFFSET + 16 == ROUTE_BYTES);
const _: () = assert!(ROUTES_OFFSET + MAX_ROUTES * ROUTE_BYTES == STATE_BYTES);

/// Offset where the route table starts.
pub const ROUTES_OFFSET: usize = 96;

/// Offset of the upgrade marker inside the state image.
///
/// The marker is stamped over the first four bytes of `migration_payload`,
/// which starts at offset 72. That region is retained verbatim by
/// [`RouterState::from_bytes`] and is not required to be zero, so a migrated
/// image still decodes. Every other reserved byte in the header is validated as
/// zero, which is why the marker cannot live at `58..60`.
pub const UPGRADE_MARKER_OFFSET: usize = 72;

/// Sentinel `last_status` value meaning a routed settlement has no callback yet.
///
/// AVM contract status codes are `0` or at least `1000`, so `u32::MAX` can never
/// collide with a real callback status. Using a sentinel instead of `0` is what
/// lets the router tell "awaiting a callback" apart from "the last callback
/// succeeded", which in turn makes a settlement one-in-flight and a replayed
/// callback rejectable.
pub const AWAITING_CALLBACK: u32 = u32::MAX;

/// Version marker written by `init` and by the version probe.
pub const VERSION_V1: u8 = 1;

/// Version marker reserved for an upgraded code package.
pub const VERSION_V2: u8 = 2;

/// Marker a v1 -> v2 migration stamps into the reserved header area.
pub const MIGRATION_MARKER: [u8; 4] = *b"SRV2";

/// Length of the migration payload accepted by the code-upgrade entry point.
pub const MIGRATION_PAYLOAD_LEN: usize = 24;

/// One routing table entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Route {
    /// Application-defined settlement route key.
    pub key: [u8; 8],
    /// DApp partition of the settlement beneficiary.
    pub dapp_id: [u8; 32],
    /// Account of the settlement beneficiary.
    pub account_id: [u8; 32],
    /// Minimum `value_main` a settlement for this route must carry.
    pub min_value: u128,
}

impl Route {
    /// Decodes one route slot, returning `None` when the slot is all zeros.
    pub fn from_slot(slot: &[u8]) -> Option<Self> {
        let bytes = slot.get(..ROUTE_BYTES)?;
        if bytes.iter().all(|byte| *byte == 0) {
            return None;
        }
        let dapp_end = SLOT_DAPP_OFFSET + 32;
        let account_end = SLOT_ACCOUNT_OFFSET + 32;
        Some(Self {
            key: bytes.get(..SLOT_DAPP_OFFSET)?.try_into().ok()?,
            dapp_id: bytes.get(SLOT_DAPP_OFFSET..dapp_end)?.try_into().ok()?,
            account_id: bytes.get(SLOT_ACCOUNT_OFFSET..account_end)?.try_into().ok()?,
            min_value: u128::from_le_bytes(
                bytes.get(SLOT_MIN_VALUE_OFFSET..ROUTE_BYTES)?.try_into().ok()?,
            ),
        })
    }

    /// Encodes this route into a fixed-size slot.
    pub fn to_slot(self) -> [u8; ROUTE_BYTES] {
        let mut bytes = [0u8; ROUTE_BYTES];
        let dapp_end = SLOT_DAPP_OFFSET + 32;
        let account_end = SLOT_ACCOUNT_OFFSET + 32;
        bytes[..SLOT_DAPP_OFFSET].copy_from_slice(&self.key);
        bytes[SLOT_DAPP_OFFSET..dapp_end].copy_from_slice(&self.dapp_id);
        bytes[SLOT_ACCOUNT_OFFSET..account_end].copy_from_slice(&self.account_id);
        bytes[SLOT_MIN_VALUE_OFFSET..ROUTE_BYTES].copy_from_slice(&self.min_value.to_le_bytes());
        bytes
    }
}

/// A decoded and structurally validated state image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouterState {
    /// Settlements accepted and forwarded so far.
    pub routed_count: u64,
    /// Callbacks observed with `status_code == 0`.
    pub completed_count: u64,
    /// Callbacks observed with a non-zero `status_code`.
    pub failed_count: u64,
    /// Admin key authorized to change the routing table.
    pub owner_pubkey: [u8; 32],
    /// Code version marker.
    pub version: u8,
    /// [`AWAITING_CALLBACK`] while a settlement is in flight, otherwise the
    /// status code reported by the most recent callback.
    pub last_status: u32,
    /// Route key of the most recent settlement.
    pub last_route_key: [u8; 8],
    /// Migration payload retained from the last code upgrade.
    pub migration_payload: [u8; MIGRATION_PAYLOAD_LEN],
    /// Routing table, exactly `routes.len()` populated slots.
    pub routes: Vec<Route>,
}

impl RouterState {
    /// The empty state image `init` installs.
    pub fn initial(owner_pubkey: [u8; 32]) -> Self {
        Self {
            routed_count: 0,
            completed_count: 0,
            failed_count: 0,
            owner_pubkey,
            version: VERSION_V1,
            last_status: AWAITING_CALLBACK,
            last_route_key: [0; 8],
            migration_payload: [0; MIGRATION_PAYLOAD_LEN],
            routes: Vec::new(),
        }
    }

    /// Looks a route up by key.
    pub fn find_route(&self, key: [u8; 8]) -> Option<&Route> {
        self.routes.iter().find(|route| route.key == key)
    }

    /// Stamps the migration marker into an encoded image.
    ///
    /// The marker overwrites the first four bytes of `migration_payload`, so a
    /// migrated image retains `MIGRATION_MARKER` followed by payload bytes
    /// `4..24`. Call [`Self::to_bytes`] first: this mutates an already encoded
    /// image rather than the struct, which keeps the codec and the migration
    /// step independently checkable.
    pub fn apply_upgrade_marker(&self, bytes: &mut [u8]) {
        let end = UPGRADE_MARKER_OFFSET + MIGRATION_MARKER.len();
        if let Some(marker) = bytes.get_mut(UPGRADE_MARKER_OFFSET..end) {
            marker.copy_from_slice(&MIGRATION_MARKER);
        }
    }

    /// Encodes the state image. The result is always [`STATE_BYTES`] long.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(STATE_BYTES);
        bytes.extend_from_slice(&self.routed_count.to_le_bytes());
        bytes.extend_from_slice(&self.completed_count.to_le_bytes());
        bytes.extend_from_slice(&self.failed_count.to_le_bytes());
        bytes.extend_from_slice(&self.owner_pubkey);
        bytes.push(self.version);
        // `routes` is bounded by `MAX_ROUTES`, so this cannot truncate. The
        // fallback is 0 rather than a plausible count, because a silent
        // overcount would make the image decode as malformed instead of
        // misreporting populated slots.
        bytes.push(u8::try_from(self.routes.len()).unwrap_or(0));
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&self.last_status.to_le_bytes());
        bytes.extend_from_slice(&self.last_route_key);
        bytes.extend_from_slice(&self.migration_payload);
        for route in &self.routes {
            bytes.extend_from_slice(&route.to_slot());
        }
        for _ in self.routes.len()..MAX_ROUTES {
            bytes.extend_from_slice(&[0; ROUTE_BYTES]);
        }
        debug_assert_eq!(bytes.len(), STATE_BYTES);
        bytes
    }

    /// Decodes and structurally validates a state image.
    ///
    /// Returns `None` when the image is malformed, when `route_count` does not
    /// match the populated slots, when a slot carries a zero address, or when
    /// the version marker is not one this code understands.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != STATE_BYTES {
            return None;
        }
        let version = bytes[56];
        if version != VERSION_V1 && version != VERSION_V2 {
            return None;
        }
        let owner_pubkey: [u8; 32] = bytes.get(24..56)?.try_into().ok()?;
        if owner_pubkey == [0; 32] {
            return None;
        }
        let declared = usize::from(bytes[57]);
        if declared > MAX_ROUTES {
            return None;
        }
        if bytes.get(58..60)?.iter().any(|byte| *byte != 0) {
            return None;
        }

        let mut routes = Vec::with_capacity(MAX_ROUTES);
        for slot in 0..MAX_ROUTES {
            let start = ROUTES_OFFSET + slot * ROUTE_BYTES;
            let Some(route) = Route::from_slot(bytes.get(start..start + ROUTE_BYTES)?) else {
                continue;
            };
            if route.key == [0; 8] || route.dapp_id == [0; 32] || route.account_id == [0; 32] {
                return None;
            }
            if routes.iter().any(|existing: &Route| existing.key == route.key) {
                return None;
            }
            routes.push(route);
        }
        if routes.len() != declared {
            return None;
        }

        Some(Self {
            routed_count: u64::from_le_bytes(bytes.get(..8)?.try_into().ok()?),
            completed_count: u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?),
            failed_count: u64::from_le_bytes(bytes.get(16..24)?.try_into().ok()?),
            owner_pubkey,
            version,
            last_status: u32::from_le_bytes(bytes.get(60..64)?.try_into().ok()?),
            last_route_key: bytes.get(64..72)?.try_into().ok()?,
            migration_payload: bytes.get(72..96)?.try_into().ok()?,
            routes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(seed: u8) -> Route {
        Route {
            key: [seed; 8],
            dapp_id: [seed + 1; 32],
            account_id: [seed + 2; 32],
            min_value: u128::from(seed) * 1000,
        }
    }

    #[test]
    fn initial_state_round_trips_at_the_declared_length() {
        let state = RouterState::initial([7; 32]);
        let bytes = state.to_bytes();
        assert_eq!(bytes.len(), STATE_BYTES);
        assert_eq!(RouterState::from_bytes(&bytes), Some(state));
    }

    #[test]
    fn route_slots_round_trip_and_keep_the_image_length_fixed() {
        let mut state = RouterState::initial([9; 32]);
        for seed in 1..=u8::try_from(MAX_ROUTES).expect("MAX_ROUTES fits u8") {
            state.routes.push(route(seed));
        }
        let bytes = state.to_bytes();
        assert_eq!(bytes.len(), STATE_BYTES);
        assert_eq!(RouterState::from_bytes(&bytes), Some(state));
        assert_eq!(state.find_route([3; 8]), Some(&route(3)));
    }

    #[test]
    fn malformed_images_are_rejected() {
        let state = RouterState::initial([1; 32]);
        assert_eq!(RouterState::from_bytes(&[]), None);
        assert_eq!(RouterState::from_bytes(&state.to_bytes()[..STATE_BYTES - 1]), None);

        let mut version = state.to_bytes();
        version[56] = 3;
        assert_eq!(RouterState::from_bytes(&version), None);

        let mut no_owner = state.to_bytes();
        no_owner[24..56].copy_from_slice(&[0; 32]);
        assert_eq!(RouterState::from_bytes(&no_owner), None);

        let mut declared_mismatch = state.to_bytes();
        declared_mismatch[57] = 1;
        assert_eq!(RouterState::from_bytes(&declared_mismatch), None);

        let mut reserved_dirty = state.to_bytes();
        reserved_dirty[59] = 1;
        assert_eq!(RouterState::from_bytes(&reserved_dirty), None);

        let mut zero_address_route = state.to_bytes();
        zero_address_route[57] = 1;
        zero_address_route[ROUTES_OFFSET..ROUTES_OFFSET + 8].copy_from_slice(&[5; 8]);
        assert_eq!(RouterState::from_bytes(&zero_address_route), None);

        let mut duplicate_route = state.to_bytes();
        duplicate_route[57] = 2;
        for slot in 0..2 {
            let start = ROUTES_OFFSET + slot * ROUTE_BYTES;
            duplicate_route[start..start + ROUTE_BYTES].copy_from_slice(&route(4).to_slot());
        }
        assert_eq!(RouterState::from_bytes(&duplicate_route), None);
    }

    #[test]
    fn slot_codec_rejects_empty_slots() {
        assert_eq!(Route::from_slot(&[0; ROUTE_BYTES]), None);
        assert_eq!(Route::from_slot(&[0; ROUTE_BYTES - 1]), None);
        assert_eq!(Route::from_slot(&route(8).to_slot()), Some(route(8)));
    }
}
