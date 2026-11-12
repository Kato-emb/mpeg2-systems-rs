//! The SCRs of a variable rate, and the packs of a pack header alone that
//! fill a leap of the time stamps

use super::pack::{Pack, PlannedPack, arrival_ticks_rounded_up, as_u64, longest_scr_interval};
use crate::ps::mux::csps::Constraints;
use crate::ps::pack_header;
use crate::time::SystemTimeClock;

/// The SCRs of a variable rate: each pack once the bytes of the one before
/// have arrived at `program_mux_rate` (§2.5.2.2 equation 2-21), and the
/// packet rate of `constraints` allows it (§2.7.9), or later
#[derive(Debug, Clone, Copy)]
pub(super) struct VariableRate {
    pub(super) program_mux_rate: u32,
    pub(super) constraints: Constraints,
}

impl VariableRate {
    /// Returns the SCR of the first pack of a PES packet of `time` after
    /// `before`, the last pack written: the later of `time` and the SCR the
    /// pack after `before` takes at the earliest
    pub(super) fn first_scr(
        self,
        before: Option<Pack>,
        time: Option<SystemTimeClock>,
    ) -> Option<SystemTimeClock> {
        // Why not `None` as the earliest: before the first pack there is no
        // pack to follow, and the first PES packet has a time.
        before.map(|pack| self.following(pack)).max(time)
    }

    /// Returns the earliest SCR of the pack after `pack`: once the bytes of
    /// `pack` have arrived, rounded up to a tick, and its packets allow
    pub(super) fn following(self, pack: Pack) -> SystemTimeClock {
        // Why round up: an SCR before the last byte of the pack before has
        // arrived would have the next byte arrive before it.
        let ticks = arrival_ticks_rounded_up(as_u64(pack.length()), self.program_mux_rate);
        pack.at()
            .add_ticks(ticks.max(self.constraints.packet_ticks(pack.packets())))
    }

    /// Returns the packs of a pack header alone that fill a leap from
    /// `before`, the last pack written, to a PES packet due at `time`: each
    /// [`LONGEST_SCR_INTERVAL`](super::pack::LONGEST_SCR_INTERVAL) after the SCR before it, or once the bytes
    /// before it have arrived if that is later, while the pack at `time`
    /// would be more than [`LONGEST_SCR_INTERVAL`](super::pack::LONGEST_SCR_INTERVAL) after the SCR before it;
    /// `None` where there is no leap to fill
    pub(super) fn gap(self, before: Pack, time: SystemTimeClock) -> Option<Filling> {
        let longest = longest_scr_interval();
        // Why compare with `time` alone, not with when `before` has arrived:
        // `largest_pack` keeps every pack arriving within the longest
        // interval, and the packet rate asks less of the packets of a pack,
        // so only the time stamp can carry the next SCR past it.
        if time.ticks_since(before.at()) <= longest {
            return None;
        }
        let first = self.following(before).max(before.at().add_ticks(longest));
        // Why each one the longest interval after the one before, not after
        // its bytes have arrived: a pack header alone arrives within the
        // longest interval at any program_mux_rate of 1 or more, and carries
        // no packet.
        let count = time
            .ticks_since(first)
            .saturating_sub(longest)
            .div_ceil(longest)
            .saturating_add(1);
        Some(Filling {
            before,
            first,
            count,
        })
    }
}

/// `count` packs of a pack header alone over a leap of the time stamps,
/// [`LONGEST_SCR_INTERVAL`](super::pack::LONGEST_SCR_INTERVAL) apart from `first` (§2.7.1), after
/// `before`
#[derive(Debug, Clone, Copy)]
pub(super) struct Filling {
    before: Pack,
    first: SystemTimeClock,
    count: u64,
}

impl Filling {
    /// Returns the bytes of all the packs
    pub(super) fn bytes(self) -> u64 {
        self.count.saturating_mul(as_u64(pack_header::FIXED_LENGTH))
    }

    /// Returns how many packs there are
    pub(super) const fn count(self) -> u64 {
        self.count
    }

    /// Returns the pack at `index`
    pub(super) fn pack(self, index: u64) -> PlannedPack {
        let header = pack_header::FIXED_LENGTH;
        PlannedPack::filled(
            self.first
                .add_ticks(index.saturating_mul(longest_scr_interval())),
            self.before
                .next_offset()
                .saturating_add(index.saturating_mul(as_u64(header))),
            header,
            header,
            0,
            0..0,
        )
    }
}
