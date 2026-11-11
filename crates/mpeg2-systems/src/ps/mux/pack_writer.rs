//! The writer of the elements of a Program Stream, one element at a time

use alloc::vec::Vec;

use super::Error;
use crate::field::write_or_truncate;
use crate::pes::{self, OptionalHeader, PesPacket, StreamId};
use crate::ps::{MPEG_PROGRAM_END_CODE, PackHeader, ProgramStreamMap, SystemHeader, pack_header};

/// Writer of the elements of a Program Stream, which appends each pack
/// header, system header, program stream map, PES packet or end code, in the
/// order given, to a buffer the caller owns
///
/// Each method appends one element, or the PES packets of one payload, and a
/// write that fails leaves the buffer as it was. The writer does not decide
/// where a pack starts, the SCR or the multiplex rate, or the bounds of a
/// system header.
///
/// Padding and pack stuffing go through the same methods: the
/// `stuffing_byte`s after a pack header are its `pack_stuffing_length`, 0 to
/// 7 (ISO/IEC 13818-1 §2.5.3.4, [`PackHeader::with_pack_stuffing_length`]),
/// and padding is the PES packets of `padding_stream`
/// ([`StreamId::PADDING_STREAM`]) given to
/// [`write_pes_packets`](Self::write_pes_packets), whose payload is written
/// as given as the `padding_byte`s, 0xFF (§2.4.3.6, §2.4.3.7).
///
/// ```
/// use mpeg2_systems::pes::{PesPacket, StreamId};
/// use mpeg2_systems::ps::PackHeader;
/// use mpeg2_systems::ps::demux::{Demuxer, Event};
/// use mpeg2_systems::ps::mux::PackWriter;
/// use mpeg2_systems::time::ClockReference;
///
/// let writer = PackWriter::new();
/// let mut stream = Vec::new();
/// // A pack header with 7 stuffing bytes, then 100 bytes of padding
/// let header = PackHeader::new(ClockReference::new(0, 0)?, 2_000).with_pack_stuffing_length(7);
/// writer.write_pack_header(&header, &mut stream)?;
/// let padding = [0xff; 94];
/// writer.write_pes_packets(&PesPacket::new(StreamId::PADDING_STREAM, &padding), &mut stream)?;
/// assert_eq!(stream.len(), 14 + 7 + 100);
///
/// // Read back
/// let mut demuxer = Demuxer::new();
/// demuxer.handle_input(&stream);
/// demuxer.finish();
/// let mut read = Vec::new();
/// while let Some(event) = demuxer.poll_event() {
///     match event {
///         Event::PackHeader(header) => read.push(usize::from(header.pack_stuffing_length)),
///         Event::PesPacket(packet) => read.push(packet.payload.len()),
///         _ => {}
///     }
/// }
/// assert_eq!(read, [7, 94]);
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
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
    /// after it have the same header without its PTS and DTS, the
    /// presentation and decoding times of an access unit, and its ESCR, the
    /// time a byte of the packet is due at the input of the PES-STD
    /// (§2.4.3.7). The payloads of the packets
    /// join back into `packet.payload` in order. An empty payload still
    /// writes one packet, so the timestamps can travel on their own. A write
    /// that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`StreamId`](Error::StreamId): the `stream_id` is below 0xBD, which
    ///   this writer does not take: 0xBC, the `program_stream_map`, is
    ///   written with [`write_program_stream_map`](Self::write_program_stream_map).
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
        if !stream_id.is_pes_packet() || stream_id == StreamId::PROGRAM_STREAM_MAP {
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

    /// Appends a pack of `length` bytes that carries nothing but padding:
    /// `header` with `stuffing_byte`s where 7 or fewer fill the pack
    /// (§2.5.3.4), or past that `header` without them and PES packets of
    /// `padding_stream` (§2.4.3.6, §2.4.3.7), of as near the same length as
    /// they can be
    ///
    /// # Errors
    ///
    /// * [`PackHeader`](Error::PackHeader): [`PackHeader::write`] refuses
    ///   `header`.
    pub(super) fn write_padding_pack(
        &self,
        header: &PackHeader,
        length: usize,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let fill = Fill::of(length.saturating_sub(pack_header::FIXED_LENGTH));
        self.write_pack_header(
            &header.with_pack_stuffing_length(fill.pack_stuffing_length()),
            out,
        )?;
        fill.write_padding(out);
        Ok(())
    }

    /// Appends `MPEG_program_end_code`, which ends the Program Stream, to
    /// `out`
    pub fn write_program_end_code(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MPEG_PROGRAM_END_CODE);
    }
}

/// The bytes a pack takes past what it carries: `stuffing_byte`s after its
/// pack header where they are 7 or fewer (§2.5.3.4), or past that PES packets
/// of `padding_stream` (§2.4.3.6, §2.4.3.7), of as near the same length as
/// they can be
#[derive(Debug, Clone, Copy)]
pub(super) struct Fill(FillKind);

