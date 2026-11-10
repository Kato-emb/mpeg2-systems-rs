//! When each pack is due: the SCRs of the packs a [`Muxer`](super::Muxer)
//! writes at a variable or a fixed rate, and the packs that fill the time
//! before a PES packet, of a pack header alone over a leap of the time stamps
//! or of padding at a fixed rate

use core::time::Duration;

use crate::ps::pack_header;
use crate::time::{SYSTEM_CLOCK_FREQUENCY, SystemTimeClock};

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
pub(super) fn bytes_within_longest_scr_interval(program_mux_rate: u32) -> usize {
    // Why not the ticks the bytes take, rounded up as `VariableRate` does:
    // in whole units of program_mux_rate and of a millisecond the count is
    // exact, and that many bytes, rounded up to a tick, still arrive within
    // the interval.
    let bytes =
        bytes_per_second(program_mux_rate).saturating_mul(LONGEST_SCR_INTERVAL_MILLIS) / 1_000;
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// A pack written: the time of the STC its SCR samples, before
/// [`MuxerConfig::mux_delay`](super::MuxerConfig::mux_delay) is taken off,
/// its bytes, and where its SCR is in the stream
#[derive(Debug, Clone, Copy)]
pub(super) struct Pack {
    pub(super) at: SystemTimeClock,
    // Why not the time its last byte arrives: the SCR of the next pack
    // depends on this time, and the leap that needs filling on `at`.
    pub(super) length: usize,
    /// The bytes from the one that carries the last bit of the first SCR to
    /// the one that carries the last bit of the SCR of this pack
    // Why not a count of the bytes the muxer has written: it would have to
    // move with `last_pack` on every pack written.
    // Why not only at a fixed rate, as `Option`: whether it is there would
    // say what the rate of the muxer says.
    pub(super) offset: u64,
}

impl Pack {
    /// Returns the `offset` of the pack after it
    pub(super) fn next_offset(self) -> u64 {
        self.offset.saturating_add(as_u64(self.length))
    }
}

/// The SCRs of a variable rate: each pack once the bytes of the one before
/// have arrived at `program_mux_rate` (§2.5.2.2 equation 2-21), or later
#[derive(Debug, Clone, Copy)]
pub(super) struct VariableRate {
    program_mux_rate: u32,
}

impl VariableRate {
    pub(super) const fn new(program_mux_rate: u32) -> Self {
        Self { program_mux_rate }
    }

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
    /// `pack` have arrived, rounded up to a tick
    pub(super) fn following(self, pack: Pack) -> SystemTimeClock {
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
    pub(super) fn gap(self, before: Pack, time: SystemTimeClock) -> Option<Gap> {
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
        Some(Gap::Filling {
            before,
            first,
            count,
        })
    }
}

/// The SCRs of a fixed rate: each on [`Line`], one pack after another, with
/// packs of padding at most `largest_pack` bytes before a PES packet
#[derive(Debug, Clone, Copy)]
pub(super) struct FixedRate {
    line: Line,
    largest_pack: usize,
}

impl FixedRate {
    /// Returns the fixed rate `program_mux_rate`, its packs at most
    /// `largest_pack` bytes
    pub(super) const fn new(program_mux_rate: u32, largest_pack: usize) -> Self {
        Self {
            line: Line::new(program_mux_rate),
            largest_pack,
        }
    }

    /// Returns the SCR of the first pack of a PES packet of `time` after
    /// `before`, the last pack written: on the line after `before`, or
    /// `time` for the first pack
    pub(super) fn first_scr(
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
    pub(super) fn following(self, pack: Pack) -> SystemTimeClock {
        self.line.at(pack, pack.next_offset())
    }

    /// Returns the packs of padding after `before`, the last pack written,
    /// that bring the line to a PES packet due at `time`, as
    /// [`padding_layout`] cuts the bytes missing; `None` where none are
    pub(super) fn gap(self, before: Pack, time: SystemTimeClock) -> Option<Gap> {
        let missing = self
            .line
            .reached(before, time)
            .checked_sub(before.next_offset())?;
        let layout = padding_layout(missing, as_u64(self.largest_pack))?;
        Some(Gap::Padding {
            before,
            line: self.line,
            layout,
        })
    }
}

/// The line of a fixed rate, ISO/IEC 13818-1 §2.5.3.6 equations 2-22 and
/// 2-23: SCR(i) = c1 × i + c2, c1 the ticks of the STC a byte takes to arrive
/// at `program_mux_rate` (§2.5.2.2 equation 2-21)
#[derive(Debug, Clone, Copy)]
pub(super) struct Line {
    program_mux_rate: u32,
}

impl Line {
    pub(super) const fn new(program_mux_rate: u32) -> Self {
        Self { program_mux_rate }
    }

    /// Returns ⌊c1 × `bytes`⌋, or [`u64::MAX`] past it
    pub(super) fn ticks(self, bytes: u64) -> u64 {
        // Why not in u64: the product passes it at the offsets a long stream
        // reaches.
        let ticks = u128::from(SYSTEM_CLOCK_FREQUENCY)
            .saturating_mul(u128::from(bytes))
            .div_euclid(u128::from(bytes_per_second(self.program_mux_rate)));
        u64::try_from(ticks).unwrap_or(u64::MAX)
    }

    /// Returns the fewest bytes whose [`ticks`](Self::ticks) are `ticks` or
    /// more
    pub(super) fn bytes(self, ticks: u64) -> u64 {
        let bytes = u128::from(ticks)
            .saturating_mul(u128::from(bytes_per_second(self.program_mux_rate)))
            .div_ceil(u128::from(SYSTEM_CLOCK_FREQUENCY));
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    /// Returns the time on the line through the SCR of `through`, `offset`
    /// bytes from the first SCR, which is not before `through`
    fn at(self, through: Pack, offset: u64) -> SystemTimeClock {
        // Why not the ticks of `offset - through.offset` alone: ⌊c1 × i⌋ is
        // taken of the whole offset, or each pack would round on its own and
        // drift from the line.
        through.at.add_ticks(
            self.ticks(offset)
                .saturating_sub(self.ticks(through.offset)),
        )
    }

    /// Returns the fewest bytes from the first SCR at which the line through
    /// the SCR of `through` is at `time` or after it
    fn reached(self, through: Pack, time: SystemTimeClock) -> u64 {
        self.bytes(
            self.ticks(through.offset)
                .saturating_add(time.ticks_since(through.at)),
        )
    }
}

/// How the bytes missing before a PES packet are cut into packs of padding:
/// `total` bytes in packs of `longest` bytes but the last two, the last no
/// shorter than a pack header
#[derive(Debug, Clone, Copy)]
pub(super) struct PaddingLayout {
    longest: u64,
    total: u64,
}

/// Returns how `missing` bytes are cut into packs of padding of at most
/// `longest` bytes, each no shorter than a pack header, or `None` where none
/// are
///
/// The packs take exactly the bytes missing: `longest` each, and the one
/// before the last gives up what the last needs to be a pack header. Where
/// fewer bytes than a pack header are missing, or are left for the last and
/// the one before it cannot give them up without falling below a pack
/// header itself, those bytes are left out, and the PES packet comes that
/// much early.
fn padding_layout(missing: u64, longest: u64) -> Option<PaddingLayout> {
    let header = as_u64(pack_header::FIXED_LENGTH);
    let count = missing.div_ceil(longest);
    let rest = missing.saturating_sub(count.saturating_sub(1).saturating_mul(longest));
    let total = if rest >= header
        || (count >= 2 && longest.saturating_add(rest) >= header.saturating_mul(2))
    {
        missing
    } else {
        // Why not a pack header for the bytes left: the PES packet would
        // come after its time, where leaving them has it come early.
        missing.saturating_sub(rest)
    };
    (total > 0).then_some(PaddingLayout { longest, total })
}

impl PaddingLayout {
    /// Returns how many packs there are
    fn count(self) -> u64 {
        self.total.div_ceil(self.longest)
    }

    /// Returns the bytes of the pack at `index`, and of the packs before it
    fn pack(self, index: u64) -> (u64, u64) {
        let header = as_u64(pack_header::FIXED_LENGTH);
        let count = self.count();
        let others = self.longest.saturating_mul(count.saturating_sub(1));
        let last = self.total.saturating_sub(others).max(header);
        let before = self.longest.saturating_mul(index);
        if index.saturating_add(1) == count {
            (last, self.total.saturating_sub(last))
        } else if index.saturating_add(2) == count {
            (
                self.total.saturating_sub(before).saturating_sub(last),
                before,
            )
        } else {
            (self.longest, before)
        }
    }
}

/// The packs that fill the time from the pack after the one before them to
/// a PES packet
#[derive(Debug, Clone, Copy)]
pub(super) enum Gap {
    /// `count` packs of a pack header alone over a leap of the time stamps,
    /// [`LONGEST_SCR_INTERVAL`] apart from `first` (§2.7.1)
    Filling {
        before: Pack,
        first: SystemTimeClock,
        count: u64,
    },
    /// Packs of padding at a fixed rate, on `line` through `before`, as
    /// `layout` cuts them
    Padding {
        before: Pack,
        line: Line,
        layout: PaddingLayout,
    },
}

impl Gap {
    /// Returns the bytes of all the packs
    pub(super) fn bytes(self) -> u64 {
        match self {
            Self::Filling { count, .. } => count.saturating_mul(as_u64(pack_header::FIXED_LENGTH)),
            Self::Padding { layout, .. } => layout.total,
        }
    }

    /// Returns the last of the packs
    pub(super) fn last(self) -> Pack {
        self.pack(self.count().saturating_sub(1))
    }

    pub(super) fn iter(self) -> impl Iterator<Item = Pack> {
        (0..self.count()).map(move |index| self.pack(index))
    }

    fn count(self) -> u64 {
        match self {
            Self::Filling { count, .. } => count,
            Self::Padding { layout, .. } => layout.count(),
        }
    }

    /// Returns the pack at `index`
    fn pack(self, index: u64) -> Pack {
        match self {
            Self::Filling { before, first, .. } => Pack {
                at: first.add_ticks(index.saturating_mul(longest_scr_interval())),
                length: pack_header::FIXED_LENGTH,
                offset: before
                    .next_offset()
                    .saturating_add(index.saturating_mul(as_u64(pack_header::FIXED_LENGTH))),
            },
            Self::Padding {
                before,
                line,
                layout,
            } => {
                let (length, packs_before) = layout.pack(index);
                let offset = before.next_offset().saturating_add(packs_before);
                Pack {
                    at: line.at(before, offset),
                    length: usize::try_from(length).unwrap_or(usize::MAX),
                    offset,
                }
            }
        }
    }
}
