//! When each pack is due: the SCRs of the packs a [`Muxer`](super::Muxer)
//! writes at a variable or a fixed rate, the packs that fill the time
//! before a PES packet, of a pack header alone over a leap of the time stamps
//! or of padding at a fixed rate, and the length of each pack the packet
//! rate of a CSPS asks for

use core::ops::Range;

use super::csps::Constraints;
use crate::ps::pack_header;
use crate::time::SystemTimeClock;
use fixed_rate::{FixedRate, Line, Padding};
use pack::{PES_PACKETS, arrival_ticks, as_u64, as_usize, pes_pack_packets};
use variable_rate::{Filling, VariableRate};

mod fixed_rate;
mod pack;
mod variable_rate;

pub(super) use pack::{Pack, PlannedPack, largest_pack, shortest_pack};

/// The bytes from the one that carries the last bit of the SCR of a pack to
/// the first of the pack after it: the pack header up to that byte
/// (§2.5.3.3), whose index in the pack is 8
const SCR_BYTE: usize = 8;

/// The bytes of `MPEG_program_end_code`
const PROGRAM_END_CODE_LENGTH: usize = 4;

/// The rate of a Program Stream, variable or fixed, which decides the SCR
/// of each pack, the packs that fill the time before a PES packet, and the
/// length of each pack
#[derive(Debug, Clone, Copy)]
pub(super) struct Rate(RateKind);

/// Which rate a [`Rate`] is
#[derive(Debug, Clone, Copy)]
enum RateKind {
    Variable(VariableRate),
    Fixed(FixedRate),
}

impl Rate {
    /// Returns the variable rate `program_mux_rate`, held to `constraints`
    pub(super) const fn variable(program_mux_rate: u32, constraints: Constraints) -> Self {
        Self(RateKind::Variable(VariableRate {
            program_mux_rate,
            constraints,
        }))
    }

    /// Returns the fixed rate `program_mux_rate`, its packs at most
    /// `largest_pack` bytes, held to `constraints`
    pub(super) const fn fixed(
        program_mux_rate: u32,
        largest_pack: usize,
        constraints: Constraints,
    ) -> Self {
        Self(RateKind::Fixed(FixedRate {
            line: Line::new(program_mux_rate),
            largest_pack,
            constraints,
        }))
    }

    /// Returns `fixed_flag` of the system header at the rate
    pub(super) const fn fixed_flag(self) -> bool {
        matches!(self.0, RateKind::Fixed(_))
    }

    /// Returns the packs that carry a PES packet due at `time` after
    /// `before`, the last pack written, with a system header of
    /// `system_header` bytes and a program stream map of
    /// `program_stream_map` bytes, both 0 where they are not sent
    pub(super) const fn plan(
        self,
        before: Option<Pack>,
        time: Option<SystemTimeClock>,
        system_header: usize,
        program_stream_map: usize,
    ) -> Plan {
        Plan {
            rate: self,
            before,
            time,
            system_header,
            program_stream_map,
        }
    }

    /// Returns the bytes of the longest pack the first PES packet takes at
    /// the least, sent with a system header of `system_header` bytes and a
    /// program stream map of `program_stream_map` bytes, its shortest pack
    /// [`shortest_pack`] without them
    pub(super) fn longest_first_pack(
        self,
        system_header: usize,
        program_stream_map: usize,
    ) -> usize {
        let shortest_pes = shortest_pack();
        match self.header_packs(system_header, program_stream_map) {
            Some(lengths) => lengths
                .into_iter()
                .fold(self.pack_length(shortest_pes, PES_PACKETS), usize::max),
            None => {
                let headers = system_header.saturating_add(program_stream_map);
                self.pack_length(
                    shortest_pes.saturating_add(headers),
                    pes_pack_packets(headers),
                )
            }
        }
    }

    /// Returns the pack of a pack header alone that goes after `last`, the
    /// last pack written, and before `MPEG_program_end_code`, where the end
    /// code would arrive sooner than the packet rate allows (§2.7.9)
    pub(super) fn closing(self, last: Pack) -> Option<Pack> {
        // Why not the time to the next SCR, as for any other pack: the end
        // code ends before the byte the next SCR would be carried in.
        let (program_mux_rate, constraints) = match self.0 {
            RateKind::Variable(rate) => (rate.program_mux_rate, rate.constraints),
            RateKind::Fixed(rate) => (rate.line.program_mux_rate(), rate.constraints),
        };
        let arrival = arrival_ticks(
            as_u64(
                last.length()
                    .saturating_add(PROGRAM_END_CODE_LENGTH)
                    .saturating_sub(SCR_BYTE.saturating_add(1)),
            ),
            program_mux_rate,
        );
        (arrival < constraints.packet_ticks(last.packets())).then(|| {
            PlannedPack::after(
                Some(last),
                self.following(last),
                pack_header::FIXED_LENGTH,
                pack_header::FIXED_LENGTH,
                0,
                0..0,
            )
            .pack
        })
    }

