//! The packet grid: the search for the `sync_byte` cadence and the packet
//! size it implies, and the units on the grid once found

use crate::ts::packet::{PACKET_LENGTH, SYNC_BYTE};

// ETSI TR 101 290: lock onto the grid after five consecutive sync bytes at a
// fixed period.
pub(super) const SYNC_LOCK_COUNT: usize = 5;

/// Bytes from one Transport Packet to the next on the wire, which stay the same
/// through a Transport Stream
///
/// The 188-byte `transport_packet` of ISO/IEC 13818-1 §2.4.3.2, alone or with
/// the bytes a carrier adds around each one. The demuxer frames the 188 bytes
/// and does not read what the carrier added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PacketSize {
    /// The Transport Packet alone, 188 bytes
    Standard188,
    /// A 4-byte timestamp before each Transport Packet, 192 bytes
    Timestamped192,
    /// 16 Reed-Solomon parity bytes after each Transport Packet, 204 bytes
    ReedSolomon204,
}

impl PacketSize {
    /// Returns the bytes from one `sync_byte` to the next
    #[must_use]
    pub const fn stride(self) -> usize {
        match self {
            Self::Standard188 => PACKET_LENGTH,
            Self::Timestamped192 => 192,
            Self::ReedSolomon204 => 204,
        }
    }

    /// Bytes the carrier puts before each `sync_byte`
    pub(super) const fn prefix(self) -> usize {
        match self {
            Self::Timestamped192 => 4,
            Self::Standard188 | Self::ReedSolomon204 => 0,
        }
    }
}

/// What the search saw at the front of the bytes it has not ruled on
///
/// A unit is the stride of bytes one packet takes on the wire; it starts
/// [`PacketSize::prefix`] bytes before its `sync_byte`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchObservation {
    /// This many bytes before the first place a unit can start
    LeadingGarbage(usize),
    /// A unit starts at the front, and its `sync_byte` repeats at this period
    GridConfirmed(PacketSize),
    /// Too few bytes to confirm or rule out every candidate period
    Undecided,
    /// A unit could start at the front, but no candidate period repeats it
    NoGridAtFront,
}

/// Every packet size, smallest first
const CANDIDATES: [PacketSize; 3] = [
    PacketSize::Standard188,
    PacketSize::Timestamped192,
    PacketSize::ReedSolomon204,
];

enum GridCheck {
    Confirmed,
    RuledOut,
    NeedMoreData,
}

/// Classifies the front of `bytes` against the candidate packet grids
///
/// A fixed packet size restricts the search to that period; `None` tries all
/// three smallest-first. [`SearchObservation::LeadingGarbage`] counts at most
/// `garbage_limit` bytes.
pub(super) fn observe(
    bytes: &[u8],
    fixed: Option<PacketSize>,
    garbage_limit: usize,
) -> SearchObservation {
    let candidates = fixed
        .as_ref()
        .map_or(&CANDIDATES[..], core::slice::from_ref);
    let longest_prefix = candidates
        .iter()
        .map(|packet_size| packet_size.prefix())
        .max()
        .unwrap_or_default();
    // Why stop short of the end by the longest prefix when no unit start is
    // found: a sync_byte past the end can still start a unit that many bytes
    // before it.
    let horizon = garbage_limit.min(bytes.len().saturating_sub(longest_prefix));
    let mut leading_garbage = horizon;
    let scanned = bytes
        .get(..horizon.saturating_add(longest_prefix))
        .unwrap_or(bytes);
    for (index, _) in scanned
        .iter()
        .enumerate()
        .filter(|&(_, &byte)| byte == SYNC_BYTE)
    {
        // Why stop here: a later sync_byte starts a unit at most the longest
        // prefix before it, so none can start one earlier than found.
        if index >= leading_garbage.saturating_add(longest_prefix) {
            break;
        }
        for packet_size in candidates {
            if let Some(start) = index.checked_sub(packet_size.prefix()) {
                leading_garbage = leading_garbage.min(start);
            }
        }
    }
    if leading_garbage > 0 {
        return SearchObservation::LeadingGarbage(leading_garbage);
    }
    let mut undecided = false;
    for &packet_size in candidates {
        match check_grid(bytes, packet_size) {
            // Why not tie-break against a larger period confirming first:
            // candidates run smallest-first, and a smaller period being
            // undecided means the bytes are too few for any larger one to
            // confirm (188 undecided => len <= 4 * 188, but 192 needs
            // 4 + 4 * 192 + 1), so the first Confirmed is always the smallest
            // matching period.
            GridCheck::Confirmed => return SearchObservation::GridConfirmed(packet_size),
            GridCheck::RuledOut => {}
            GridCheck::NeedMoreData => undecided = true,
        }
    }
    if undecided {
        SearchObservation::Undecided
    } else {
        SearchObservation::NoGridAtFront
    }
}

