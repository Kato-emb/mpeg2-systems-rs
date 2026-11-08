//! Writing a Program Stream: the PES packets of a payload, split to fit
//!
//! A Program Stream is written element by element into a buffer the caller
//! owns, in the order ISO/IEC 13818-1 §2.5.3 lays out: a pack header, then
//! the program stream map and the PES packets of the pack, then the next pack
//! header, and so on, and `MPEG_program_end_code` at the end. The pack
//! header, the program stream map and the end code are written by their own
//! types; this module writes the PES packets, which a payload longer than one
//! packet can hold needs several of. Where a pack starts is up to the caller.
//! The crate does not write a system header, a program stream directory,
//! padding or pack stuffing.
//!
//! ```
//! use mpeg2_systems::pes::StreamId;
//! use mpeg2_systems::ps::demux::{Demuxer, Event};
//! use mpeg2_systems::ps::mux::write_pes_packets;
//! use mpeg2_systems::ps::psm::ProgramStreamMap;
//! use mpeg2_systems::ps::{MPEG_PROGRAM_END_CODE, PackHeader};
//! use mpeg2_systems::psi::VersionNumber;
//! use mpeg2_systems::time::{ClockReference, Timestamp};
//!
//! let mut stream = Vec::new();
//! // A pack: its header, the program stream map, and the PES packets of an
//! // access unit too long for one packet
//! PackHeader::new(ClockReference::new(0, 0)?, 2_000).write(&mut stream)?;
//! ProgramStreamMap::new(VersionNumber::try_from(0)?).write(&mut stream)?;
//! let video = StreamId::from(0xe0);
//! let access_unit = vec![0xaa; 100_000];
//! let pts = Timestamp::try_from(90_000)?;
//! let packets = write_pes_packets(video, Some(pts), None, &access_unit, &mut stream)?;
//! assert_eq!(packets, 2);
//! // The end of the Program Stream
//! stream.extend_from_slice(&MPEG_PROGRAM_END_CODE);
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
use crate::time::Timestamp;

/// The lowest `stream_id` of a PES packet; those below it are the start
/// codes of other elements, ISO/IEC 13818-1 Table 2-22
const FIRST_PES_STREAM_ID: u8 = 0xBD;

/// The most bytes `PES_packet_length` can state after itself
const PES_PACKET_LENGTH_MAX: usize = 0xFFFF;

/// Failure to write the PES packets of a payload
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// A DTS was given without a PTS, which `PTS_DTS_flags` `'01'` would
    /// state and ISO/IEC 13818-1 §2.4.3.7 forbids
    DtsWithoutPts,
    /// A timestamp was given for a stream whose PES packets have no optional
    /// header to carry it
    TimestampsOnStreamWithoutOptionalHeader {
        /// The `stream_id` given
        stream_id: StreamId,
    },
    /// The `stream_id` given is not that of a PES packet: below 0xBD it is
    /// the start code of another element, such as a pack header or a
    /// program stream map
    StreamId {
        /// The `stream_id` given
        stream_id: StreamId,
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
            Self::DtsWithoutPts => write!(formatter, "a DTS is given without a PTS"),
            Self::TimestampsOnStreamWithoutOptionalHeader { stream_id } => write!(
                formatter,
                "stream_id {:#04x} has no optional PES header to carry a timestamp",
                stream_id.get()
            ),
            Self::StreamId { stream_id } => write!(
                formatter,
                "stream_id {:#04x} is not that of a PES packet",
                stream_id.get()
            ),
            Self::PesPacket { .. } => write!(formatter, "a PES packet could not be written"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::PesPacket { error } => Some(error),
            Self::DtsWithoutPts
            | Self::TimestampsOnStreamWithoutOptionalHeader { .. }
            | Self::StreamId { .. } => None,
        }
    }
}

/// Appends `payload` to `out` as the PES packets of `stream_id`, as many as
/// a 16-bit `PES_packet_length` needs, and returns how many it wrote
///
/// PES packet, ISO/IEC 13818-1 §2.4.3.6
///
/// The timestamps go in the first packet only, and the payloads of the
/// packets join back into `payload` in order. An empty `payload` still
/// writes one packet, so the timestamps can travel on their own. A stream
/// whose packets have an optional header gets one with `PTS_DTS_flags` from
/// `pts` and `dts` and every other field at its default,
/// `data_alignment_indicator` included; a stream without one gets the
/// payload alone. The function does not choose where a pack starts, nor
/// write a pack header; that is up to the caller. A write that fails leaves
/// `out` as it was.
///
/// # Errors
///
/// * [`DtsWithoutPts`](Error::DtsWithoutPts): `dts` is given without `pts`.
/// * [`TimestampsOnStreamWithoutOptionalHeader`](Error::TimestampsOnStreamWithoutOptionalHeader):
///   a timestamp is given for a stream whose packets have no optional header.
/// * [`StreamId`](Error::StreamId): `stream_id` is below 0xBD, not that of a
///   PES packet.
/// * [`PesPacket`](Error::PesPacket): a packet could not be written, such as
///   one of a stream other than video with an empty `payload`, which would
///   leave its length unspecified.
pub fn write_pes_packets(
    stream_id: StreamId,
    pts: Option<Timestamp>,
    dts: Option<Timestamp>,
    payload: &[u8],
    out: &mut Vec<u8>,
) -> Result<usize, Error> {
    if stream_id.get() < FIRST_PES_STREAM_ID {
        return Err(Error::StreamId { stream_id });
    }
    if pts.is_none() && dts.is_some() {
        return Err(Error::DtsWithoutPts);
    }
    let timestamped = pts.is_some();
    if timestamped && !stream_id.carries_optional_header() {
        return Err(Error::TimestampsOnStreamWithoutOptionalHeader { stream_id });
    }
    // Why not set data_alignment_indicator: only the first packet of a split
    // payload starts where an access unit does, and nothing given here says
    // that the payload starts at one.
    let header = |pts, dts| {
        stream_id.carries_optional_header().then(|| {
            let mut header = OptionalHeader::new();
            header.pts = pts;
            header.dts = dts;
            header
        })
    };
    let mut written: usize = 0;
    write_or_truncate(out, |out| {
        let mut optional_header = header(pts, dts);
        let mut rest = payload;
        loop {
            // Why split even a video payload rather than leave its length
            // unspecified: PES_packet_length 0 is allowed only in a
            // Transport Stream (§2.4.3.7).
            let capacity = PES_PACKET_LENGTH_MAX.saturating_sub(
                optional_header
                    .as_ref()
                    .map_or(0, OptionalHeader::encoded_len),
            );
            let (chunk, after) = rest.split_at(rest.len().min(capacity));
            let mut packet = PesPacket::new(stream_id, chunk);
            packet.optional_header = optional_header;
            packet
                .write(out)
                .map_err(|error| Error::PesPacket { error })?;
            written = written.saturating_add(1);
            if after.is_empty() {
                return Ok(());
            }
            rest = after;
            optional_header = header(None, None);
        }
    })?;
    Ok(written)
}

#[cfg(test)]
mod tests;
