//! A Program Stream written with the crate's writers, read back by the
//! Program Stream demuxer and by the PES packet reader

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    reason = "the stream is cut at offsets its own writers gave"
)]
mod tests {
    use mpeg2_systems::pes::{
        OptionalHeader, PesPacket, PesPacketLength, StreamId, pes_packet_length,
    };
    use mpeg2_systems::ps::demux::{Demuxer, Event};
    use mpeg2_systems::ps::mux::write_pes_packets;
    use mpeg2_systems::ps::psm::ElementaryStreamLoop;
    use mpeg2_systems::ps::{MPEG_PROGRAM_END_CODE, PackHeader, ProgramStreamMap};
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

    /// The PES packets that `bytes`, which start with one, hold one after
    /// another, cut by hand by their `PES_packet_length`
    fn cut_by_hand(bytes: &[u8]) -> Vec<PesPacket<'_>> {
        let mut packets = Vec::new();
        let mut rest = bytes;
        while let PesPacketLength::Bounded(length) = pes_packet_length(rest) {
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

        let mut stream = Vec::new();
        pack_header.write(&mut stream).unwrap();
        map.write(&mut stream).unwrap();
        let pes_start = stream.len();
        write_pes_packets(
            StreamId::from(0xE0),
            Some(timestamp(93_600)),
            Some(timestamp(90_000)),
            &video,
            &mut stream,
        )
        .unwrap();
        write_pes_packets(
            StreamId::from(0xC0),
            Some(timestamp(91_800)),
            None,
            &audio,
            &mut stream,
        )
        .unwrap();
        let pes_end = stream.len();
        stream.extend_from_slice(&MPEG_PROGRAM_END_CODE);

        let mut video_header = OptionalHeader::new();
        video_header.pts = Some(timestamp(93_600));
        video_header.dts = Some(timestamp(90_000));
        let mut video_packet = PesPacket::new(StreamId::from(0xE0), &video);
        video_packet.optional_header = Some(video_header);
        let mut audio_header = OptionalHeader::new();
        audio_header.pts = Some(timestamp(91_800));
        let mut audio_packet = PesPacket::new(StreamId::from(0xC0), &audio);
        audio_packet.optional_header = Some(audio_header);

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
        let mut stream = Vec::new();
        PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000)
            .write(&mut stream)
            .unwrap();
        let pes_start = stream.len();
        let written = write_pes_packets(
            StreamId::from(0xE0),
            Some(timestamp(90_000)),
            None,
            &payload,
            &mut stream,
        )
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
        pack_header.write(&mut stream).unwrap();
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
        let mut stream = Vec::new();
        PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000)
            .write(&mut stream)
            .unwrap();
        map.write(&mut stream).unwrap();
        assert_eq!(
            demuxed(&stream)[2..],
            [described(&Event::ProgramStreamMap(map))]
        );
    }
}
