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

/// Returns [`LONGEST_SCR_INTERVAL`] in ticks of the STC
fn longest_scr_interval() -> u64 {
    SystemTimeClock::ticks_in(LONGEST_SCR_INTERVAL)
}

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
/// `count` of them, [`LONGEST_SCR_INTERVAL`] apart from `first`
#[derive(Debug, Clone, Copy)]
pub(super) struct Filling {
    first: SystemTimeClock,
    pub(super) count: u64,
}

impl Filling {
    /// Returns the packs that fill the leap from `last`, the last pack
    /// written, to `time`: each [`LONGEST_SCR_INTERVAL`] after the SCR before
    /// it, or once the bytes before it have arrived if that is later, while
    /// the pack at `time` would be more than [`LONGEST_SCR_INTERVAL`] after
    /// the SCR before it; `None` where there is no leap to fill
    pub(super) fn across(last: Pack, time: SystemTimeClock, program_mux_rate: u32) -> Option<Self> {
        let longest = longest_scr_interval();
        // Why compare with `time` alone, not with when `last` has arrived:
        // `largest_pack` keeps every pack arriving within the longest
        // interval, so only the time stamp can carry the next SCR past it.
        if time.ticks_since(last.at) <= longest {
            return None;
        }
        let first = last
            .arrived(program_mux_rate)
            .max(last.at.add_ticks(longest));
        // Why each one the longest interval after the one before, not after
        // its bytes have arrived: a pack header alone arrives within the
        // longest interval at any program_mux_rate of 1 or more.
        let count = time
            .ticks_since(first)
            .saturating_sub(longest)
            .div_ceil(longest)
            .saturating_add(1);
        Some(Self { first, count })
    }

    pub(super) fn iter(self) -> impl Iterator<Item = SystemTimeClock> {
        let longest = longest_scr_interval();
        (0..self.count).map(move |index| self.first.add_ticks(index.saturating_mul(longest)))
    }
}
