//! The writer of the elements of a Program Stream, one element at a time

use alloc::vec::Vec;

use super::Error;
use crate::field::write_or_truncate;
use crate::pes::{self, OptionalHeader, PesPacket};
use crate::ps::{MPEG_PROGRAM_END_CODE, PackHeader, ProgramStreamMap, SystemHeader};

/// Writer of the elements of a Program Stream, which appends each pack
/// header, system header, program stream map, PES packet or end code, in the
/// order given, to a buffer the caller owns
///
/// Each method appends one element, or the PES packets of one payload, and a
/// write that fails leaves the buffer as it was. The writer does not decide
/// where a pack starts, the SCR or the multiplex rate, or the bounds of a
/// system header.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct PackWriter;

impl PackWriter {
    /// Returns a writer
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

    /// Appends `header`, as the caller has built it, to `out`
    ///
    /// The writer does not work out the bounds the header states (`rate_bound`,
    /// `audio_bound`, the buffer size of each stream); they are the caller's.
    ///
    /// # Errors
    ///
    /// * [`SystemHeader`](Error::SystemHeader): [`SystemHeader::write`]
    ///   refuses the header.
    pub fn write_system_header(
        &self,
        header: &SystemHeader<'_>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        header
            .write(out)
            .map_err(|error| Error::SystemHeader { error })
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
        if !stream_id.is_pes_packet() {
            return Err(Error::StreamId { stream_id });
        }
        // Why not split a packet of an unspecified length into more of the
        // same: PES_packet_length 0 is allowed only in a Transport Stream
        // (§2.4.3.7).
        if packet.length_unspecified {
            return Err(Error::LengthUnspecified { stream_id });
        }
        let mut written: usize = 0;
        write_or_truncate(out, |out| {
            for piece in
                PesPacketPieces::new(packet, pes::LONGEST_PES_PACKET, pes::LONGEST_PES_PACKET)
            {
                piece
                    .write(out)
                    .map_err(|error| Error::PesPacket { error })?;
                written = written.saturating_add(1);
            }
            Ok(())
        })?;
        Ok(written)
    }

    /// Appends `MPEG_program_end_code`, which ends the Program Stream, to
    /// `out`
    pub fn write_program_end_code(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MPEG_PROGRAM_END_CODE);
    }
}

/// The PES packets a payload is split into, each no longer than a length
/// given: the first with the optional header of the packet, the ones after
/// it with the same header without its times (PTS, DTS and ESCR), which
/// belong to the start of the payload
///
/// An empty payload still gives one packet, and the payloads of the packets
/// join back into the payload in order.
#[derive(Debug, Clone)]
pub(crate) struct PesPacketPieces<'packet> {
    /// The header of the next packet and the payload left, `None` once the
    /// last packet is given
    rest: Option<PesPacket<'packet>>,
    continued_header: Option<OptionalHeader<'packet>>,
    /// The most bytes the next packet may take
    longest: usize,
    /// The most bytes each packet after the first may take
    longest_continued: usize,
}

impl<'packet> PesPacketPieces<'packet> {
    /// Returns the packets of `packet`, the first at most `longest` bytes
    /// and each after it at most `longest_continued`
    pub(crate) fn new(
        packet: &PesPacket<'packet>,
        longest: usize,
        longest_continued: usize,
    ) -> Self {
        let continued_header = packet.optional_header.clone().map(|mut header| {
            header.pts = None;
            header.dts = None;
            header.escr = None;
            header
        });
        Self {
            rest: Some(packet.clone()),
            continued_header,
            longest,
            longest_continued,
        }
    }
}

impl<'packet> Iterator for PesPacketPieces<'packet> {
    type Item = PesPacket<'packet>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.rest.as_mut()?;
        let header_length = pes::PREFIX_LENGTH.saturating_add(
            rest.optional_header
                .as_ref()
                .map_or(0, OptionalHeader::encoded_len),
        );
        // Why not a capacity of 0 when the header takes all the length: the
        // payload would never shrink. The caller gives lengths longer than
        // the headers, so the floor of 1 is never reached.
        let capacity = self.longest.saturating_sub(header_length).max(1);
        let (chunk, after) = rest.payload.split_at(rest.payload.len().min(capacity));
        let mut piece = rest.clone();
        piece.payload = chunk;
        if after.is_empty() {
            self.rest = None;
        } else {
            rest.payload = after;
            rest.optional_header.clone_from(&self.continued_header);
            self.longest = self.longest_continued;
        }
        Some(piece)
    }
}

#[cfg(test)]
mod tests;
