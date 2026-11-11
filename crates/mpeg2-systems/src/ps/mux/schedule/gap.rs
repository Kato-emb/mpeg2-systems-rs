//! The packs that fill the time before a PES packet: of a pack header alone
//! over a leap of the time stamps, or of padding on the line of a fixed rate

use super::{Pack, PlannedPack, as_u64, as_usize, bytes_per_second, longest_scr_interval};
use crate::ps::pack_header;
use crate::time::{SYSTEM_CLOCK_FREQUENCY, SystemTimeClock};

/// Returns the bytes of a pack header with all its `stuffing_byte`s
/// (§2.5.3.4)
fn stuffed_pack_header() -> u64 {
    as_u64(pack_header::FIXED_LENGTH)
        .saturating_add(u64::from(pack_header::MAX_PACK_STUFFING_LENGTH))
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

    /// Returns the `program_mux_rate` of the line
    pub(super) const fn program_mux_rate(self) -> u32 {
        self.program_mux_rate
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
    pub(super) fn at(self, through: Pack, offset: u64) -> SystemTimeClock {
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
    pub(super) fn reached(self, through: Pack, time: SystemTimeClock) -> u64 {
        self.bytes(
            self.ticks(through.offset)
                .saturating_add(time.ticks_since(through.at)),
        )
    }
}

/// How the bytes missing before a PES packet are cut into packs of padding:
/// `total` bytes in packs of `longest` bytes but the last two, each of a
/// length a pack of padding may take
#[derive(Debug, Clone, Copy)]
pub(super) struct PaddingLayout {
    longest: u64,
    total: u64,
    /// The fewest bytes of a pack that carries a PES packet of padding
    shortest_padded: u64,
}

/// Returns how `missing` bytes are cut into packs of padding of at most
/// `longest` bytes, each a pack header with stuffing alone or a pack of at
/// least `shortest_padded` bytes and of more than the stuffing, or `None`
/// where none are
///
/// The packs take exactly the bytes missing: `longest` each, and the one
/// before the last gives up what the last needs to be a pack it may be.
/// Where fewer bytes than a pack header are missing, or are left for the
/// last and the one before it cannot give them up, those bytes are left out,
/// and the PES packet comes that much early; where the last would carry
/// padding in fewer than `shortest_padded` bytes, it takes the pack header
/// and its stuffing alone, and the rest is left out.
pub(super) fn padding_layout(
    missing: u64,
    longest: u64,
    shortest_padded: u64,
) -> Option<PaddingLayout> {
    let shortest_padded = shortest_padded.max(stuffed_pack_header().saturating_add(1));
    let layout = |total| PaddingLayout {
        longest,
        total,
        shortest_padded,
    };
    let count = missing.div_ceil(longest);
    let rest = missing.saturating_sub(count.saturating_sub(1).saturating_mul(longest));
    let probe = layout(missing);
    let stuffed = stuffed_pack_header();
    let total = if probe.is_pack(rest)
        || (count >= 2
            && probe.is_pack(
                longest
                    .saturating_add(rest)
                    .saturating_sub(probe.lift(rest)),
            )) {
        missing
    } else if rest > stuffed {
        // Why not a pack of `shortest_padded` for the bytes left: the PES
        // packet would come after its time, where leaving them has it come
        // early.
        missing.saturating_sub(rest).saturating_add(stuffed)
    } else {
        // Why not a pack header for the bytes left: as above.
        missing.saturating_sub(rest)
    };
    (total > 0).then_some(layout(total))
}

impl PaddingLayout {
    /// Returns whether a pack of padding may be `length` bytes: a pack
    /// header with stuffing alone, or a pack of `shortest_padded` to
    /// `longest` bytes
    fn is_pack(self, length: u64) -> bool {
        (as_u64(pack_header::FIXED_LENGTH)..=stuffed_pack_header()).contains(&length)
            || (self.shortest_padded..=self.longest).contains(&length)
    }

    /// Returns the bytes the last pack takes of `rest`, the bytes left for
    /// it: a pack header where fewer, `shortest_padded` where it would carry
    /// padding in fewer, or `rest`
    fn lift(self, rest: u64) -> u64 {
        let header = as_u64(pack_header::FIXED_LENGTH);
        if rest < header {
            header
        } else if rest > stuffed_pack_header() && rest < self.shortest_padded {
            self.shortest_padded
        } else {
            rest
        }
    }

    /// Returns how many packs there are
    fn count(self) -> u64 {
        self.total.div_ceil(self.longest)
    }

    /// Returns the bytes of the pack at `index`, and of the packs before it
    fn pack(self, index: u64) -> (u64, u64) {
        let count = self.count();
        let others = self.longest.saturating_mul(count.saturating_sub(1));
        let last = self.lift(self.total.saturating_sub(others));
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
pub(in crate::ps::mux) struct Gap(GapKind);

/// What the packs of a [`Gap`] are
#[derive(Debug, Clone, Copy)]
enum GapKind {
    /// `count` packs of a pack header alone over a leap of the time stamps,
    /// [`LONGEST_SCR_INTERVAL`](super::LONGEST_SCR_INTERVAL) apart from `first` (§2.7.1)
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
    /// Returns `count` packs of a pack header alone after `before`, the
    /// first at `first`
    pub(super) const fn filling(before: Pack, first: SystemTimeClock, count: u64) -> Self {
        Self(GapKind::Filling {
            before,
            first,
            count,
        })
    }

    /// Returns the packs of padding on `line` through `before`, as `layout`
    /// cuts them
    pub(super) const fn padding(before: Pack, line: Line, layout: PaddingLayout) -> Self {
        Self(GapKind::Padding {
            before,
            line,
            layout,
        })
    }

    /// Returns the bytes of all the packs
    pub(in crate::ps::mux) fn bytes(self) -> u64 {
        match self.0 {
            GapKind::Filling { count, .. } => {
                count.saturating_mul(as_u64(pack_header::FIXED_LENGTH))
            }
            GapKind::Padding { layout, .. } => layout.total,
        }
    }

    /// Returns the last of the packs
    pub(super) fn last(self) -> Pack {
        self.pack(self.count().saturating_sub(1)).pack
    }

    pub(in crate::ps::mux) fn iter(self) -> impl Iterator<Item = PlannedPack> {
        (0..self.count()).map(move |index| self.pack(index))
    }

    fn count(self) -> u64 {
        match self.0 {
            GapKind::Filling { count, .. } => count,
            GapKind::Padding { layout, .. } => layout.count(),
        }
    }

    /// Returns the pack at `index`
    fn pack(self, index: u64) -> PlannedPack {
        let header = pack_header::FIXED_LENGTH;
        match self.0 {
            GapKind::Filling { before, first, .. } => PlannedPack::filled(
                first.add_ticks(index.saturating_mul(longest_scr_interval())),
                before
                    .next_offset()
                    .saturating_add(index.saturating_mul(as_u64(header))),
                header,
                header,
                0,
                0..0,
            ),
            GapKind::Padding {
                before,
                line,
                layout,
            } => {
                let (length, packs_before) = layout.pack(index);
                let offset = before.next_offset().saturating_add(packs_before);
                PlannedPack::filled(
                    line.at(before, offset),
                    offset,
                    as_usize(length),
                    header,
                    0,
                    0..0,
                )
            }
        }
    }
}
