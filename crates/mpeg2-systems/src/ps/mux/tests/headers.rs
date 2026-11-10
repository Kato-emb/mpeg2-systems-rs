//! The system header and the program stream map sent again, and the map
//! updated

use super::*;

/// 0.5 s in 90 kHz ticks
const HALF_SECOND: u64 = 45_000;

/// Whether each pack of `bytes` carries the system header and the map
fn headers_by_pack(bytes: &[u8]) -> Vec<(bool, bool)> {
    carried(bytes)
        .iter()
        .map(|pack| (pack.system_header.is_some(), pack.map.is_some()))
        .collect()
}

/// A PES packet of video every 0.5 s, from 0 to 3 s
fn every_half_second() -> Vec<PesPacket<'static>> {
    (0..7)
        .map(|index| pes_packet(VIDEO, &[0xaa; 100], Some(HALF_SECOND.saturating_mul(index))))
        .collect()
}

/// The bytes of a program stream map of `program_stream_info` and the
/// video and audio streams of `video_type` and `audio_type`, with
/// `video_info` for the video stream
fn map_bytes(
    version: u8,
    program_stream_info: &[u8],
    (video_type, video_info): (StreamType, &[u8]),
    audio_type: StreamType,
) -> Vec<u8> {
    let mut streams = Vec::new();
    ElementaryStreamLoop::write_from(
        [
            ElementaryStream::new(video_type, StreamId::from(VIDEO))
                .with_elementary_stream_info(DescriptorLoop::from(video_info)),
            ElementaryStream::new(audio_type, StreamId::from(AUDIO)),
        ],
        &mut streams,
    )
    .unwrap();
    let mut map = ProgramStreamMap::new(VersionNumber(version));
    map.program_stream_info = DescriptorLoop::from(program_stream_info);
    map.elementary_streams = ElementaryStreamLoop::from(streams.as_slice());
    let mut bytes = Vec::new();
    map.write(&mut bytes).unwrap();
    bytes
}

/// The video stream as an AVC video stream with `es_info`
fn avc_video(es_info: &[u8]) -> ElementaryStream<'_> {
    ElementaryStream::new(StreamType::AVC_VIDEO, StreamId::from(VIDEO))
        .with_elementary_stream_info(DescriptorLoop::from(es_info))
}

/// The map version of each pack of `bytes` that carries a map
fn map_versions(bytes: &[u8]) -> Vec<u8> {
    carried(bytes)
        .into_iter()
        .filter_map(|pack| pack.map.map(|map| map.program_stream_map_version.get()))
        .collect()
}

#[test]
fn by_default_the_system_header_and_the_map_are_in_the_first_pack_alone() {
    let stream = multiplexed_by(muxer(), &every_half_second());

    let mut expected = vec![(false, false); 7];
    expected[0] = (true, true);
    assert_eq!(headers_by_pack(&stream), expected);
}

#[test]
fn the_headers_go_again_in_the_first_pack_header_interval_or_more_after_the_last() {
    let at_interval = |interval: Duration| {
        multiplexed_by(
            muxer_of(MuxerConfig::new(RATE).with_header_interval(interval)),
            &every_half_second(),
        )
    };
    let second = at_interval(Duration::from_secs(1));
    // 27 000 001 ticks of the STC, a tick past 1 s
    let past_a_second = at_interval(Duration::from_nanos(1_000_000_038));

    let with = (true, true);
    let without = (false, false);
    assert_eq!(
        (headers_by_pack(&second), headers_by_pack(&past_a_second)),
        (
            vec![with, without, with, without, with, without, with],
            vec![with, without, without, with, without, without, with]
        )
    );
    let packs = carried(&second);
    let first = &packs[0];
    assert!(
        packs
            .iter()
            .filter(|pack| pack.system_header.is_some())
            .all(|pack| (&pack.system_header, &pack.map) == (&first.system_header, &first.map))
    );
}

#[test]
fn at_a_header_interval_of_0_each_pes_packet_carries_the_headers_and_no_pack_after_it() {
    let packets = [
        pes_packet(VIDEO, &[0xaa; 3_000], Some(0)),
        pes_packet(VIDEO, &[0xaa; 100], Some(3 * HALF_SECOND)),
        pes_packet(AUDIO, &[0xbb; 100], Some(3 * HALF_SECOND)),
    ];
    let stream = multiplexed_by(
        muxer_of(MuxerConfig::new(RATE).with_header_interval(Duration::ZERO)),
        &packets,
    );

    let packs: Vec<_> = carried(&stream)
        .iter()
        .map(|pack| (pack.system_header.is_some(), pack.stream_ids.len()))
        .collect();
    // The split PES packet, two packs of a pack header alone over the leap,
    // and the two PES packets at 1.5 s
    assert_eq!(
        packs,
        [
            (true, 1),
            (false, 1),
            (false, 0),
            (false, 0),
            (true, 1),
            (true, 1)
        ]
    );
}

