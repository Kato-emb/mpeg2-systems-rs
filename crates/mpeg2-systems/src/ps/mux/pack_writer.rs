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
        if !is_written_in_pes_packets(stream_id) {
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

    /// Appends a pack to `out`: `header` with the `stuffing_byte`s of
    /// `fill`, `headers`, the bytes of a system header and a program stream
    /// map as written, `piece` where it carries a PES packet, then the PES
    /// packets of padding of `fill`
    ///
    /// # Errors
    ///
    /// * [`PackHeader`](Error::PackHeader): [`PackHeader::write`] refuses
    ///   `header`.
    /// * Those of [`write_pes_packets`](Self::write_pes_packets) for `piece`.
    pub(super) fn write_pack(
        &self,
        header: &PackHeader,
        headers: &[u8],
        piece: Option<&PesPacket<'_>>,
        fill: Fill,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        self.write_pack_header(
            &header.with_pack_stuffing_length(fill.pack_stuffing_length()),
            out,
        )?;
        out.extend_from_slice(headers);
        if let Some(piece) = piece {
            self.write_pes_packets(piece, out)?;
        }
        fill.write_padding(out);
        Ok(())
    }

    /// Appends `MPEG_program_end_code`, which ends the Program Stream, to
    /// `out`
    pub fn write_program_end_code(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&MPEG_PROGRAM_END_CODE);
    }
}

