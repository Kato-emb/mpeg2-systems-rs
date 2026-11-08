//! Writing a Program Stream: pack headers, program stream maps, PES packets
//! split to fit, and the end code
//!
//! A Program Stream is written element by element into a buffer the caller
//! owns, in the order ISO/IEC 13818-1 §2.5.3 lays out: a pack header, then
//! the program stream map and the PES packets of the pack, then the next pack
//! header, and so on, and `MPEG_program_end_code` at the end. [`Muxer`]
//! writes each element. It holds no state, and is a type so that a Program
//! Stream is written the way a Transport Stream is, through
//! `ts::mux::Muxer`, which keeps the continuity counters, as the two
//! demuxers are driven by methods of the same names. Where a pack starts is
//! up to the caller. The crate does not write a system header, a program
//! stream directory, padding or pack stuffing.
//!
//! ```
//! use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
//! use mpeg2_systems::ps::demux::{Demuxer, Event};
//! use mpeg2_systems::ps::mux::Muxer;
//! use mpeg2_systems::ps::{PackHeader, ProgramStreamMap};
//! use mpeg2_systems::psi::VersionNumber;
//! use mpeg2_systems::time::{ClockReference, Timestamp};
//!
//! let muxer = Muxer::new();
//! let mut stream = Vec::new();
//! // A pack: its header, the program stream map, and an access unit too long
//! // for one PES packet
//! let pack_header = PackHeader::new(ClockReference::new(0, 0)?, 2_000);
//! muxer.write_pack_header(&pack_header, &mut stream)?;
//! let map = ProgramStreamMap::new(VersionNumber::try_from(0)?);
//! muxer.write_program_stream_map(&map, &mut stream)?;
//! let access_unit = vec![0xaa; 100_000];
//! let mut header = OptionalHeader::new();
//! header.pts = Some(Timestamp::try_from(90_000)?);
//! let mut packet = PesPacket::new(StreamId::from(0xe0), &access_unit);
//! packet.optional_header = Some(header);
//! assert_eq!(muxer.write_pes_packets(&packet, &mut stream)?, 2);
//! // The end of the Program Stream
//! muxer.write_program_end_code(&mut stream);
//!
//! // Read back, the payloads of the PES packets join into the access unit
//! let mut demuxer = Demuxer::new();
//! demuxer.handle_input(&stream);
//! demuxer.finish();
//! let mut payload = Vec::new();
//! while let Some(event) = demuxer.poll_event() {
//!     if let Event::PesPacket(packet) = event {
//!         payload.extend_from_slice(packet.payload);
//!     }
//! }
//! assert_eq!(payload, access_unit);
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

use alloc::vec::Vec;
use core::fmt;

use crate::field::write_or_truncate;
use crate::pes::{self, OptionalHeader, PesPacket, StreamId};
use crate::ps::{MPEG_PROGRAM_END_CODE, PackHeader, ProgramStreamMap, pack_header, psm};

/// Failure to write an element of a Program Stream
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The `stream_id` given is not that of a PES packet: below 0xBD it is
    /// the start code of another element, such as a pack header or a
    /// program stream map
    StreamId {
        /// The `stream_id` given
        stream_id: StreamId,
    },
    /// The PES packet has `length_unspecified` set, which a Program Stream
    /// does not allow (ISO/IEC 13818-1 §2.4.3.7)
    LengthUnspecified {
        /// The `stream_id` of the packet
        stream_id: StreamId,
    },
    /// A pack header could not be written
    PackHeader {
        /// How the pack header failed
        error: pack_header::Error,
    },
    /// A program stream map could not be written
    ProgramStreamMap {
        /// How the program stream map failed
        error: psm::Error,
    },
    /// A PES packet could not be written
    PesPacket {
        /// How the PES packet failed
        error: pes::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamId { stream_id } => write!(
                formatter,
                "stream_id {:#04x} is not that of a PES packet",
                stream_id.get()
            ),
            Self::LengthUnspecified { stream_id } => write!(
                formatter,
                "a PES packet of stream_id {:#04x} leaves its length unspecified, which a \
                 program stream does not allow",
                stream_id.get()
            ),
            Self::PackHeader { .. } => write!(formatter, "a pack header could not be written"),
            Self::ProgramStreamMap { .. } => {
                write!(formatter, "a program stream map could not be written")
            }
            Self::PesPacket { .. } => write!(formatter, "a PES packet could not be written"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::PackHeader { error } => Some(error),
            Self::ProgramStreamMap { error } => Some(error),
            Self::PesPacket { error } => Some(error),
            Self::StreamId { .. } | Self::LengthUnspecified { .. } => None,
        }
    }
}

