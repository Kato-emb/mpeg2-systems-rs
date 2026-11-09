//! A Program Stream written with `PackWriter`, read back by the
//! Program Stream demuxer and by the PES packet reader

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    reason = "the stream is cut at offsets its own writers gave"
)]
mod tests {
    use mpeg2_systems::pes::{self, EncodedLen, OptionalHeader, PesPacket, StreamId};
    use mpeg2_systems::ps::demux::{Demuxer, Event};
    use mpeg2_systems::ps::mux::PackWriter;
    use mpeg2_systems::ps::psm::ElementaryStreamLoop;
    use mpeg2_systems::ps::{
        PackHeader, ProgramStreamMap, StreamBound, StreamBoundLoop, SystemHeader,
    };
    use mpeg2_systems::psi::VersionNumber;
    use mpeg2_systems::time::{ClockReference, Timestamp};

    fn timestamp(value: u64) -> Timestamp {
        Timestamp::try_from(value).unwrap()
    }

    /// Every event the demuxer gives for `stream`, as `Debug` prints it
    fn demuxed(stream: &[u8]) -> Vec<String> {
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(stream);
        demuxer.finish();
        let mut events = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            events.push(format!("{event:?}"));
        }
        events
    }

    fn described(event: &Event<'_>) -> String {
        format!("{event:?}")
    }

    /// A PES packet of `stream_id` carrying `payload`, with an optional header
    /// holding the timestamps given
    fn pes_packet(
        stream_id: u8,
        pts: Option<u64>,
        dts: Option<u64>,
        payload: &[u8],
    ) -> PesPacket<'_> {
        let mut header = OptionalHeader::new();
        header.pts = pts.map(timestamp);
        header.dts = dts.map(timestamp);
        let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
        packet.optional_header = Some(header);
        packet
    }

    /// The PES packets that `bytes`, which start with one, hold one after
    /// another, cut by hand by their `PES_packet_length`
    fn cut_by_hand(bytes: &[u8]) -> Vec<PesPacket<'_>> {
        let mut packets = Vec::new();
        let mut rest = bytes;
        while let EncodedLen::Bounded(length) = pes::encoded_len(rest) {
            if rest.len() < length {
                break;
            }
            let (packet, after) = rest.split_at(length);
            packets.push(PesPacket::read(packet).unwrap());
            rest = after;
        }
        packets
    }

    #[test]
    fn a_written_pack_reads_back_through_the_demuxer_and_the_pes_reader() {
        let pack_header = PackHeader::new(ClockReference::new(1234, 56).unwrap(), 2_000);
        let streams = [0x1B, 0xE0, 0x00, 0x00, 0x03, 0xC0, 0x00, 0x00];
        let mut map = ProgramStreamMap::new(VersionNumber::try_from(0).unwrap());
        map.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
        let video = vec![0xAA; 1000];
        let audio = vec![0xBB; 500];
        let video_packet = pes_packet(0xE0, Some(93_600), Some(90_000), &video);
        let audio_packet = pes_packet(0xC0, Some(91_800), None, &audio);

        let writer = PackWriter::new();
        let mut stream = Vec::new();
        writer.write_pack_header(&pack_header, &mut stream).unwrap();
        writer.write_program_stream_map(&map, &mut stream).unwrap();
        let pes_start = stream.len();
        writer
            .write_pes_packets(&video_packet, &mut stream)
            .unwrap();
        writer
            .write_pes_packets(&audio_packet, &mut stream)
            .unwrap();
        let pes_end = stream.len();
        writer.write_program_end_code(&mut stream);

        let hand_cut = cut_by_hand(&stream[pes_start..pes_end]);
        assert_eq!(
            (demuxed(&stream), hand_cut),
            (
                vec![
                    described(&Event::SyncAcquired),
                    described(&Event::PackHeader(pack_header)),
                    described(&Event::ProgramStreamMap(map)),
                    described(&Event::PesPacket(video_packet.clone())),
                    described(&Event::PesPacket(audio_packet.clone())),
                    described(&Event::ProgramEnd),
                ],
                vec![video_packet, audio_packet]
            )
        );
    }

    #[test]
    fn a_split_payload_reads_back_joined_with_its_timestamp_on_the_first_packet() {
        let payload: Vec<u8> = (0..200_000_u32)
            .map(|index| index.to_be_bytes()[3])
            .collect();
        let writer = PackWriter::new();
        let mut stream = Vec::new();
        writer
            .write_pack_header(
                &PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000),
                &mut stream,
            )
            .unwrap();
        let pes_start = stream.len();
        let written = writer
            .write_pes_packets(&pes_packet(0xE0, Some(90_000), None, &payload), &mut stream)
            .unwrap();
        let packets = cut_by_hand(&stream[pes_start..]);
        let joined: Vec<u8> = packets
            .iter()
            .flat_map(|packet| packet.payload.iter().copied())
            .collect();
        let timestamps: Vec<Option<u64>> = packets
            .iter()
            .map(|packet| {
                packet
                    .optional_header
                    .as_ref()
                    .and_then(|header| header.pts)
                    .map(Timestamp::get)
            })
            .collect();
        let read_back = demuxed(&stream)
            .iter()
            .filter(|event| event.starts_with("PesPacket"))
            .count();
        assert_eq!(
            (written, read_back, joined == payload, timestamps),
            (4, 4, true, vec![Some(90_000), None, None, None])
        );
    }

    #[test]
    fn an_scr_and_a_mux_rate_at_the_top_of_their_ranges_read_back() {
        let pack_header = PackHeader::new(
            ClockReference::new((1 << 33) - 1, 299).unwrap(),
            (1 << 22) - 1,
        );
        let mut stream = Vec::new();
        PackWriter::new()
            .write_pack_header(&pack_header, &mut stream)
            .unwrap();
        assert_eq!(
            demuxed(&stream),
            [
                described(&Event::SyncAcquired),
                described(&Event::PackHeader(pack_header)),
            ]
        );
    }

    #[test]
    fn a_map_naming_no_streams_reads_back() {
        let map = ProgramStreamMap::new(VersionNumber::try_from(31).unwrap());
        let writer = PackWriter::new();
        let mut stream = Vec::new();
        writer
            .write_pack_header(
                &PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000),
                &mut stream,
            )
            .unwrap();
        writer.write_program_stream_map(&map, &mut stream).unwrap();
        assert_eq!(
            demuxed(&stream)[2..],
            [described(&Event::ProgramStreamMap(map))]
        );
    }

    #[test]
    fn a_written_system_header_reads_back_in_its_pack() {
        let pack_header = PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000);
        let mut stream_bounds = Vec::new();
        StreamBoundLoop::write_from(
            [
                StreamBound::new(StreamId::from(0xE0), true, 230),
                StreamBound::new(StreamId::from(0xC0), false, 32),
            ],
            &mut stream_bounds,
        )
        .unwrap();
        let mut system_header = SystemHeader::new(2_000, 1, 1);
        system_header.stream_bounds = StreamBoundLoop::from(&stream_bounds[..]);
        let video = vec![0xAA; 100];
        let video_packet = pes_packet(0xE0, Some(3_600), None, &video);

        let writer = PackWriter::new();
        let mut stream = Vec::new();
        writer.write_pack_header(&pack_header, &mut stream).unwrap();
        writer
            .write_system_header(&system_header, &mut stream)
            .unwrap();
        writer
            .write_pes_packets(&video_packet, &mut stream)
            .unwrap();
        writer.write_program_end_code(&mut stream);

        assert_eq!(
            demuxed(&stream),
            vec![
                described(&Event::SyncAcquired),
                described(&Event::PackHeader(pack_header)),
                described(&Event::SystemHeader(system_header)),
                described(&Event::PesPacket(video_packet)),
                described(&Event::ProgramEnd),
            ]
        );
    }
}
