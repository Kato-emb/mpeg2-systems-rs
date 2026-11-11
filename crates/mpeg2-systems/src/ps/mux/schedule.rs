//! When each pack is due: the SCRs of the packs a [`Muxer`](super::Muxer)
//! writes at a variable or a fixed rate, and the packs that fill the time
//! before a PES packet, of a pack header alone over a leap of the time stamps
//! or of padding at a fixed rate

use core::time::Duration;

use crate::time::{SYSTEM_CLOCK_FREQUENCY, SystemTimeClock};
use gap::{Line, padding_layout};

mod gap;

pub(super) use gap::Gap;

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

/// Returns the bytes a second `program_mux_rate` gives (§2.5.3.4)
fn bytes_per_second(program_mux_rate: u32) -> u64 {
    u64::from(program_mux_rate).saturating_mul(PROGRAM_MUX_RATE_UNIT)
}

/// Returns `bytes` as a `u64`
fn as_u64(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// Returns the most bytes that arrive within [`LONGEST_SCR_INTERVAL`] at
/// `program_mux_rate` (§2.5.2.2 equation 2-21), the longest pack after which
/// the next SCR can follow within it (§2.7.1)
fn bytes_within_longest_scr_interval(program_mux_rate: u32) -> usize {
    // Why not the ticks the bytes take, rounded up as `VariableRate` does:
    // in whole units of program_mux_rate and of a millisecond the count is
    // exact, and that many bytes, rounded up to a tick, still arrive within
    // the interval.
    let bytes =
        bytes_per_second(program_mux_rate).saturating_mul(LONGEST_SCR_INTERVAL_MILLIS) / 1_000;
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// Returns the most bytes of a pack: `pack_size`, or fewer where fewer
/// arrive within [`LONGEST_SCR_INTERVAL`] at `program_mux_rate`
pub(super) fn largest_pack(pack_size: usize, program_mux_rate: u32) -> usize {
    pack_size.min(bytes_within_longest_scr_interval(program_mux_rate))
}

/// A pack written: the time of the STC its SCR samples, before
/// [`MuxerConfig::mux_delay`](super::MuxerConfig::mux_delay) is taken off,
/// its bytes, and where its SCR is in the stream
#[derive(Debug, Clone, Copy)]
pub(super) struct Pack {
    at: SystemTimeClock,
    // Why not the time its last byte arrives: the SCR of the next pack
    // depends on this time, and the leap that needs filling on `at`.
    length: usize,
    /// The bytes from the one that carries the last bit of the first SCR to
    /// the one that carries the last bit of the SCR of this pack
    // Why not a count of the bytes the muxer has written: it would have to
    // move with `last_pack` on every pack written.
    // Why not only at a fixed rate, as `Option`: whether it is there would
    // say what the rate of the muxer says.
    offset: u64,
}

impl Pack {
    /// Returns the pack of `length` bytes, its SCR at `at`, written after
    /// `before`, or first
    pub(super) fn after(before: Option<Self>, at: SystemTimeClock, length: usize) -> Self {
        Self {
            at,
            length,
            offset: before.map_or(0, Self::next_offset),
        }
    }

    /// Returns the time of the STC the SCR of the pack samples, before
    /// `mux_delay` is taken off
    pub(super) const fn at(self) -> SystemTimeClock {
        self.at
    }

    /// Returns the bytes of the pack
    pub(super) const fn length(self) -> usize {
        self.length
    }

    /// Returns the `offset` of the pack after it
    fn next_offset(self) -> u64 {
        self.offset.saturating_add(as_u64(self.length))
    }
}

/// The rate of a Program Stream, variable or fixed, which decides the SCR
/// of each pack and the packs that fill the time before a PES packet
#[derive(Debug, Clone, Copy)]
pub(super) struct Rate(RateKind);

/// Which rate a [`Rate`] is
#[derive(Debug, Clone, Copy)]
enum RateKind {
    Variable(VariableRate),
    Fixed(FixedRate),
}

impl Rate {
    /// Returns the variable rate `program_mux_rate`
    pub(super) const fn variable(program_mux_rate: u32) -> Self {
        Self(RateKind::Variable(VariableRate::new(program_mux_rate)))
    }

    /// Returns the fixed rate `program_mux_rate`, its packs at most
    /// `largest_pack` bytes
    pub(super) const fn fixed(program_mux_rate: u32, largest_pack: usize) -> Self {
        Self(RateKind::Fixed(FixedRate::new(
            program_mux_rate,
            largest_pack,
        )))
    }

    /// Returns `fixed_flag` of the system header at the rate
    pub(super) const fn fixed_flag(self) -> bool {
        matches!(self.0, RateKind::Fixed(_))
    }

    /// Returns the SCR of the first pack of a PES packet of `time` after
    /// `before`, the last pack written
    pub(super) fn first_scr(
        self,
        before: Option<Pack>,
        time: Option<SystemTimeClock>,
    ) -> Option<SystemTimeClock> {
        match self.0 {
            RateKind::Variable(rate) => rate.first_scr(before, time),
            RateKind::Fixed(rate) => rate.first_scr(before, time),
        }
    }

    /// Returns the SCR of the pack after `pack` that carries the rest of a
    /// PES packet
    pub(super) fn following(self, pack: Pack) -> SystemTimeClock {
        match self.0 {
            RateKind::Variable(rate) => rate.following(pack),
            RateKind::Fixed(rate) => rate.following(pack),
        }
    }

    /// Returns the packs after `before`, the last pack written, that fill
    /// the time to a PES packet due at `time`
    pub(super) fn gap(self, before: Pack, time: SystemTimeClock) -> Option<Gap> {
        match self.0 {
            RateKind::Variable(rate) => rate.gap(before, time),
            RateKind::Fixed(rate) => rate.gap(before, time),
        }
    }
}

/// The SCRs of a variable rate: each pack once the bytes of the one before
/// have arrived at `program_mux_rate` (§2.5.2.2 equation 2-21), or later
#[derive(Debug, Clone, Copy)]
struct VariableRate {
    program_mux_rate: u32,
}

impl VariableRate {
    const fn new(program_mux_rate: u32) -> Self {
        Self { program_mux_rate }
    }

    /// Returns the SCR of the first pack of a PES packet of `time` after
    /// `before`, the last pack written: the later of `time` and the SCR the
    /// pack after `before` takes at the earliest
    fn first_scr(
        self,
        before: Option<Pack>,
        time: Option<SystemTimeClock>,
    ) -> Option<SystemTimeClock> {
        // Why not `None` as the earliest: before the first pack there is no
        // pack to follow, and the first PES packet has a time.
        before.map(|pack| self.following(pack)).max(time)
    }

    /// Returns the earliest SCR of the pack after `pack`: once the bytes of
    /// `pack` have arrived, rounded up to a tick
    fn following(self, pack: Pack) -> SystemTimeClock {
        // Why round up: an SCR before the last byte of the pack before has
        // arrived would have the next byte arrive before it.
        let ticks = SYSTEM_CLOCK_FREQUENCY
            .saturating_mul(as_u64(pack.length))
            .div_ceil(bytes_per_second(self.program_mux_rate));
        pack.at.add_ticks(ticks)
    }

    /// Returns the packs of a pack header alone that fill a leap from
    /// `before`, the last pack written, to a PES packet due at `time`: each
    /// [`LONGEST_SCR_INTERVAL`] after the SCR before it, or once the bytes
    /// before it have arrived if that is later, while the pack at `time`
    /// would be more than [`LONGEST_SCR_INTERVAL`] after the SCR before it;
    /// `None` where there is no leap to fill
    fn gap(self, before: Pack, time: SystemTimeClock) -> Option<Gap> {
        let longest = longest_scr_interval();
        // Why compare with `time` alone, not with when `before` has arrived:
        // `largest_pack` keeps every pack arriving within the longest
        // interval, so only the time stamp can carry the next SCR past it.
        if time.ticks_since(before.at) <= longest {
            return None;
        }
        let first = self.following(before).max(before.at.add_ticks(longest));
        // Why each one the longest interval after the one before, not after
        // its bytes have arrived: a pack header alone arrives within the
        // longest interval at any program_mux_rate of 1 or more.
        let count = time
            .ticks_since(first)
            .saturating_sub(longest)
            .div_ceil(longest)
            .saturating_add(1);
        Some(Gap::filling(before, first, count))
    }
}

/// The SCRs of a fixed rate: each on [`Line`], one pack after another, with
/// packs of padding at most `largest_pack` bytes before a PES packet
#[derive(Debug, Clone, Copy)]
struct FixedRate {
    line: Line,
    largest_pack: usize,
}

impl FixedRate {
    /// Returns the fixed rate `program_mux_rate`, its packs at most
    /// `largest_pack` bytes
    const fn new(program_mux_rate: u32, largest_pack: usize) -> Self {
        Self {
            line: Line::new(program_mux_rate),
            largest_pack,
        }
    }

    /// Returns the SCR of the first pack of a PES packet of `time` after
    /// `before`, the last pack written: on the line after `before`, or
    /// `time` for the first pack
    fn first_scr(
        self,
        before: Option<Pack>,
        time: Option<SystemTimeClock>,
    ) -> Option<SystemTimeClock> {
        match before {
            Some(pack) => Some(self.following(pack)),
            None => time,
        }
    }

    /// Returns the SCR of the pack after `pack`, on the line through it
    fn following(self, pack: Pack) -> SystemTimeClock {
        self.line.at(pack, pack.next_offset())
    }

    /// Returns the packs of padding after `before`, the last pack written,
    /// that bring the line to a PES packet due at `time`, as
    /// [`padding_layout`] cuts the bytes missing; `None` where none are
    fn gap(self, before: Pack, time: SystemTimeClock) -> Option<Gap> {
        let missing = self
            .line
            .reached(before, time)
            .checked_sub(before.next_offset())?;
        let layout = padding_layout(missing, as_u64(self.largest_pack))?;
        Some(Gap::padding(before, self.line, layout))
    }
}

#[cfg(test)]
mod tests;