    /// Returns the SCR of the pack after `pack`
    fn following(self, pack: Pack) -> SystemTimeClock {
        match self.0 {
            RateKind::Variable(rate) => rate.following(pack),
            RateKind::Fixed(rate) => rate.following(pack),
        }
    }

    /// Returns the bytes of a pack of `contents` bytes and `packets` packets
    /// with the fill the rate asks for
    fn pack_length(self, contents: usize, packets: usize) -> usize {
        match self.0 {
            RateKind::Variable(_) => contents,
            RateKind::Fixed(rate) => rate.pack_length(contents, packets),
        }
    }

    /// Returns the bytes of the packs of the system header and of the
    /// program stream map, each of its own, where the rate puts them apart
    /// from the PES packet
    fn header_packs(self, system_header: usize, program_stream_map: usize) -> Option<[usize; 2]> {
        match self.0 {
            RateKind::Variable(_) => None,
            RateKind::Fixed(rate) => rate.header_packs(system_header, program_stream_map),
        }
    }

    /// Returns the SCR of the first pack of a PES packet of `time` after
    /// `before`
    fn first_scr(
        self,
        before: Option<Pack>,
        time: Option<SystemTimeClock>,
    ) -> Option<SystemTimeClock> {
        match self.0 {
            RateKind::Variable(rate) => rate.first_scr(before, time),
            RateKind::Fixed(rate) => rate.first_scr(before, time),
        }
    }

    /// Returns the packs after `before` that fill the time to a PES packet
    /// due at `time`, `reserved` bytes of other packs left before it
    fn gap(self, before: Pack, time: SystemTimeClock, reserved: usize) -> Option<Gap> {
        match self.0 {
            RateKind::Variable(rate) => rate
                .gap(before, time)
                .map(|filling| Gap(GapKind::Filling(filling))),
            RateKind::Fixed(rate) => rate
                .gap(before, time, reserved)
                .map(|padding| Gap(GapKind::Padding(padding))),
        }
    }
}

/// The packs that carry a PES packet: those that fill the time before it,
/// those of the system header and the program stream map where they go on
/// their own, and those of the PES packet
#[derive(Debug, Clone, Copy)]
pub(super) struct Plan {
    rate: Rate,
    before: Option<Pack>,
    time: Option<SystemTimeClock>,
    system_header: usize,
    program_stream_map: usize,
}

/// Where the packs of a [`Plan`] go before the PES packet
#[derive(Debug, Clone)]
struct Layout {
    gap: Option<Gap>,
    /// The packs of the system header and of the program stream map
    headers: Option<[PlannedPack; 2]>,
    first_scr: Option<SystemTimeClock>,
}

impl Plan {
    /// Returns where the packs go
    fn layout(self) -> Layout {
        let lengths = self
            .rate
            .header_packs(self.system_header, self.program_stream_map);
        let reserved = lengths.map_or(0, |[system_header, map]| system_header.saturating_add(map));
        let gap = self
            .before
            .zip(self.time)
            .and_then(|(before, time)| self.rate.gap(before, time, reserved));
        let before = gap.map(Gap::last).or(self.before);
        let headers = lengths.zip(self.rate.first_scr(before, self.time)).map(
            |([system_header, map], at)| {
                let carried = self.system_header;
                let first = PlannedPack::after(
                    before,
                    at,
                    system_header,
                    pack_header::FIXED_LENGTH.saturating_add(carried),
                    PES_PACKETS,
                    0..carried,
                );
                let second = PlannedPack::after(
                    Some(first.pack),
                    self.rate.following(first.pack),
                    map,
                    pack_header::FIXED_LENGTH.saturating_add(self.program_stream_map),
                    PES_PACKETS,
                    carried..carried.saturating_add(self.program_stream_map),
                );
                [first, second]
            },
        );
        let first_scr = match &headers {
            Some([_, map]) => Some(self.rate.following(map.pack)),
            None => self.rate.first_scr(before, self.time),
        };
        Layout {
            gap,
            headers,
            first_scr,
        }
    }

    /// Returns the packs that fill the time before the PES packet
    pub(super) fn gap(self) -> Option<Gap> {
        self.layout().gap
    }