/// Program Stream muxer, which writes the elements of a Program Stream into
/// a buffer the caller owns
///
/// Each method appends one element, or the PES packets of one payload, and a
/// write that fails leaves the buffer as it was. The muxer does not decide
/// where a pack starts, nor write a system header; that is up to the caller.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct Muxer;

impl Muxer {
    /// Returns a muxer
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Appends `header` to `out`
    ///
    /// # Errors
    ///
    /// * [`PackHeader`](Error::PackHeader): [`PackHeader::write`] refuses
    ///   the header.
    pub fn write_pack_header(&self, header: &PackHeader, out: &mut Vec<u8>) -> Result<(), Error> {
        header
            .write(out)
            .map_err(|error| Error::PackHeader { error })
    }

    /// Appends `map` to `out`
    ///
    /// # Errors
    ///
    /// * [`ProgramStreamMap`](Error::ProgramStreamMap):
    ///   [`ProgramStreamMap::write`] refuses the map.
    pub fn write_program_stream_map(
        &self,
        map: &ProgramStreamMap<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        map.write(out)
            .map_err(|error| Error::ProgramStreamMap { error })
    }

    /// Appends `packet` to `out` as as many PES packets as a 16-bit
    /// `PES_packet_length` needs for its payload, and returns how many it
    /// wrote
    ///
    /// PES packet, ISO/IEC 13818-1 §2.4.3.6
    ///
    /// The first packet has the optional header given, as it is; the ones
    /// after it have the same header without its times (PTS, DTS and ESCR),
    /// which belong to the start of the payload. The payloads of the packets
    /// join back into `packet.payload` in order. An empty payload still
    /// writes one packet, so the timestamps can travel on their own. A write
    /// that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`StreamId`](Error::StreamId): the `stream_id` is below 0xBD, not
    ///   that of a PES packet.
    /// * [`LengthUnspecified`](Error::LengthUnspecified): `length_unspecified`
    ///   is set.
    /// * [`PesPacket`](Error::PesPacket): [`PesPacket::write`] refuses a
    ///   packet, as it does a DTS without a PTS, an optional header on a
    ///   stream that has none, or a stream other than video with an empty
    ///   payload.
    pub fn write_pes_packets(
        &self,
        packet: &PesPacket<'_>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let stream_id = packet.stream_id;
        // Why compare with private_stream_1: it is the lowest stream_id of a
        // PES packet, and those below it are the start codes of other
        // elements (Table 2-22).
        if stream_id.get() < StreamId::PRIVATE_STREAM_1.get() {
            return Err(Error::StreamId { stream_id });
        }
        // Why not split a packet of an unspecified length into more of the
        // same: PES_packet_length 0 is allowed only in a Transport Stream
        // (§2.4.3.7).
        if packet.length_unspecified {
            return Err(Error::LengthUnspecified { stream_id });
        }
        let continued_header = packet.optional_header.clone().map(|mut header| {
            header.pts = None;
            header.dts = None;
            header.escr = None;
            header
        });
        let mut written: usize = 0;
        write_or_truncate(out, |out| {
            let mut piece = packet.clone();
            let mut rest = packet.payload;
            loop {
                let capacity = usize::from(u16::MAX).saturating_sub(
                    piece
                        .optional_header
                        .as_ref()
                        .map_or(0, OptionalHeader::encoded_len),
                );
                let (chunk, after) = rest.split_at(rest.len().min(capacity));
                piece.payload = chunk;
                piece
                    .write(out)
                    .map_err(|error| Error::PesPacket { error })?;
                written = written.saturating_add(1);
                if after.is_empty() {
                    return Ok(());
                }
                rest = after;
                piece.optional_header.clone_from(&continued_header);
            }
        })?;
        Ok(written)
    }

    /// Appends `MPEG_program_end_code`, which ends the Program Stream, to
    /// `out`
    pub fn write_program_end_code(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MPEG_PROGRAM_END_CODE);
    }
}

#[cfg(test)]
mod tests;