/// Checks the `sync_byte` cadence of units starting at the front
fn check_grid(bytes: &[u8], packet_size: PacketSize) -> GridCheck {
    let sampled = bytes
        .get(packet_size.prefix()..)
        .unwrap_or_default()
        .iter()
        .step_by(packet_size.stride())
        .take(SYNC_LOCK_COUNT);
    if sampled.clone().any(|&byte| byte != SYNC_BYTE) {
        GridCheck::RuledOut
    } else if sampled.count() < SYNC_LOCK_COUNT {
        GridCheck::NeedMoreData
    } else {
        GridCheck::Confirmed
    }
}

/// What the unit at the front of the bytes holds on the locked grid
pub(super) enum FrameObservation {
    OnGrid,
    SyncByteMissing {
        found: u8,
    },
    /// Fewer bytes than a unit, but a packet on the grid without the carrier
    /// bytes after it
    PacketWithoutCarrier,
    /// Fewer bytes than a unit, and no packet on the grid among them
    Short,
}

/// Classifies the unit at the front of `bytes` against the locked grid
pub(super) fn observe_on_grid(bytes: &[u8], packet_size: PacketSize) -> FrameObservation {
    let prefix = packet_size.prefix();
    match (bytes.get(prefix), bytes.len()) {
        (Some(&SYNC_BYTE), length) if length >= packet_size.stride() => FrameObservation::OnGrid,
        (Some(&found), length) if length >= packet_size.stride() => {
            FrameObservation::SyncByteMissing { found }
        }
        (Some(&SYNC_BYTE), length) if length >= prefix.saturating_add(PACKET_LENGTH) => {
            FrameObservation::PacketWithoutCarrier
        }
        _ => FrameObservation::Short,
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "fixtures are built at constant offsets well inside their length"
)]
pub(super) mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    /// A `sync_byte` at `lead + k * stride` for each of `sync_count` packets,
    /// and no other 0x47
    pub(in crate::ts::demux::framer) fn grid(
        stride: usize,
        sync_count: usize,
        lead: usize,
    ) -> Vec<u8> {
        let mut bytes = vec![0x00; lead + stride * sync_count];
        for packet in 0..sync_count {
            bytes[lead + stride * packet] = SYNC_BYTE;
        }
        bytes
    }

    #[test]
    fn five_sync_bytes_at_one_period_confirm_the_grid() {
        for packet_size in CANDIDATES {
            assert_eq!(
                observe(
                    &grid(packet_size.stride(), SYNC_LOCK_COUNT, packet_size.prefix()),
                    None,
                    usize::MAX
                ),
                SearchObservation::GridConfirmed(packet_size)
            );
        }
    }

    #[test]
    fn four_sync_bytes_leave_the_grid_undecided() {
        assert_eq!(
            observe(&grid(188, SYNC_LOCK_COUNT - 1, 0), None, usize::MAX),
            SearchObservation::Undecided
        );
    }

    #[test]
    fn a_192_byte_unit_starts_at_the_timestamp_before_its_sync_byte() {
        assert_eq!(
            observe(&grid(192, SYNC_LOCK_COUNT, 4), None, usize::MAX),
            SearchObservation::GridConfirmed(PacketSize::Timestamped192)
        );
    }

    #[test]
    fn bytes_before_the_first_unit_are_leading_garbage() {
        assert_eq!(
            observe(
                &grid(188, SYNC_LOCK_COUNT, 10),
                Some(PacketSize::Standard188),
                usize::MAX
            ),
            SearchObservation::LeadingGarbage(10)
        );
    }

    #[test]
    fn detection_keeps_the_bytes_a_timestamp_could_take_before_a_sync_byte() {
        assert_eq!(
            observe(&grid(188, SYNC_LOCK_COUNT, 10), None, usize::MAX),
            SearchObservation::LeadingGarbage(10 - 4)
        );
    }

    #[test]
    fn a_sync_byte_with_no_room_for_a_timestamp_starts_no_192_byte_unit() {
        assert_eq!(
            observe(
                &grid(192, SYNC_LOCK_COUNT, 2),
                Some(PacketSize::Timestamped192),
                usize::MAX
            ),
            SearchObservation::LeadingGarbage(2 + 192 - 4)
        );
    }

    #[test]
    fn a_sync_byte_no_period_repeats_is_no_grid() {
        let mut bytes = grid(188, SYNC_LOCK_COUNT, 0);
        bytes[188 * 3] = 0x00;
        bytes.extend_from_slice(&[0x00; 204 * SYNC_LOCK_COUNT]);
        assert_eq!(
            observe(&bytes, None, usize::MAX),
            SearchObservation::NoGridAtFront
        );
    }

    #[test]
    fn a_fixed_packet_size_rules_out_the_other_periods() {
        let mut bytes = grid(188, SYNC_LOCK_COUNT, 0);
        bytes.extend_from_slice(&[0x00; 204 * SYNC_LOCK_COUNT]);
        assert_eq!(
            observe(&bytes, Some(PacketSize::ReedSolomon204), usize::MAX),
            SearchObservation::NoGridAtFront
        );
    }
}
