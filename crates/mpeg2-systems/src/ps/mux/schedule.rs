//! When each pack is due: the SCRs of the packs a [`Muxer`](super::Muxer)
//! writes at a variable or a fixed rate, the packs that fill the time
//! before a PES packet, of a pack header alone over a leap of the time stamps
//! or of padding at a fixed rate, and the length of each pack the packet
//! rate of a CSPS asks for

use core::ops::Range;
use core::time::Duration;

use super::csps::Constraints;
use super::pack_writer::Fill;
use crate::ps::pack_header;
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

/// The packets of a PES packet in a pack (§2.7.9: its
/// `packet_start_code_prefix`)
const PES_PACKETS: usize = 1;

/// The packets of a system header and a program stream map in a pack (§2.7.9:
/// a `system_header_start_code` and a `packet_start_code_prefix`)
const HEADER_PACKETS: usize = 2;

/// The bytes from the one that carries the last bit of the SCR of a pack to
/// the first of the pack after it: the pack header up to that byte
/// (§2.5.3.3), whose index in the pack is 8
const SCR_BYTE: usize = 8;

/// The bytes of `MPEG_program_end_code`
const PROGRAM_END_CODE_LENGTH: usize = 4;

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

/// Returns `bytes` as a `usize`
fn as_usize(bytes: u64) -> usize {
    usize::try_from(bytes).unwrap_or(usize::MAX)
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
    as_usize(bytes)
}

/// Returns the most bytes of a pack: `pack_size`, or fewer where fewer
/// arrive within [`LONGEST_SCR_INTERVAL`] at `program_mux_rate`
pub(super) fn largest_pack(pack_size: usize, program_mux_rate: u32) -> usize {
    pack_size.min(bytes_within_longest_scr_interval(program_mux_rate))
}

/// A pack written: the time of the STC its SCR samples, before
/// [`MuxerConfig::mux_delay`](super::MuxerConfig::mux_delay) is taken off,
/// its bytes, where its SCR is in the stream, and the packets it carries
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
    /// NP of §2.7.9: the `packet_start_code_prefix`es and the
    /// `system_header_start_code`s of the pack
    // Why not whether the SCR is the muxer's `headers_sent_at`: that is
    // cleared when the map changes, and the next SCR would follow the map.
    packets: usize,
}

