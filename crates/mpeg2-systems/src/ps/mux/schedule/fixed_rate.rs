//! The SCRs of a fixed rate, on the line of §2.5.3.6 equations 2-22 and
//! 2-23, and the packs of padding that bring the line to a PES packet

use super::pack::{
    PES_PACKETS, Pack, PlannedPack, arrival_ticks, arrival_ticks_rounded_up, as_u64, as_usize,
    bytes_per_second,
};
use crate::ps::mux::csps::Constraints;
use crate::ps::mux::pack_writer::Fill;
use crate::ps::pack_header;
use crate::time::{SYSTEM_CLOCK_FREQUENCY, SystemTimeClock};

/// The SCRs of a fixed rate: each on [`Line`], one pack after another, with
/// packs of padding at most `largest_pack` bytes before a PES packet, and
/// each pack as long as the packet rate of `constraints` asks (§2.7.9)
#[derive(Debug, Clone, Copy)]
pub(super) struct FixedRate {
    pub(super) line: Line,
    pub(super) largest_pack: usize,
    pub(super) constraints: Constraints,
}

impl FixedRate {
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

    /// Returns the time to plan a PES packet for so that its first pack is
    /// at `earliest` or later: later than `earliest` by the most bytes of
    /// padding [`padding_layout`] leaves out, fewer than a pack header or
    /// those past the stuffing of a last pack too short to carry padding,
    /// rounded up to a tick
    pub(super) fn time_for(self, earliest: SystemTimeClock) -> SystemTimeClock {
        let left_out = as_u64(pack_header::FIXED_LENGTH.saturating_sub(1)).max(
            shortest_padded_pack(as_u64(self.shortest(1)))
                .saturating_sub(stuffed_pack_header())
                .saturating_sub(1),
        );
        earliest.add_ticks(arrival_ticks_rounded_up(
            left_out,
            self.line.program_mux_rate(),
        ))
    }

    /// Returns the SCR of the pack after `pack`, on the line through it
    pub(super) fn following(self, pack: Pack) -> SystemTimeClock {
        self.line.at(pack.at(), pack.offset(), pack.next_offset())
    }

    /// Returns the fewest bytes of a pack of `packets` packets on the line:
    /// those whose ticks the packet rate asks for (§2.7.9)
    fn shortest(self, packets: usize) -> usize {
        // Why not the ticks from the offset of the pack: ⌊c1 × (i + n)⌋ −
        // ⌊c1 × i⌋ ≥ ⌊c1 × n⌋, so the bytes do for a pack at any offset.
        as_usize(self.line.bytes(self.constraints.packet_ticks(packets)))
    }

    /// Returns the bytes of a pack of `contents` bytes and `packets`
    /// packets, filled to those the packet rate asks for, counting the
    /// packets of the fill
    pub(super) fn pack_length(self, contents: usize, packets: usize) -> usize {
        let length = contents.max(self.shortest(packets));
        // Why not loop until the length holds: the fill grows to the bytes of
        // one more packet, fewer than the longest PES packet, so its packets
        // do not grow again.
        match Fill::of(length.saturating_sub(contents)).packets() {
            0 => length,
            fill => length.max(self.shortest(packets.saturating_add(fill))),
        }
    }

    /// Returns the bytes of the packs of the system header and of the
    /// program stream map, each of its own, in a CSPS: `None` where they are
    /// not sent, or the stream is not a CSPS
    pub(super) fn header_packs(
        self,
        system_header: usize,
        program_stream_map: usize,
    ) -> Option<[usize; 2]> {
        // Why not the two in one pack, or with the PES packet: each packet a
        // pack carries asks for more bytes of it, past what a pack of
        // `pack_size` holds where one packet does not.
        (self.constraints.csps_flag() && system_header.saturating_add(program_stream_map) > 0).then(
            || {
                [system_header, program_stream_map].map(|carried| {
                    self.pack_length(
                        pack_header::FIXED_LENGTH.saturating_add(carried),
                        PES_PACKETS,
                    )
                })
            },
        )
    }

    /// Returns the packs of padding after `before`, the last pack written,
    /// that bring the line to `reserved` bytes before a PES packet due at
    /// `time`, as [`padding_layout`] cuts the bytes missing; `None` where
    /// none are
    pub(super) fn gap(
        self,
        before: Pack,
        time: SystemTimeClock,
        reserved: usize,
    ) -> Option<Padding> {
        let missing = self
            .line
            .reached(before.at(), before.offset(), time)
            .checked_sub(before.next_offset())?
            .checked_sub(as_u64(reserved))?;
        let layout = padding_layout(missing, as_u64(self.largest_pack), as_u64(self.shortest(1)))?;
        Some(Padding {
            before,
            line: self.line,
            layout,
        })
    }
}

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
    fn ticks(self, bytes: u64) -> u64 {
        arrival_ticks(bytes, self.program_mux_rate)
    }

    /// Returns the fewest bytes whose [`ticks`](Self::ticks) are `ticks` or
    /// more
    pub(super) fn bytes(self, ticks: u64) -> u64 {
        let bytes = u128::from(ticks)
            .saturating_mul(u128::from(bytes_per_second(self.program_mux_rate)))
            .div_ceil(u128::from(SYSTEM_CLOCK_FREQUENCY));
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    /// Returns the time on the line through `through`, the SCR
    /// `through_offset` bytes from the first SCR, `offset` bytes from the
    /// first SCR, which is not before `through`
    pub(super) fn at(
        self,
        through: SystemTimeClock,
        through_offset: u64,
        offset: u64,
    ) -> SystemTimeClock {
        // Why not the ticks of `offset - through_offset` alone: ⌊c1 × i⌋ is
        // taken of the whole offset, or each pack would round on its own and
        // drift from the line.
        through.add_ticks(
            self.ticks(offset)
                .saturating_sub(self.ticks(through_offset)),
        )
    }

    /// Returns the fewest bytes from the first SCR at which the line through
    /// `through`, the SCR `through_offset` bytes from the first SCR, is at
    /// `time` or after it
    pub(super) fn reached(
        self,
        through: SystemTimeClock,
        through_offset: u64,
        time: SystemTimeClock,
    ) -> u64 {
        self.bytes(
            self.ticks(through_offset)
                .saturating_add(time.ticks_since(through)),
        )
    }
}

