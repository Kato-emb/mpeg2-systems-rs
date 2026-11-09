//! When each pack is due: the SCRs of the packs a [`Muxer`](super::Muxer)
//! writes, and the packs of a pack header alone that fill a leap of the time
//! stamps

use core::time::Duration;

use crate::time::SystemTimeClock;

/// Bytes per second in a unit of `program_mux_rate`, ISO/IEC 13818-1 §2.5.3.4
const PROGRAM_MUX_RATE_UNIT: u64 = 50;

/// The longest time from one SCR to the next, in milliseconds, ISO/IEC
/// 13818-1 §2.7.1
const LONGEST_SCR_INTERVAL_MILLIS: u64 = 700;

/// The longest time from one SCR to the next, ISO/IEC 13818-1 §2.7.1
const LONGEST_SCR_INTERVAL: Duration = Duration::from_millis(LONGEST_SCR_INTERVAL_MILLIS);

/// The time from one pack of a pack header alone to the next, where the time
/// stamps leap past [`LONGEST_SCR_INTERVAL`]
// Why not LONGEST_SCR_INTERVAL itself: the pack after the last of them is due
// at the time of its PES packet, which may fall up to a whole step after it,
// so a step no shorter than the longest interval could leave it too far.
// Why not a step from the end of the pack before: a pack header alone takes
// less than this step to arrive at any program_mux_rate of 1 or more, so the
// next one is never due before the step is over.
const FILLING_PACK_INTERVAL: Duration = Duration::from_millis(500);

/// Returns the most bytes that arrive within [`LONGEST_SCR_INTERVAL`] at
/// `program_mux_rate` (§2.5.2.2 equation 2-21), the longest pack after which
/// the next SCR can follow within it (§2.7.1)
pub(super) fn bytes_within_longest_scr_interval(program_mux_rate: u32) -> usize {
    // Why not the ticks the bytes take, rounded up as `Pack::arrived` does:
    // in whole units of program_mux_rate and of a millisecond the count is
    // exact, and that many bytes, rounded up to a tick, still arrive within
    // the interval.
    let bytes = u64::from(program_mux_rate)
        .saturating_mul(PROGRAM_MUX_RATE_UNIT)
        .saturating_mul(LONGEST_SCR_INTERVAL_MILLIS)
        / 1_000;
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// A pack written: the time of the STC its SCR samples, before
/// [`MuxerConfig::mux_delay`] is taken off, and its bytes
#[derive(Debug, Clone, Copy)]
pub(super) struct Pack {
    pub(super) at: SystemTimeClock,
    // Why not the time its last byte arrives: the SCR of the next pack
    // depends on this time, and the leap that needs filling on `at`.
    pub(super) length: usize,
}

impl Pack {
    /// Returns the time the bytes of the pack have arrived at
    /// `program_mux_rate` (§2.5.2.2 equation 2-21), rounded up to a tick
    pub(super) fn arrived(self, program_mux_rate: u32) -> SystemTimeClock {
        let length = u64::try_from(self.length).unwrap_or(u64::MAX);
        // Why round up: an SCR before the last byte of the pack before has
        // arrived would have the next byte arrive before it.
        let ticks = SystemTimeClock::ticks_in(Duration::from_secs(1))
            .saturating_mul(length)
            .div_ceil(u64::from(program_mux_rate).saturating_mul(PROGRAM_MUX_RATE_UNIT));
        self.at.add_ticks(ticks)
    }
}

/// The times of the packs of a pack header alone before a PES packet:
/// `count` of them, [`FILLING_PACK_INTERVAL`] apart from `first`
#[derive(Debug, Clone, Copy)]
pub(super) struct Filling {
    first: SystemTimeClock,
    pub(super) count: u64,
}

impl Filling {
    /// Returns the packs that fill the leap from `last`, the last pack
    /// written, to `time`, so that no two SCRs are more than
    /// [`LONGEST_SCR_INTERVAL`] apart, `None` where there is no leap to fill
    pub(super) fn across(last: Pack, time: SystemTimeClock, program_mux_rate: u32) -> Option<Self> {
        let longest = SystemTimeClock::ticks_in(LONGEST_SCR_INTERVAL);
        let step = SystemTimeClock::ticks_in(FILLING_PACK_INTERVAL);
        if time.ticks_since(last.at) <= longest {
            return None;
        }
        let first = last.arrived(program_mux_rate).max(last.at.add_ticks(step));
        // Why the count from `first` alone: each pack after the first is due
        // a step after the one before, which it outlasts (see
        // FILLING_PACK_INTERVAL).
        let count = time
            .ticks_since(first)
            .saturating_sub(longest)
            .div_ceil(step)
            .saturating_add(1);
        Some(Self { first, count })
    }

    pub(super) fn iter(self) -> impl Iterator<Item = SystemTimeClock> {
        let step = SystemTimeClock::ticks_in(FILLING_PACK_INTERVAL);
        (0..self.count).map(move |index| self.first.add_ticks(index.saturating_mul(step)))
    }
}