#[test]
fn an_updated_stream_goes_in_the_map_of_the_next_pes_packet_its_version_incremented() {
    let mut muxer = muxer();
    let mut stream = Vec::new();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 100], Some(0)))
        .unwrap();
    let info = descriptors(6);
    muxer.update_stream(avc_video(&info)).unwrap();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 100], Some(3_600)))
        .unwrap();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 100], Some(7_200)))
        .unwrap();
    muxer.finish();
    muxer.poll_output(&mut stream);

    let packs = carried(&stream);
    let updated = map_bytes(
        1,
        &[],
        (StreamType::AVC_VIDEO, &info),
        StreamType::ISO_IEC_11172_3_AUDIO,
    );
    assert_eq!(
        packs.iter().map(|pack| &pack.map).collect::<Vec<_>>(),
        [
            &Some(
                ProgramStreamMap::read(&map_bytes(
                    0,
                    &[],
                    (StreamType::ISO_IEC_13818_2_VIDEO, &[]),
                    StreamType::ISO_IEC_11172_3_AUDIO
                ))
                .unwrap()
            ),
            &Some(ProgramStreamMap::read(&updated).unwrap()),
            &None
        ]
    );
    assert_eq!(packs[1].system_header, packs[0].system_header);
}

#[test]
fn updates_before_the_next_pes_packet_are_one_change_of_version_and_none_before_the_first() {
    let info = descriptors(6);
    let mut before_first = muxer();
    before_first.update_stream(avc_video(&info)).unwrap();
    let mut twice = muxer();
    twice
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
        .unwrap();
    twice.poll_output(&mut Vec::new());
    twice.update_stream(avc_video(&[])).unwrap();
    twice.update_stream(avc_video(&info)).unwrap();
    let packets = [
        pes_packet(VIDEO, &[0xaa], Some(0)),
        pes_packet(VIDEO, &[0xaa], Some(3_600)),
    ];

    let first = multiplexed_by(before_first, &packets);
    let mut stream = Vec::new();
    twice.handle_pes_packet(&packets[1]).unwrap();
    twice.poll_output(&mut stream);
    assert_eq!(
        (carried(&first)[0].map, map_versions(&stream)),
        (
            Some(
                ProgramStreamMap::read(&map_bytes(
                    0,
                    &[],
                    (StreamType::AVC_VIDEO, &info),
                    StreamType::ISO_IEC_11172_3_AUDIO
                ))
                .unwrap()
            ),
            vec![1]
        )
    );
}

#[test]
fn an_update_or_program_stream_info_that_changes_nothing_writes_no_map() {
    let mut muxer = muxer();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
        .unwrap();
    muxer.poll_output(&mut Vec::new());
    muxer
        .update_stream(ElementaryStream::new(
            StreamType::ISO_IEC_13818_2_VIDEO,
            StreamId::from(VIDEO),
        ))
        .unwrap();
    muxer
        .set_program_stream_info(DescriptorLoop::from([].as_slice()))
        .unwrap();
    let mut stream = Vec::new();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(3_600)))
        .unwrap();
    muxer.poll_output(&mut stream);

    assert_eq!(headers_by_pack(&stream), [(false, false)]);
}

#[test]
fn the_version_of_the_map_wraps_from_31_to_0() {
    let mut muxer = muxer();
    let mut stream = Vec::new();
    for index in 0..33 {
        if index > 0 {
            let stream_type = if index % 2 == 1 {
                StreamType::AVC_VIDEO
            } else {
                StreamType::ISO_IEC_13818_2_VIDEO
            };
            muxer
                .update_stream(ElementaryStream::new(stream_type, StreamId::from(VIDEO)))
                .unwrap();
        }
        muxer
            .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(index * 3_600)))
            .unwrap();
    }
    muxer.poll_output(&mut stream);

    let mut expected: Vec<u8> = (0..32).collect();
    expected.push(0);
    assert_eq!(map_versions(&stream), expected);
}

#[test]
fn program_stream_info_goes_in_the_map_its_version_incremented_after_the_first_pes_packet() {
    let info = descriptors(8);
    let mut before_first = muxer();
    before_first
        .set_program_stream_info(DescriptorLoop::from(info.as_slice()))
        .unwrap();
    let mut after_first = muxer();
    let mut stream = Vec::new();
    after_first
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
        .unwrap();
    after_first
        .set_program_stream_info(DescriptorLoop::from(info.as_slice()))
        .unwrap();
    after_first
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(3_600)))
        .unwrap();
    after_first.poll_output(&mut stream);

    let video = (StreamType::ISO_IEC_13818_2_VIDEO, [].as_slice());
    let audio = StreamType::ISO_IEC_11172_3_AUDIO;
    let first = multiplexed_by(before_first, &[pes_packet(VIDEO, &[0xaa], Some(0))]);
    let maps: Vec<_> = carried(&stream)
        .into_iter()
        .filter_map(|pack| pack.map)
        .collect();
    assert_eq!(
        (carried(&first)[0].map, maps),
        (
            Some(ProgramStreamMap::read(&map_bytes(0, &info, video, audio)).unwrap()),
            vec![
                ProgramStreamMap::read(&map_bytes(0, &[], video, audio)).unwrap(),
                ProgramStreamMap::read(&map_bytes(1, &info, video, audio)).unwrap(),
            ]
        )
    );
}