    /// Returns the packs of the system header and of the program stream map
    /// where they go on their own
    pub(super) fn header_packs(self) -> impl Iterator<Item = PlannedPack> {
        self.layout().headers.into_iter().flatten()
    }

    /// Returns the SCR of the pack that carries the system header
    pub(super) fn headers_scr(self) -> Option<SystemTimeClock> {
        let layout = self.layout();
        layout
            .headers
            .map(|[system_header, _]| system_header.pack.at())
            .or(layout.first_scr)
    }

    /// Returns the SCR of the first pack of the PES packet
    pub(super) fn first_scr(self) -> Option<SystemTimeClock> {
        self.layout().first_scr
    }

    /// Returns the bytes of the system header and the program stream map the
    /// first pack of the PES packet carries
    fn first_headers(self) -> Range<usize> {
        if self
            .rate
            .header_packs(self.system_header, self.program_stream_map)
            .is_some()
        {
            0..0
        } else {
            0..self.system_header.saturating_add(self.program_stream_map)
        }
    }

    /// Returns the most bytes the first piece of the PES packet, and each
    /// piece after it, may take in a pack of at most `largest_pack` bytes
    pub(super) fn longest_pieces(self, largest_pack: usize) -> (usize, usize) {
        let room = largest_pack.saturating_sub(pack_header::FIXED_LENGTH);
        (room.saturating_sub(self.first_headers().len()), room)
    }

    /// Returns the bytes the first pack of the PES packet carries before its
    /// fill, `piece` bytes of the PES packet among them
    pub(super) fn first_pes_pack_contents(self, piece: usize) -> usize {
        self.pes_pack_contents(0, piece).0
    }

    /// Returns the bytes of the pack of the piece at `index` of the PES
    /// packet, `piece` bytes
    pub(super) fn pes_pack_length(self, index: usize, piece: usize) -> usize {
        let (contents, packets) = self.pes_pack_contents(index, piece);
        self.rate.pack_length(contents, packets)
    }

    /// Returns the pack of the piece at `index` of the PES packet, `piece`
    /// bytes, after `before`; `None` where the pack has no SCR, the first
    /// pack of the stream without a time stamp
    pub(super) fn pes_pack(
        self,
        index: usize,
        piece: usize,
        before: Option<Pack>,
    ) -> Option<PlannedPack> {
        let at = if index == 0 {
            self.first_scr()
        } else {
            before.map(|pack| self.rate.following(pack))
        }?;
        let (contents, packets) = self.pes_pack_contents(index, piece);
        let carried = if index == 0 {
            self.first_headers()
        } else {
            0..0
        };
        Some(PlannedPack::after(
            before,
            at,
            self.rate.pack_length(contents, packets),
            contents,
            packets,
            carried,
        ))
    }

    /// Returns the bytes and the packets the pack of the piece at `index`
    /// carries before its fill
    fn pes_pack_contents(self, index: usize, piece: usize) -> (usize, usize) {
        let headers = if index == 0 {
            self.first_headers().len()
        } else {
            0
        };
        (
            pack_header::FIXED_LENGTH
                .saturating_add(headers)
                .saturating_add(piece),
            pes_pack_packets(headers),
        )
    }
}

/// The packs that fill the time from the pack after the one before them to
/// a PES packet: of a pack header alone over a leap of the time stamps, or
/// of padding at a fixed rate
#[derive(Debug, Clone, Copy)]
pub(super) struct Gap(GapKind);

/// What the packs of a [`Gap`] are
#[derive(Debug, Clone, Copy)]
enum GapKind {
    Filling(Filling),
    Padding(Padding),
}

impl Gap {
    /// Returns the bytes of all the packs
    pub(super) fn bytes(self) -> usize {
        as_usize(match self.0 {
            GapKind::Filling(filling) => filling.bytes(),
            GapKind::Padding(padding) => padding.bytes(),
        })
    }

    /// Returns the last of the packs
    fn last(self) -> Pack {
        self.pack(self.count().saturating_sub(1)).pack
    }

    pub(super) fn iter(self) -> impl Iterator<Item = PlannedPack> {
        (0..self.count()).map(move |index| self.pack(index))
    }

    fn count(self) -> u64 {
        match self.0 {
            GapKind::Filling(filling) => filling.count(),
            GapKind::Padding(padding) => padding.count(),
        }
    }

    /// Returns the pack at `index`
    fn pack(self, index: u64) -> PlannedPack {
        match self.0 {
            GapKind::Filling(filling) => filling.pack(index),
            GapKind::Padding(padding) => padding.pack(index),
        }
    }
}