/// Returns whether [`PackWriter::write_pes_packets`] writes PES packets of
/// `stream_id`: those of 0xBD and above, the PES packets but the
/// `program_stream_map`
pub(super) fn is_written_in_pes_packets(stream_id: StreamId) -> bool {
    stream_id.is_pes_packet() && stream_id != StreamId::PROGRAM_STREAM_MAP
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
    const fn pack_stuffing_length(self) -> u8 {
        match self.0 {
            FillKind::Stuffing(stuffing) => stuffing,
            FillKind::Padding(_) => 0,
        }
    }

    /// Returns the bytes of the fill after what the pack carries: those of
    /// its PES packets of padding, none for stuffing in the pack header
    pub(super) const fn trailing(self) -> usize {
        match self.0 {
            FillKind::Stuffing(_) => 0,
            FillKind::Padding(bytes) => bytes,
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
    fn write_padding(self, out: &mut Vec<u8>) {
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
/// given and than the longest PES packet: the first with the optional header
/// of the packet, the ones after it with the same header without its PTS,
/// DTS and ESCR
///
/// An empty payload still gives one packet, and the payloads of the packets
/// join back into the payload in order. How many packets is known before
/// they are cut.
#[derive(Debug, Clone)]
pub(super) struct PesPacketPieces<'packet> {
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
    /// and each after it at most `longest_continued`, none past the longest
    /// PES packet
    pub(super) fn new(
        packet: &PesPacket<'packet>,
        longest: usize,
        longest_continued: usize,
    ) -> Self {
        Self {
            rest: Some(packet.clone()),
            longest: longest.min(pes::LONGEST_PES_PACKET),
            longest_continued: longest_continued.min(pes::LONGEST_PES_PACKET),
        }
    }

    /// Returns the fewest bytes the next packet takes: its header and a byte
    /// of the payload left, the header alone where none is left
    pub(super) fn shortest_next(&self) -> usize {
        self.rest.as_ref().map_or(0, |rest| {
            header_length(rest).saturating_add(rest.payload.len().min(1))
        })
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
    packet.encoded_len().saturating_sub(packet.payload.len())
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
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;
    use crate::pes::{self, EncodedLen, OptionalHeader, StreamId};
    use crate::ps::demux::{Demuxer, Event};
    use crate::ps::pack_header;
    use crate::ps::system_header::{self, StreamBound, StreamBoundLoop, SystemHeader};
    use crate::time::{ClockReference, Timestamp};

    /// `stream_id` of the first video stream
    const VIDEO: u8 = 0xE0;

    fn timestamp(value: u64) -> Timestamp {
        Timestamp::try_from(value).unwrap()
    }

    /// `length` bytes counting up, so a payload put back together out of order
    /// differs
    fn counting_payload(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect()
    }

    /// The PES packets `bytes` hold one after another, each cut by its
    /// `PES_packet_length`
    fn packets(bytes: &[u8]) -> Vec<PesPacket<'_>> {
        let mut packets = Vec::new();
        let mut rest = bytes;
        while let EncodedLen::Bounded(length) = pes::encoded_len(rest) {
            let (packet, after) = rest.split_at(length);
            packets.push(PesPacket::read(packet).unwrap());
            rest = after;
        }
        assert!(rest.is_empty(), "bytes after the PES packets: {rest:02x?}");
        packets
    }

    fn joined_payload(packets: &[PesPacket<'_>]) -> Vec<u8> {
        packets
            .iter()
            .flat_map(|packet| packet.payload.iter().copied())
            .collect()
    }

    /// The timestamps of each packet and the length of its payload
    fn shapes(packets: &[PesPacket<'_>]) -> Vec<(Option<u64>, Option<u64>, usize)> {
        packets
            .iter()
            .map(|packet| {
                let header = packet.optional_header.as_ref();
                (
                    header.and_then(|header| header.pts).map(Timestamp::get),
                    header.and_then(|header| header.dts).map(Timestamp::get),
                    packet.payload.len(),
                )
            })
            .collect()
    }

    /// A video PES packet carrying `payload`, with an optional header holding
    /// the timestamps given
    fn video_packet(pts: Option<u64>, dts: Option<u64>, payload: &[u8]) -> PesPacket<'_> {
        let mut header = OptionalHeader::new();
        header.pts = pts.map(timestamp);
        header.dts = dts.map(timestamp);
        let mut packet = PesPacket::new(StreamId::from(VIDEO), payload);
        packet.optional_header = Some(header);
        packet
    }

    fn write(packet: &PesPacket<'_>) -> (usize, Vec<u8>) {
        let mut out = Vec::new();
        let written = PackWriter::new()
            .write_pes_packets(packet, &mut out)
            .unwrap();
        (written, out)
    }

    #[test]
    fn a_payload_too_long_for_one_packet_splits_and_rejoins() {
        let payload = counting_payload(200_000);
        let (written, out) = write(&video_packet(Some(90_000), Some(86_400), &payload));
        let packets = packets(&out);
        assert_eq!(
            (
                written,
                joined_payload(&packets) == payload,
                shapes(&packets)
            ),
            (
                4,
                true,
                vec![
                    (Some(90_000), Some(86_400), 65_522),
                    (None, None, 65_532),
                    (None, None, 65_532),
                    (None, None, 3_414),
                ]
            )
        );
    }

    #[test]
    fn a_payload_filling_one_packet_exactly_is_not_split() {
        let payload = counting_payload(65_527);
        let packet = video_packet(Some(0), None, &payload);
        let (written, out) = write(&packet);
        assert_eq!((written, packets(&out)), (1, vec![packet]));
    }

    #[test]
    fn a_payload_one_byte_over_one_packet_goes_on_in_a_second_without_timestamps() {
        let payload = counting_payload(65_528);
        let (written, out) = write(&video_packet(Some(0), None, &payload));
        let (first, second) = payload.split_at(65_527);
        assert_eq!(
            (written, packets(&out)),
            (
                2,
                vec![
                    video_packet(Some(0), None, first),
                    video_packet(None, None, second),
                ]
            )
        );
    }

    #[test]
    fn the_packets_after_the_first_keep_its_header_but_its_times() {
        let payload = counting_payload(70_000);
        let mut header = OptionalHeader::new();
        header.pts = Some(timestamp(90_000));
        header.escr = Some(ClockReference::new(1_000, 0).unwrap());
        header.copyright = true;
        header.stuffing_bytes = 2;
        let mut packet = PesPacket::new(StreamId::from(VIDEO), &payload);
        packet.optional_header = Some(header.clone());
        let (written, out) = write(&packet);
        let mut continued = header.clone();
        continued.pts = None;
        continued.escr = None;
        let first_length = usize::from(u16::MAX) - header.encoded_len();
        let (first, second) = payload.split_at(first_length);
        let mut expected_first = PesPacket::new(StreamId::from(VIDEO), first);
        expected_first.optional_header = Some(header);
        let mut expected_second = PesPacket::new(StreamId::from(VIDEO), second);
        expected_second.optional_header = Some(continued);
        assert_eq!(
            (written, packets(&out)),
            (2, vec![expected_first, expected_second])
        );
    }

    #[test]
    fn an_empty_payload_still_carries_its_timestamps() {
        let packet = video_packet(Some(45_000), None, &[]);
        let (written, out) = write(&packet);
        assert_eq!((written, packets(&out)), (1, vec![packet]));
    }

    #[test]
    fn a_packet_written_without_timestamps_round_trips() {
        let packet = video_packet(None, None, &[0xAA, 0xBB]);
        let (written, out) = write(&packet);
        assert_eq!((written, packets(&out)), (1, vec![packet]));
    }

    #[test]
    fn a_timestamp_at_the_top_of_its_range_round_trips() {
        let top = (1 << 33) - 1;
        let packet = video_packet(Some(top), Some(top), &[0x00]);
        let (_, out) = write(&packet);
        assert_eq!(packets(&out), vec![packet]);
    }

    #[test]
    fn a_stream_without_an_optional_header_gets_its_payload_alone() {
        let payload = counting_payload(70_000);
        let (written, out) = write(&PesPacket::new(StreamId::PRIVATE_STREAM_2, &payload));
        let (first, second) = payload.split_at(65_535);
        assert_eq!(
            (written, packets(&out)),
            (
                2,
                vec![
                    PesPacket::new(StreamId::PRIVATE_STREAM_2, first),
                    PesPacket::new(StreamId::PRIVATE_STREAM_2, second),
                ]
            )
        );
    }

    /// Writes `packet` after a byte already in `out`, and returns the result
    /// and what `out` holds after
    fn written_after_a_byte(packet: &PesPacket<'_>) -> (Result<usize, Error>, Vec<u8>) {
        let mut out = vec![0x47];
        let result = PackWriter::new().write_pes_packets(packet, &mut out);
        (result, out)
    }

    #[test]
    fn rejects_a_dts_without_a_pts() {
        assert_eq!(
            written_after_a_byte(&video_packet(None, Some(0), &[0x00])),
            (
                Err(Error::PesPacket {
                    error: pes::Error::ForbiddenPtsDtsFlags
                }),
                vec![0x47]
            )
        );
    }

    #[test]
    fn rejects_a_padding_byte_other_than_0xff() {
        assert_eq!(
            written_after_a_byte(&PesPacket::new(StreamId::PADDING_STREAM, &[0xff, 0x00])),
            (
                Err(Error::PesPacket {
                    error: pes::Error::Forbidden {
                        field: "padding_byte",
                        value: 0x00
                    }
                }),
                vec![0x47]
            )
        );
    }

    #[test]
    fn rejects_a_timestamp_on_a_stream_that_carries_no_optional_header() {
        let mut packet = video_packet(Some(0), None, &[0x00]);
        packet.stream_id = StreamId::PADDING_STREAM;
        assert_eq!(
            written_after_a_byte(&packet),
            (
                Err(Error::PesPacket {
                    error: pes::Error::OptionalHeaderPresence {
                        stream_id: StreamId::PADDING_STREAM
                    }
                }),
                vec![0x47]
            )
        );
    }

    #[test]
    fn rejects_the_stream_id_of_an_element_that_is_not_a_pes_packet() {
        for value in [0xB9, 0xBA, 0xBB, 0xBC, 0x00] {
            let stream_id = StreamId::from(value);
            assert_eq!(
                written_after_a_byte(&PesPacket::new(stream_id, &[0x00])),
                (Err(Error::StreamId { stream_id }), vec![0x47]),
                "{value:#04x}"
            );
        }
    }

    #[test]
    fn rejects_a_packet_of_an_unspecified_length() {
        let mut packet = video_packet(None, None, &[0x00]);
        packet.length_unspecified = true;
        assert_eq!(
            written_after_a_byte(&packet),
            (
                Err(Error::LengthUnspecified {
                    stream_id: StreamId::from(VIDEO)
                }),
                vec![0x47]
            )
        );
    }

    #[test]
    fn a_packet_the_pes_layer_refuses_leaves_the_output_as_it_was() {
        assert_eq!(
            written_after_a_byte(&PesPacket::new(StreamId::PRIVATE_STREAM_2, &[])),
            (
                Err(Error::PesPacket {
                    error: pes::Error::LengthUnspecified {
                        stream_id: StreamId::PRIVATE_STREAM_2
                    }
                }),
                vec![0x47]
            )
        );
    }

    #[test]
    fn a_system_header_is_written_as_it_reads_back() {
        let mut stream_bounds = Vec::new();
        StreamBoundLoop::write_from(
            [
                StreamBound::new(StreamId::from(VIDEO), true, 230),
                StreamBound::new(StreamId::from(0xC0), false, 32),
            ],
            &mut stream_bounds,
        )
        .unwrap();
        let mut header = SystemHeader::new(1_101_198, 1, 1);
        header.stream_bounds = StreamBoundLoop::from(&stream_bounds[..]);
        let mut out = Vec::new();

        PackWriter::new()
            .write_system_header(&header, &mut out)
            .unwrap();

        assert_eq!(SystemHeader::read(&out), Ok(header));
    }

    #[test]
    fn a_system_header_the_system_header_writer_refuses_is_refused_leaving_out_as_it_was() {
        let header = SystemHeader::new(0, 1, 1);
        let mut out = vec![0x00];

        let error = PackWriter::new().write_system_header(&header, &mut out);

        assert_eq!(
            error,
            Err(Error::SystemHeader {
                error: system_header::Error::Forbidden {
                    field: "rate_bound",
                    value: 0,
                }
            })
        );
        assert_eq!(out, [0x00]);
    }

    #[test]
    fn the_count_of_the_pieces_is_known_before_they_are_cut() {
        // Pieces of at most 100 bytes, then 60: a header of 14 bytes with its
        // PTS leaves 86 for the first payload, and one of 9 without it 51
        let shapes: Vec<(usize, (usize, usize))> = [0, 86, 87, 86 + 51, 86 + 51 + 1, 5_000]
            .into_iter()
            .map(|length| {
                let payload = vec![0xaa; length];
                let packet = video_packet(Some(90_000), None, &payload);
                let pieces = PesPacketPieces::new(&packet, 100, 60);
                let known = pieces.len();
                let cut: Vec<PesPacket<'_>> = pieces.collect();
                (
                    known,
                    (cut.len(), cut.iter().map(PesPacket::encoded_len).sum()),
                )
            })
            .collect();
        let (known, cut): (Vec<_>, Vec<_>) = shapes.into_iter().unzip();
        assert_eq!(
            (known, cut.clone()),
            (
                cut.iter().map(|&(count, _)| count).collect::<Vec<_>>(),
                vec![
                    (1, 14),
                    (1, 100),
                    (2, 110),
                    (2, 160),
                    (3, 170),
                    (98, 5_000 + 14 + 97 * 9)
                ]
            )
        );
    }

    #[test]
    fn pack_stuffing_and_padding_read_back_through_the_demuxer_as_written() {
        let writer = PackWriter::new();
        let header =
            PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000).with_pack_stuffing_length(7);
        let padding = vec![0xff; 65_536];
        let mut stream = Vec::new();
        writer.write_pack_header(&header, &mut stream).unwrap();
        let written = writer
            .write_pes_packets(
                &PesPacket::new(StreamId::PADDING_STREAM, &padding),
                &mut stream,
            )
            .unwrap();
        writer.write_program_end_code(&mut stream);

        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&stream);
        demuxer.finish();
        let mut read = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            read.push(if let Event::PackHeader(header) = event {
                Read::PackHeader(header)
            } else if let Event::PesPacket(packet) = event {
                Read::PesPacket(packet.stream_id, packet.payload.to_vec())
            } else if event == Event::ProgramEnd {
                Read::ProgramEnd
            } else {
                Read::Other(alloc::format!("{event:?}"))
            });
        }
        // 65 535 bytes of padding fill the PES_packet_length of one packet
        assert_eq!(
            (written, read),
            (
                2,
                vec![
                    Read::Other("SyncAcquired".into()),
                    Read::PackHeader(header),
                    Read::PesPacket(StreamId::PADDING_STREAM, vec![0xff; 65_535]),
                    Read::PesPacket(StreamId::PADDING_STREAM, vec![0xff]),
                    Read::ProgramEnd,
                ]
            )
        );
    }

    /// What the demuxer reads of a Program Stream, owned
    #[derive(Debug, PartialEq)]
    enum Read {
        PackHeader(PackHeader),
        PesPacket(StreamId, Vec<u8>),
        ProgramEnd,
        Other(alloc::string::String),
    }

    #[test]
    fn pack_stuffing_past_7_bytes_is_refused_leaving_out_as_it_was() {
        let header =
            PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000).with_pack_stuffing_length(8);
        let mut out = vec![0xaa];

        assert_eq!(
            (PackWriter::new().write_pack_header(&header, &mut out), out),
            (
                Err(Error::PackHeader {
                    error: pack_header::Error::Field {
                        field: "pack_stuffing_length",
                        error: crate::field::Error::ValueTooWide { width: 3, value: 8 }
                    }
                }),
                vec![0xaa]
            )
        );
    }

    #[test]
    fn a_pack_of_padding_takes_stuffing_up_to_7_bytes_and_padding_packets_past_them() {
        let header = PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000);
        let written = |length: usize| {
            let mut out = Vec::new();
            PackWriter::new()
                .write_pack(&header, &[], None, Fill::of(length - 14), &mut out)
                .unwrap();
            let stuffing = out.get(13).unwrap() & 0x07;
            let (pack_header, rest) = out.split_at(14 + usize::from(stuffing));
            let padding: Vec<_> = packets(rest)
                .into_iter()
                .map(|packet| {
                    (
                        packet.stream_id,
                        packet.payload.len(),
                        packet.payload.iter().all(|&byte| byte == 0xff),
                    )
                })
                .collect();
            (
                out.len(),
                PackHeader::read(pack_header).unwrap().pack_stuffing_length,
                padding,
            )
        };
        let padding = |length: usize| (StreamId::PADDING_STREAM, length, true);

        // Past the longest PES packet, two of as near the same length
        assert_eq!(
            [14, 21, 22, 23, 14 + 65_541, 14 + 65_542].map(written),
            [
                (14, 0, vec![]),
                (21, 7, vec![]),
                (22, 0, vec![padding(2)]),
                (23, 0, vec![padding(3)]),
                (14 + 65_541, 0, vec![padding(65_535)]),
                (14 + 65_542, 0, vec![padding(32_765), padding(32_765)]),
            ]
        );
    }
}
