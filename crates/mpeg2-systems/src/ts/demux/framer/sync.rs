//! Search for the packet grid: the `sync_byte` cadence and the packet size it
//! implies

use super::Input;
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
}

/// What the search saw at the front of the bytes it has not ruled on
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SearchObservation {
    /// This many bytes before the next `sync_byte` value, or to the end
    LeadingGarbage(usize),
    /// A `sync_byte` value at the front repeats at this period
    GridConfirmed(PacketSize),
    /// Too few bytes to confirm or rule out every candidate period
    Undecided,
    /// Too few bytes to decide, and no more will follow
    UndecidedAtEnd,
    /// A `sync_byte` value at the front that no candidate period repeats
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
    input: Input,
) -> SearchObservation {
    let scanned = bytes.get(..garbage_limit).unwrap_or(bytes);
    let leading_garbage = scanned
        .iter()
        .position(|&byte| byte == SYNC_BYTE)
        .unwrap_or(scanned.len());
    if leading_garbage > 0 {
        return SearchObservation::LeadingGarbage(leading_garbage);
    }
    let candidates = fixed
        .as_ref()
        .map_or(&CANDIDATES[..], core::slice::from_ref);
    let mut undecided = false;
    for &packet_size in candidates {
        match check_grid(bytes, packet_size.stride()) {
            // Why not tie-break against a larger period confirming first:
            // candidates run smallest-first, and a smaller period being
            // undecided means the bytes are too few for any larger one to
            // confirm (188 undecided => len <= 4 * 188, but 192 needs
            // 4 * 192 + 1), so the first Confirmed is always the smallest
            // matching period.
            GridCheck::Confirmed => return SearchObservation::GridConfirmed(packet_size),
            GridCheck::RuledOut => {}
            GridCheck::NeedMoreData => undecided = true,
        }
    }
    match (undecided, input) {
        (true, Input::Continues) => SearchObservation::Undecided,
        (true, Input::Ends) => SearchObservation::UndecidedAtEnd,
        (false, _) => SearchObservation::NoGridAtFront,
    }
}

/// Checks the `sync_byte` cadence at offsets `0, stride, 2 * stride, ...`
fn check_grid(bytes: &[u8], stride: usize) -> GridCheck {
    let sampled = bytes.iter().step_by(stride).take(SYNC_LOCK_COUNT);
    if sampled.clone().any(|&byte| byte != SYNC_BYTE) {
        GridCheck::RuledOut
    } else if sampled.count() < SYNC_LOCK_COUNT {
        GridCheck::NeedMoreData
    } else {
        GridCheck::Confirmed
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
                    &grid(packet_size.stride(), SYNC_LOCK_COUNT, 0),
                    None,
                    usize::MAX,
                    Input::Continues
                ),
                SearchObservation::GridConfirmed(packet_size)
            );
        }
    }

    #[test]
    fn four_sync_bytes_leave_the_grid_undecided() {
        assert_eq!(
            observe(
                &grid(188, SYNC_LOCK_COUNT - 1, 0),
                None,
                usize::MAX,
                Input::Continues
            ),
            SearchObservation::Undecided
        );
    }

    #[test]
    fn bytes_before_the_first_sync_byte_are_leading_garbage() {
        assert_eq!(
            observe(
                &grid(192, SYNC_LOCK_COUNT, 4),
                None,
                usize::MAX,
                Input::Continues
            ),
            SearchObservation::LeadingGarbage(4)
        );
    }

    #[test]
    fn a_sync_byte_no_period_repeats_is_no_grid() {
        let mut bytes = grid(188, SYNC_LOCK_COUNT, 0);
        bytes[188 * 3] = 0x00;
        bytes.extend_from_slice(&[0x00; 204 * SYNC_LOCK_COUNT]);
        assert_eq!(
            observe(&bytes, None, usize::MAX, Input::Continues),
            SearchObservation::NoGridAtFront
        );
    }

    #[test]
    fn a_fixed_packet_size_rules_out_the_other_periods() {
        let mut bytes = grid(188, SYNC_LOCK_COUNT, 0);
        bytes.extend_from_slice(&[0x00; 204 * SYNC_LOCK_COUNT]);
        assert_eq!(
            observe(
                &bytes,
                Some(PacketSize::ReedSolomon204),
                usize::MAX,
                Input::Continues
            ),
            SearchObservation::NoGridAtFront
        );
    }
}
