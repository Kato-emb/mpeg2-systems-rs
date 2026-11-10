//! When each pack is due: the SCRs of the packs a [`Muxer`](super::Muxer)
//! writes, and the packs that fill the time before a PES packet, of a pack
//! header alone over a leap of the time stamps or of padding at a fixed rate

use core::time::Duration;

use crate::pes;
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

/// Returns the most bytes that arrive within [`LONGEST_SCR_INTERVAL`] at
/// `program_mux_rate` (§2.5.2.2 equation 2-21), the longest pack after which
/// the next SCR can follow within it (§2.7.1)
pub(super) fn bytes_within_longest_scr_interval(program_mux_rate: u32) -> usize {
    // Why not the ticks the bytes take, rounded up as `Pack::arrived` does:
    // in whole units of program_mux_rate and of a millisecond the count is
    // exact, and that many bytes, rounded up to a tick, still arrive within
    // the interval.
    let bytes =
        bytes_per_second(program_mux_rate).saturating_mul(LONGEST_SCR_INTERVAL_MILLIS) / 1_000;
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// Returns ⌊c1 × `bytes`⌋ of ISO/IEC 13818-1 §2.5.3.6 equations 2-22 and
/// 2-23 at the fixed rate `program_mux_rate`, c1 the ticks of the STC a byte
/// takes to arrive (§2.5.2.2 equation 2-21), or [`u64::MAX`] past it
pub(super) fn line_ticks(bytes: u64, program_mux_rate: u32) -> u64 {
    // Why not in u64: the product passes it at the offsets a long stream
    // reaches.
    let ticks = u128::from(SYSTEM_CLOCK_FREQUENCY)
        .saturating_mul(u128::from(bytes))
        .div_euclid(u128::from(bytes_per_second(program_mux_rate)));
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

/// Returns the fewest bytes whose [`line_ticks`] are `ticks` or more
pub(super) fn line_bytes(ticks: u64, program_mux_rate: u32) -> u64 {
    let bytes = u128::from(ticks)
        .saturating_mul(u128::from(bytes_per_second(program_mux_rate)))
        .div_ceil(u128::from(SYSTEM_CLOCK_FREQUENCY));
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

/// A pack written: the time of the STC its SCR samples, before
/// [`MuxerConfig::mux_delay`](super::MuxerConfig::mux_delay) is taken off,
/// its bytes, and where its SCR is
/// in the stream
#[derive(Debug, Clone, Copy)]
pub(super) struct Pack {
    pub(super) at: SystemTimeClock,
    // Why not the time its last byte arrives: the SCR of the next pack
    // depends on this time, and the leap that needs filling on `at`.
    pub(super) length: usize,
    /// The bytes from the one that carries the last bit of the first SCR to
    /// the one that carries the last bit of the SCR of this pack
    // Why not a count of the bytes the muxer has written: the SCR of the
    // pack after it at a fixed rate follows from this pack alone, made with
    // it as one value.
    pub(super) offset: u64,
}

impl Pack {
    /// Returns the time the bytes of the pack have arrived at
    /// `program_mux_rate` (§2.5.2.2 equation 2-21), rounded up to a tick
    pub(super) fn arrived(self, program_mux_rate: u32) -> SystemTimeClock {
        let length = u64::try_from(self.length).unwrap_or(u64::MAX);
        // Why round up: an SCR before the last byte of the pack before has
        // arrived would have the next byte arrive before it.
        let ticks = SYSTEM_CLOCK_FREQUENCY
            .saturating_mul(length)
            .div_ceil(bytes_per_second(program_mux_rate));
        self.at.add_ticks(ticks)
    }

    /// Returns the `offset` of the pack after it
    pub(super) fn next_offset(self) -> u64 {
        self.offset
            .saturating_add(u64::try_from(self.length).unwrap_or(u64::MAX))
    }

    /// Returns the time on the line of a fixed rate through the SCR of the
    /// pack (§2.5.3.6 equations 2-22 and 2-23) `offset` bytes from the first
    /// SCR, which is not before the pack
    pub(super) fn on_line(self, offset: u64, program_mux_rate: u32) -> SystemTimeClock {
        // Why not the ticks of `offset - self.offset` alone: ⌊c1 × i⌋ is
        // taken of the whole offset, or each pack would round on its own and
        // drift from the line.
        self.at.add_ticks(
            line_ticks(offset, program_mux_rate)
                .saturating_sub(line_ticks(self.offset, program_mux_rate)),
        )
    }
}

/// The packs that fill the time from the pack after `last` to a PES packet
#[derive(Debug, Clone, Copy)]
pub(super) enum Gap {
    /// `count` packs of a pack header alone over a leap of the time stamps,
    /// [`LONGEST_SCR_INTERVAL`] apart from `first` (§2.7.1)
    Filling {
        last: Pack,
        first: SystemTimeClock,
        count: u64,
    },
    /// `count` packs of padding at a fixed rate, each `length` bytes but the
    /// last, of `last_length`, on the line through `last` (§2.5.3.6)
    Padding {
        last: Pack,
        program_mux_rate: u32,
        count: u64,
        length: usize,
        last_length: usize,
    },
}

impl Gap {
    /// Returns the packs of a pack header alone that fill a leap from
    /// `last`, the last pack written, to a PES packet due at `time`, at the
    /// variable rate `program_mux_rate`: each [`LONGEST_SCR_INTERVAL`] after
    /// the SCR before it, or once the bytes before it have arrived if that is
    /// later, while the pack at `time` would be more than
    /// [`LONGEST_SCR_INTERVAL`] after the SCR before it; `None` where there
    /// is no leap to fill
    pub(super) fn filling(
        last: Pack,
        time: SystemTimeClock,
        program_mux_rate: u32,
    ) -> Option<Self> {
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
        Some(Self::Filling { last, first, count })
    }

    /// Returns the packs of padding, each at most `longest` bytes and the
    /// last at least a pack header, that bring the line of the fixed rate
    /// `program_mux_rate` through `last`, the last pack written, to a PES
    /// packet due at `time`; `None` where the line reaches it already
    pub(super) fn padding(
        last: Pack,
        time: SystemTimeClock,
        program_mux_rate: u32,
        longest: usize,
    ) -> Option<Self> {
        let reached = line_bytes(
            line_ticks(last.offset, program_mux_rate).saturating_add(time.ticks_since(last.at)),
            program_mux_rate,
        );
        let bytes = reached
            .checked_sub(last.next_offset())
            .filter(|&bytes| bytes > 0)?;
        let longest_bytes = u64::try_from(longest).unwrap_or(u64::MAX);
        let count = bytes.div_ceil(longest_bytes);
        let last_length =
            bytes.saturating_sub(count.saturating_sub(1).saturating_mul(longest_bytes));
        Some(Self::Padding {
            last,
            program_mux_rate,
            count,
            length: longest,
            last_length: usize::try_from(last_length)
                .unwrap_or(usize::MAX)
                .max(pack_header::FIXED_LENGTH),
        })
    }

    /// Returns the bytes of all the packs
    pub(super) fn bytes(self) -> u64 {
        match self {
            Self::Filling { count, .. } => {
                count.saturating_mul(u64::try_from(pack_header::FIXED_LENGTH).unwrap_or(u64::MAX))
            }
            Self::Padding {
                count,
                length,
                last_length,
                ..
            } => u64::try_from(length)
                .unwrap_or(u64::MAX)
                .saturating_mul(count.saturating_sub(1))
                .saturating_add(u64::try_from(last_length).unwrap_or(u64::MAX)),
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
            Self::Filling { count, .. } | Self::Padding { count, .. } => count,
        }
    }

    /// Returns the pack at `index`
    fn pack(self, index: u64) -> Pack {
        match self {
            Self::Filling { last, first, .. } => {
                Pack {
                    at: first.add_ticks(index.saturating_mul(longest_scr_interval())),
                    length: pack_header::FIXED_LENGTH,
                    offset: last.next_offset().saturating_add(index.saturating_mul(
                        u64::try_from(pack_header::FIXED_LENGTH).unwrap_or(u64::MAX),
                    )),
                }
            }
            Self::Padding {
                last,
                program_mux_rate,
                count,
                length,
                last_length,
            } => {
                let offset = last.next_offset().saturating_add(
                    index.saturating_mul(u64::try_from(length).unwrap_or(u64::MAX)),
                );
                Pack {
                    at: last.on_line(offset, program_mux_rate),
                    length: if index.saturating_add(1) == count {
                        last_length
                    } else {
                        length
                    },
                    offset,
                }
            }
        }
    }
}

/// What fills a pack of a [`Gap`] after its pack header
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PackFill {
    /// `pack_stuffing_length` `stuffing_byte`s (§2.5.3.4)
    Stuffing(u8),
    /// A PES packet of `padding_stream` of this `PES_packet_length`
    /// (§2.4.3.6, §2.4.3.7)
    PaddingPacket(u16),
}

/// Returns what fills a pack of `length` bytes after its pack header:
/// `stuffing_byte`s up to 7 of them, and past that a PES packet of
/// `padding_stream`, which is no shorter than 7 bytes
pub(super) fn pack_fill(length: usize) -> PackFill {
    let rest = length.saturating_sub(pack_header::FIXED_LENGTH);
    match u8::try_from(rest) {
        Ok(stuffing) if stuffing <= pack_header::MAX_PACK_STUFFING_LENGTH => {
            PackFill::Stuffing(stuffing)
        }
        // Why not an error past 16 bits: the muxer asks `Gap::padding` for
        // no pack longer than a pack header and the longest PES packet.
        _ => PackFill::PaddingPacket(
            u16::try_from(rest.saturating_sub(pes::PREFIX_LENGTH)).unwrap_or(u16::MAX),
        ),
    }
}