impl Pack {
    /// Returns the pack of `length` bytes and `packets` packets, its SCR at
    /// `at`, written after `before`, or first
    fn after(before: Option<Self>, at: SystemTimeClock, length: usize, packets: usize) -> Self {
        Self {
            at,
            length,
            offset: before.map_or(0, Self::next_offset),
            packets,
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

    /// Returns the bytes a second of the rate
    const fn program_mux_rate(self) -> u32 {
        match self.0 {
            RateKind::Variable(rate) => rate.program_mux_rate,
            RateKind::Fixed(rate) => rate.line.program_mux_rate(),
        }
    }

    /// Returns the bounds the rate is held to
    const fn constraints(self) -> Constraints {
        match self.0 {
            RateKind::Variable(rate) => rate.constraints,
            RateKind::Fixed(rate) => rate.constraints,
        }
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
    /// `shortest_pes` bytes without them
    pub(super) fn longest_first_pack(
        self,
        system_header: usize,
        program_stream_map: usize,
        shortest_pes: usize,
    ) -> usize {
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
        let arrival = u128::from(SYSTEM_CLOCK_FREQUENCY)
            .saturating_mul(u128::from(as_u64(
                last.length
                    .saturating_add(PROGRAM_END_CODE_LENGTH)
                    .saturating_sub(SCR_BYTE.saturating_add(1)),
            )))
            .div_euclid(u128::from(bytes_per_second(self.program_mux_rate())));
        (arrival < u128::from(self.constraints().packet_ticks(last.packets))).then(|| {
            Pack::after(
                Some(last),
                self.following(last),
                pack_header::FIXED_LENGTH,
                0,
            )
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
            RateKind::Variable(rate) => rate.gap(before, time),
            RateKind::Fixed(rate) => rate.gap(before, time, reserved),
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
#[derive(Debug, Clone, Copy)]
struct Layout {
    gap: Option<Gap>,
    /// The packs of the system header and of the program stream map, and the
    /// fill of each
    headers: Option<[(Pack, Fill); 2]>,
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
                let header_pack =
                    |before: Option<Pack>, at: SystemTimeClock, length: usize, contents: usize| {
                        let fill = Fill::of(length.saturating_sub(contents));
                        (
                            Pack::after(
                                before,
                                at,
                                length,
                                PES_PACKETS.saturating_add(fill.packets()),
                            ),
                            fill,
                        )
                    };
                let first = header_pack(
                    before,
                    at,
                    system_header,
                    pack_header::FIXED_LENGTH.saturating_add(self.system_header),
                );
                [
                    first,
                    header_pack(
                        Some(first.0),
                        self.rate.following(first.0),
                        map,
                        pack_header::FIXED_LENGTH.saturating_add(self.program_stream_map),
                    ),
                ]
            },
        );
        let first_scr = match headers {
            Some([_, (map, _)]) => Some(self.rate.following(map)),
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
    /// where they go on their own, each with the bytes of the two it
    /// carries, and the fill past them
    pub(super) fn header_packs(self) -> impl Iterator<Item = (Pack, Range<usize>, Fill)> {
        let carried = [
            0..self.system_header,
            self.system_header..self.system_header.saturating_add(self.program_stream_map),
        ];
        self.layout()
            .headers
            .into_iter()
            .flat_map(move |packs| packs.into_iter().zip(carried.clone()))
            .map(|((pack, fill), carried)| (pack, carried, fill))
    }

    /// Returns the SCR of the pack that carries the system header
    pub(super) fn headers_scr(self) -> Option<SystemTimeClock> {
        let layout = self.layout();
        layout
            .headers
            .map(|[(system_header, _), _]| system_header.at)
            .or(layout.first_scr)
    }

    /// Returns the SCR of the first pack of the PES packet
    pub(super) fn first_scr(self) -> Option<SystemTimeClock> {
        self.layout().first_scr
    }

    /// Returns the bytes of the system header and the program stream map the
    /// first pack of the PES packet carries
    pub(super) fn first_headers(self) -> Range<usize> {
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

    /// Returns the bytes of the pack of the piece at `index` of the PES
    /// packet, `piece` bytes
    pub(super) fn pes_pack_length(self, index: usize, piece: usize) -> usize {
        let (contents, packets) = self.pes_pack_contents(index, piece);
        self.rate.pack_length(contents, packets)
    }

    /// Returns the pack of the piece at `index` of the PES packet, `piece`
    /// bytes, after `before`, the bytes of the system header and the program
    /// stream map it carries, and the fill past the piece; `None` where the
    /// pack has no SCR, the first pack of the stream without a time stamp
    pub(super) fn pes_pack(
        self,
        index: usize,
        piece: usize,
        before: Option<Pack>,
    ) -> Option<(Pack, Range<usize>, Fill)> {
        let at = if index == 0 {
            self.first_scr()
        } else {
            before.map(|pack| self.rate.following(pack))
        }?;
        let (contents, packets) = self.pes_pack_contents(index, piece);
        let length = self.rate.pack_length(contents, packets);
        let fill = Fill::of(length.saturating_sub(contents));
        let carried = if index == 0 {
            self.first_headers()
        } else {
            0..0
        };
        Some((
            Pack::after(before, at, length, packets.saturating_add(fill.packets())),
            carried,
            fill,
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

/// Returns the packets of a pack of a PES packet that carries `headers` bytes
/// of the system header and the program stream map
const fn pes_pack_packets(headers: usize) -> usize {
    if headers > 0 {
        PES_PACKETS.saturating_add(HEADER_PACKETS)
    } else {
        PES_PACKETS
    }
}

/// The SCRs of a variable rate: each pack once the bytes of the one before
/// have arrived at `program_mux_rate` (§2.5.2.2 equation 2-21), and the
/// packet rate of `constraints` allows it (§2.7.9), or later
#[derive(Debug, Clone, Copy)]
struct VariableRate {
    program_mux_rate: u32,
    constraints: Constraints,
}

impl VariableRate {
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
    /// `pack` have arrived, rounded up to a tick, and its packets allow
    fn following(self, pack: Pack) -> SystemTimeClock {
        // Why round up: an SCR before the last byte of the pack before has
        // arrived would have the next byte arrive before it.
        let ticks = SYSTEM_CLOCK_FREQUENCY
            .saturating_mul(as_u64(pack.length))
            .div_ceil(bytes_per_second(self.program_mux_rate));
        pack.at
            .add_ticks(ticks.max(self.constraints.packet_ticks(pack.packets)))
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
        // interval, and the packet rate asks less of the packets of a pack,
        // so only the time stamp can carry the next SCR past it.
        if time.ticks_since(before.at) <= longest {
            return None;
        }
        let first = self.following(before).max(before.at.add_ticks(longest));
        // Why each one the longest interval after the one before, not after
        // its bytes have arrived: a pack header alone arrives within the
        // longest interval at any program_mux_rate of 1 or more, and carries
        // no packet.
        let count = time
            .ticks_since(first)
            .saturating_sub(longest)
            .div_ceil(longest)
            .saturating_add(1);
        Some(Gap::filling(before, first, count))
    }
}

/// The SCRs of a fixed rate: each on [`Line`], one pack after another, with
/// packs of padding at most `largest_pack` bytes before a PES packet, and
/// each pack as long as the packet rate of `constraints` asks (§2.7.9)
#[derive(Debug, Clone, Copy)]
struct FixedRate {
    line: Line,
    largest_pack: usize,
    constraints: Constraints,
}

impl FixedRate {
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
    fn pack_length(self, contents: usize, packets: usize) -> usize {
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
    fn header_packs(self, system_header: usize, program_stream_map: usize) -> Option<[usize; 2]> {
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
    fn gap(self, before: Pack, time: SystemTimeClock, reserved: usize) -> Option<Gap> {
        let missing = self
            .line
            .reached(before, time)
            .checked_sub(before.next_offset())?
            .checked_sub(as_u64(reserved))?;
        let layout = padding_layout(missing, as_u64(self.largest_pack), as_u64(self.shortest(1)))?;
        Some(Gap::padding(before, self.line, layout))
    }
}

#[cfg(test)]
mod tests;