/// How a [`Fill`] takes its bytes
#[derive(Debug, Clone, Copy)]
enum FillKind {
    Stuffing(u8),
    Padding(usize),
}

impl Fill {
    /// Returns the fill of `bytes`
    pub(super) fn of(bytes: usize) -> Self {
        Self(match u8::try_from(bytes) {
            Ok(stuffing) if stuffing <= pack_header::MAX_PACK_STUFFING_LENGTH => {
                FillKind::Stuffing(stuffing)
            }
            _ => FillKind::Padding(bytes),
        })
    }

    /// Returns the `pack_stuffing_length` of the pack header
    pub(super) const fn pack_stuffing_length(self) -> u8 {
        match self.0 {
            FillKind::Stuffing(stuffing) => stuffing,
            FillKind::Padding(_) => 0,
        }
    }

    /// Returns how many PES packets of padding there are
    pub(super) fn packets(self) -> usize {
        match self.0 {
            FillKind::Stuffing(_) => 0,
            FillKind::Padding(bytes) => bytes.div_ceil(pes::LONGEST_PES_PACKET),
        }
    }

    /// Appends the PES packets of padding to `out`
    pub(super) fn write_padding(self, out: &mut Vec<u8>) {
        let FillKind::Padding(bytes) = self.0 else {
            return;
        };
        // Why as many as the longest PES packet needs, split evenly: past 7
        // bytes of padding each packet gets at least 7, the shortest a
        // padding packet of a Program Stream can be.
        let count = self.packets();
        let share = bytes.checked_div(count).unwrap_or(bytes);
        let longer = bytes.checked_rem(count).unwrap_or(0);
        for index in 0..count {
            let share = share.saturating_add(usize::from(index < longer));
            let pes_packet_length = share.saturating_sub(pes::PREFIX_LENGTH);
            pes::write_padding_packet(u16::try_from(pes_packet_length).unwrap_or(u16::MAX), out);
        }
    }
}

/// The PES packets a payload is split into, each no longer than a length
/// given: the first with the optional header of the packet, the ones after
/// it with the same header without its PTS, DTS and ESCR
///
/// An empty payload still gives one packet, and the payloads of the packets
/// join back into the payload in order. How many packets is known before
/// they are cut.
#[derive(Debug, Clone)]
pub(crate) struct PesPacketPieces<'packet> {
    /// The header of the next packet and the payload left, `None` once the
    /// last packet is given
    rest: Option<PesPacket<'packet>>,
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
        Self {
            rest: Some(packet.clone()),
            longest,
            longest_continued,
        }
    }

    /// Returns how many packets are left, from the lengths of the headers
    /// and the payload alone
    fn count(&self) -> usize {
        let Some(rest) = &self.rest else {
            return 0;
        };
        let first_header = header_length(rest);
        let after = rest
            .payload
            .len()
            .saturating_sub(capacity(self.longest, first_header));
        if after == 0 {
            return 1;
        }
        let mut continued = rest.clone();
        if let Some(header) = continued.optional_header.as_mut() {
            clear_pts_dts_and_escr(header);
        }
        let continued_header = header_length(&continued);
        after
            .div_ceil(capacity(self.longest_continued, continued_header))
            .saturating_add(1)
    }
}

/// Returns the bytes of the header of `packet`, up to its payload
fn header_length(packet: &PesPacket<'_>) -> usize {
    pes::PREFIX_LENGTH.saturating_add(
        packet
            .optional_header
            .as_ref()
            .map_or(0, OptionalHeader::encoded_len),
    )
}

/// Returns the payload a packet of at most `longest` bytes holds after a
/// header of `header_length`
fn capacity(longest: usize, header_length: usize) -> usize {
    // Why not a capacity of 0 when the header takes all the length: the
    // payload would never shrink. The caller gives lengths longer than the
    // headers, so the floor of 1 is never reached.
    longest.saturating_sub(header_length).max(1)
}

/// Takes the PTS, the DTS and the ESCR off `header`
fn clear_pts_dts_and_escr(header: &mut OptionalHeader<'_>) {
    header.pts = None;
    header.dts = None;
    header.escr = None;
}

impl<'packet> Iterator for PesPacketPieces<'packet> {
    type Item = PesPacket<'packet>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.rest.as_mut()?;
        let capacity = capacity(self.longest, header_length(rest));
        let (chunk, after) = rest.payload.split_at(rest.payload.len().min(capacity));
        let mut piece = rest.clone();
        piece.payload = chunk;
        if after.is_empty() {
            self.rest = None;
        } else {
            rest.payload = after;
            if let Some(header) = rest.optional_header.as_mut() {
                clear_pts_dts_and_escr(header);
            }
            self.longest = self.longest_continued;
        }
        Some(piece)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.count();
        (count, Some(count))
    }
}

impl ExactSizeIterator for PesPacketPieces<'_> {}

#[cfg(test)]
mod tests;
