//! A pack a [`Rate`](super::Rate) plans: its SCR, its bytes and the packets
//! it carries, and the units that count them

use core::ops::Range;
use core::time::Duration;

use crate::pes;
use crate::ps::mux::pack_writer::Fill;
use crate::ps::{PackHeader, pack_header};
use crate::time::{SYSTEM_CLOCK_FREQUENCY, SystemTimeClock};

/// Bytes per second in a unit of `program_mux_rate`, ISO/IEC 13818-1 §2.5.3.4
const PROGRAM_MUX_RATE_UNIT: u64 = 50;

/// The longest time from one SCR to the next, in milliseconds, ISO/IEC
/// 13818-1 §2.7.1
const LONGEST_SCR_INTERVAL_MILLIS: u64 = 700;

/// The longest time from one SCR to the next, ISO/IEC 13818-1 §2.7.1
const LONGEST_SCR_INTERVAL: Duration = Duration::from_millis(LONGEST_SCR_INTERVAL_MILLIS);

/// The packets of a PES packet in a pack (§2.7.9: its
/// `packet_start_code_prefix`)
pub(super) const PES_PACKETS: usize = 1;

/// The packets of a system header and a program stream map in a pack (§2.7.9:
/// a `system_header_start_code` and a `packet_start_code_prefix`)
const HEADER_PACKETS: usize = 2;

/// Returns [`LONGEST_SCR_INTERVAL`] in ticks of the STC
pub(super) fn longest_scr_interval() -> u64 {
    SystemTimeClock::ticks_in(LONGEST_SCR_INTERVAL)
}

/// Returns the bytes a second `program_mux_rate` gives (§2.5.3.4)
pub(super) fn bytes_per_second(program_mux_rate: u32) -> u64 {
    u64::from(program_mux_rate).saturating_mul(PROGRAM_MUX_RATE_UNIT)
}

/// Returns ⌊c1 × `bytes`⌋, the ticks of the STC `bytes` bytes take to
/// arrive at `program_mux_rate` (§2.5.2.2 equation 2-21) rounded down, or
/// [`u64::MAX`] past it
pub(super) fn arrival_ticks(bytes: u64, program_mux_rate: u32) -> u64 {
    // Why not in u64: the product passes it at the offsets a long stream
    // reaches.
    let ticks = u128::from(SYSTEM_CLOCK_FREQUENCY)
        .saturating_mul(u128::from(bytes))
        .div_euclid(u128::from(bytes_per_second(program_mux_rate)));
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

/// Returns `bytes` as a `u64`
pub(super) fn as_u64(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// Returns `bytes` as a `usize`
pub(super) fn as_usize(bytes: u64) -> usize {
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

/// Returns the bytes of the shortest pack: a pack header, the shortest PES
/// packet header and a byte of payload
pub(in crate::ps::mux) fn shortest_pack() -> usize {
    pack_header::FIXED_LENGTH
        .saturating_add(pes::PREFIX_LENGTH)
        .saturating_add(1)
}

/// Returns the most bytes of a pack: `pack_size`, or fewer where fewer
/// arrive within [`LONGEST_SCR_INTERVAL`] at `program_mux_rate`
pub(in crate::ps::mux) fn largest_pack(pack_size: usize, program_mux_rate: u32) -> usize {
    pack_size.min(bytes_within_longest_scr_interval(program_mux_rate))
}

/// Returns the packets of a pack of a PES packet that carries `headers` bytes
/// of the system header and the program stream map
pub(super) const fn pes_pack_packets(headers: usize) -> usize {
    if headers > 0 {
        PES_PACKETS.saturating_add(HEADER_PACKETS)
    } else {
        PES_PACKETS
    }
}

/// A pack written: the time of the STC its SCR samples, before
/// [`MuxerConfig::mux_delay`](crate::ps::mux::MuxerConfig::mux_delay) is taken off,
/// its bytes, where its SCR is in the stream, and the packets it carries
#[derive(Debug, Clone, Copy)]
pub(in crate::ps::mux) struct Pack {
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
    // Why not tell from when the headers were last sent (`Headers`) whether
    // the pack carried the system header and the map, and count its packets
    // from that: a change of the map clears it, and the next SCR would
    // follow the map.
    packets: usize,
}

impl Pack {
    /// Returns the time of the STC the SCR of the pack samples, before
    /// `mux_delay` is taken off
    pub(super) const fn at(self) -> SystemTimeClock {
        self.at
    }

    /// Returns the bytes of the pack
    pub(in crate::ps::mux) const fn length(self) -> usize {
        self.length
    }

    /// Returns the bytes from the one that carries the last bit of the
    /// first SCR to the one that carries the last bit of the SCR of the pack
    pub(super) const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns NP of §2.7.9 of the pack
    pub(super) const fn packets(self) -> usize {
        self.packets
    }

    /// Returns the pack header of the pack, its SCR `mux_delay` before
    /// [`at`](Self::at), at `program_mux_rate`
    pub(in crate::ps::mux) fn pack_header(
        self,
        mux_delay: Duration,
        program_mux_rate: u32,
    ) -> PackHeader {
        PackHeader::new(self.at.to_clock_reference(mux_delay), program_mux_rate)
    }

    /// Returns the `offset` of the pack after it
    pub(super) fn next_offset(self) -> u64 {
        self.offset.saturating_add(as_u64(self.length))
    }
}

/// A pack to write: the pack, the bytes of the system header and the
/// program stream map it carries, and the fill past what it carries
#[derive(Debug, Clone)]
pub(in crate::ps::mux) struct PlannedPack {
    pub(in crate::ps::mux) pack: Pack,
    pub(in crate::ps::mux) headers: Range<usize>,
    pub(in crate::ps::mux) fill: Fill,
}

impl PlannedPack {
    /// Returns the pack of `length` bytes, its SCR at `at` and `offset`
    /// bytes from the first SCR, that carries `contents` bytes in `packets`
    /// packets, the `headers` bytes of the system header and the program
    /// stream map among them, and is filled past them, the packets of the
    /// fill counted with them (§2.7.9)
    pub(super) fn filled(
        at: SystemTimeClock,
        offset: u64,
        length: usize,
        contents: usize,
        packets: usize,
        headers: Range<usize>,
    ) -> Self {
        let fill = Fill::of(length.saturating_sub(contents));
        Self {
            pack: Pack {
                at,
                length,
                offset,
                packets: packets.saturating_add(fill.packets()),
            },
            headers,
            fill,
        }
    }

    /// Returns [`filled`](Self::filled) as the pack after `before`, or the
    /// first
    pub(super) fn after(
        before: Option<Pack>,
        at: SystemTimeClock,
        length: usize,
        contents: usize,
        packets: usize,
        headers: Range<usize>,
    ) -> Self {
        Self::filled(
            at,
            before.map_or(0, Pack::next_offset),
            length,
            contents,
            packets,
            headers,
        )
    }
}