/// How the bytes missing before a PES packet are cut into packs of padding:
/// `total` bytes in packs of `longest` bytes but the last two, each of a
/// length a pack of padding may take
#[derive(Debug, Clone, Copy)]
struct PaddingLayout {
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
fn padding_layout(missing: u64, longest: u64, shortest_padded: u64) -> Option<PaddingLayout> {
    let shortest_padded = shortest_padded_pack(shortest_padded);
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

/// Returns the fewest bytes of a pack that carries a PES packet of padding,
/// a pack of `shortest_padded` bytes at the least and of more than the
/// stuffing
fn shortest_padded_pack(shortest_padded: u64) -> u64 {
    shortest_padded.max(stuffed_pack_header().saturating_add(1))
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

/// Packs of padding on `line` through `before`, as `layout` cuts them
#[derive(Debug, Clone, Copy)]
pub(super) struct Padding {
    before: Pack,
    line: Line,
    layout: PaddingLayout,
}

impl Padding {
    /// Returns the bytes of all the packs
    pub(super) const fn bytes(self) -> u64 {
        self.layout.total
    }

    /// Returns how many packs there are
    pub(super) fn count(self) -> u64 {
        self.layout.count()
    }

    /// Returns the pack at `index`
    pub(super) fn pack(self, index: u64) -> PlannedPack {
        let (length, packs_before) = self.layout.pack(index);
        let offset = self.before.next_offset().saturating_add(packs_before);
        PlannedPack::filled(
            self.line.at(self.before.at(), self.before.offset(), offset),
            offset,
            as_usize(length),
            pack_header::FIXED_LENGTH,
            0,
            0..0,
        )
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{FixedRate, Line};
    use crate::ps::mux::csps::Constraints;
    use crate::ps::mux::schedule::pack::PlannedPack;
    use crate::time::{SystemTimeClock, Timestamp};

    /// A `program_mux_rate` of 1 260 000 bytes a second
    const RATE: u32 = 25_200;

    /// Ticks of the STC that 7 bytes take to arrive at [`RATE`]
    const TICKS_OF_SEVEN_BYTES: u64 = 150;

    fn timestamp(value: u64) -> Timestamp {
        Timestamp::try_from(value).unwrap()
    }

    #[test]
    fn the_padding_fills_the_bytes_missing_exactly_in_packs_of_at_most_the_longest_from_a_pack_header_on()
     {
        let before = PlannedPack::after(
            None,
            SystemTimeClock::first(timestamp(90_000)),
            100,
            100,
            1,
            0..0,
        )
        .pack;
        let lengths = |bytes: u64| {
            let time = before
                .at()
                .add_ticks((100 + bytes) * TICKS_OF_SEVEN_BYTES / 7);
            FixedRate {
                line: Line::new(RATE),
                largest_pack: 2_048,
                constraints: Constraints::none(),
            }
            .gap(before, time, 0)
            .map(|padding| {
                (
                    (0..padding.count())
                        .map(|index| padding.pack(index).pack.length())
                        .collect::<Vec<_>>(),
                    padding.bytes(),
                    padding.pack(padding.count() - 1).pack.next_offset() - 100,
                )
            })
        };

        // Below a pack header, none; from it on, the bytes missing
        assert_eq!(
            [0, 1, 13, 14, 2_048, 2_049, 2_061, 2_062, 4_097].map(lengths),
            [
                None,
                None,
                None,
                Some((vec![14], 14, 14)),
                Some((vec![2_048], 2_048, 2_048)),
                Some((vec![2_035, 14], 2_049, 2_049)),
                Some((vec![2_047, 14], 2_061, 2_061)),
                Some((vec![2_048, 14], 2_062, 2_062)),
                Some((vec![2_048, 2_035, 14], 4_097, 4_097)),
            ]
        );
    }

    #[test]
    fn the_line_is_taken_in_whole_ticks_rounded_down_past_the_width_of_u64() {
        // 2^40 bytes: 27 000 000 × 2^40 is past u64
        let far = 1_u64 << 40;
        let far_ticks = u64::try_from(u128::from(far) * 150 / 7).unwrap();

        assert_eq!(
            (
                [0, 1, 6, 7, far].map(|bytes| Line::new(RATE).ticks(bytes)),
                Line::new(1).ticks(u64::MAX),
                [128, 129, 150, 151].map(|ticks| Line::new(RATE).bytes(ticks)),
            ),
            ([0, 21, 128, 150, far_ticks], u64::MAX, [6, 7, 7, 8])
        );
    }
}