#[test]
fn an_update_of_a_stream_not_registered_is_refused() {
    let mut muxer = muxer();
    assert_eq!(
        [
            muxer.update_stream(ElementaryStream::new(
                StreamType::ISO_IEC_13818_2_VIDEO,
                StreamId::from(0xe1)
            )),
            muxer.update_stream(ElementaryStream::new(
                StreamType::ISO_IEC_13818_2_VIDEO,
                StreamId::EXTENDED_STREAM_ID
            )),
        ],
        [
            Err(Error::UnregisteredStream {
                stream_id: StreamId::from(0xe1),
                stream_id_extension: None
            }),
            Err(Error::StreamIdExtension {
                stream_id: StreamId::EXTENDED_STREAM_ID
            }),
        ]
    );
}

#[test]
fn an_update_into_or_out_of_audio_or_video_is_refused_after_the_first_pes_packet() {
    let update = |muxer: &mut Muxer, stream_id: u8, stream_type: StreamType| {
        muxer.update_stream(ElementaryStream::new(
            stream_type,
            StreamId::from(stream_id),
        ))
    };
    let mut before_first = muxer();
    let mut after_first = muxer();
    after_first
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
        .unwrap();

    let private = StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA;
    assert_eq!(
        [
            update(&mut before_first, VIDEO, private),
            update(&mut after_first, VIDEO, private),
            update(&mut after_first, AUDIO, StreamType::ISO_IEC_13818_7_AUDIO),
            update(&mut after_first, AUDIO, StreamType::ISO_IEC_13818_3_AUDIO),
        ],
        [
            Ok(()),
            Err(Error::SystemHeaderChange {
                stream_id: StreamId::from(VIDEO),
                stream_id_extension: None
            }),
            Err(Error::SystemHeaderChange {
                stream_id: StreamId::from(AUDIO),
                stream_id_extension: None
            }),
            Ok(()),
        ]
    );
}

#[test]
fn a_map_past_1018_bytes_or_its_pack_past_pack_size_is_refused_and_leaves_the_muxer_as_it_was() {
    // A map of video and audio: 10 bytes besides its loop and two entries of
    // 4 leave 1 000 bytes of descriptors
    let too_long = Err(Error::ProgramStreamMap {
        error: psm::Error::ProgramStreamMapLength {
            program_stream_map_length: 1_019,
            limit: 1_018,
        },
    });
    let past_1018 = descriptors(1_001);
    let mut muxer = muxer();
    let packets = [
        pes_packet(VIDEO, &[0xaa], Some(0)),
        pes_packet(VIDEO, &[0xaa], Some(3_600)),
    ];
    muxer.handle_pes_packet(&packets[0]).unwrap();
    let refused = [
        muxer.update_stream(avc_video(&past_1018)),
        muxer.set_program_stream_info(DescriptorLoop::from(past_1018.as_slice())),
    ];
    // 14 of pack header, 18 of system header, 24 of map, 6 of PES packet
    // header and a byte of payload
    let mut tight = muxer_of(MuxerConfig::new(RATE).with_pack_size(63));
    let info = descriptors(2);
    let past_pack_size = [
        tight.update_stream(avc_video(&info)),
        tight.set_program_stream_info(DescriptorLoop::from(info.as_slice())),
    ];
    let mut stream = Vec::new();
    muxer.poll_output(&mut stream);
    muxer.handle_pes_packet(&packets[1]).unwrap();
    muxer.finish();
    muxer.poll_output(&mut stream);

    let past = Err(Error::PackLength { length: 65 });
    assert_eq!(
        (refused, past_pack_size, stream),
        (
            [too_long, too_long],
            [past, past],
            multiplexed_by(self::muxer(), &packets)
        )
    );
}

#[test]
fn a_pes_packet_whose_pack_cannot_also_carry_the_headers_sent_again_is_refused() {
    // The first pack: 14 + 18 + 24 + 6 + 3 + 5 of PTS + 3 of P-STD buffer
    // and a byte of payload. One with a PTS and a DTS takes 14 + 6 + 3 + 10
    // and a byte alone, 18 + 24 more with the headers
    let config = MuxerConfig::new(RATE).with_pack_size(74);
    let mut with_dts = pes_packet(VIDEO, &[0xaa], Some(3_600));
    with_dts.optional_header.as_mut().unwrap().dts = Some(timestamp(3_000));
    let first = pes_packet(VIDEO, &[0xaa], Some(0));
    let give = |config: MuxerConfig| {
        let mut muxer = muxer_of(config);
        muxer.handle_pes_packet(&first).unwrap();
        muxer.handle_pes_packet(&with_dts)
    };

    assert_eq!(
        [
            give(config),
            give(config.with_header_interval(Duration::ZERO))
        ],
        [Ok(()), Err(Error::PackLength { length: 76 })]
    );
}
