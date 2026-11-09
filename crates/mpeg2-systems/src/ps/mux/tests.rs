use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::descriptor;
use crate::pes::OptionalHeader;
use crate::ps::psm::{ElementaryStream, ElementaryStreamLoop};
use crate::ps::{ProgramStreamMap, StreamBound, StreamBoundLoop, SystemHeader, psm};
use crate::psi::VersionNumber;
use crate::time::{ClockReference, Timestamp};

const VIDEO: u8 = 0xe0;
const AUDIO: u8 = 0xc0;

/// The P-STD buffer registered for the video stream, 224 KiB
const VIDEO_BUFFER: PStdBuffer = PStdBuffer::new(true, 224);

/// The P-STD buffer registered for the audio stream, 4 KiB
const AUDIO_BUFFER: PStdBuffer = PStdBuffer::new(false, 32);

/// The `program_mux_rate` of the tests, 1 260 000 bytes a second
const RATE: u32 = 25_200;

fn timestamp(value: u64) -> Timestamp {
    Timestamp::try_from(value).unwrap()
}

/// A PES packet of `stream_id` carrying `payload`, with a PTS of `pts` 90
/// kHz ticks, or no time stamp
fn pes_packet(stream_id: u8, payload: &[u8], pts: Option<u64>) -> PesPacket<'_> {
    let mut header = OptionalHeader::new();
    header.pts = pts.map(timestamp);
    let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
    packet.optional_header = Some(header);
    packet
}

/// A muxer of `config` with a video and an audio stream
fn muxer_of(config: MuxerConfig) -> Muxer {
    let mut muxer = Muxer::new(config).unwrap();
    muxer
        .add_stream(
            StreamId::from(VIDEO),
            None,
            StreamType::ISO_IEC_13818_2_VIDEO,
            VIDEO_BUFFER,
            &[],
        )
        .unwrap();
    muxer
        .add_stream(
            StreamId::from(AUDIO),
            None,
            StreamType::ISO_IEC_11172_3_AUDIO,
            AUDIO_BUFFER,
            &[],
        )
        .unwrap();
    muxer
}

fn muxer() -> Muxer {
    muxer_of(MuxerConfig::new(RATE))
}

/// What `muxer` gives of `packets`, each taken before the next is given
fn multiplexed_by(mut muxer: Muxer, packets: &[PesPacket<'_>]) -> Vec<u8> {
    let mut stream = Vec::new();
    for packet in packets {
        muxer.handle_pes_packet(packet).unwrap();
        muxer.poll_output(&mut stream);
    }
    muxer.finish();
    muxer.poll_output(&mut stream);
    stream
}

/// An element of a Program Stream, cut by hand by its start code and length
#[derive(Debug, Clone, PartialEq, Eq)]
enum Element<'a> {
    PackHeader(PackHeader),
    SystemHeader(&'a [u8]),
    ProgramStreamMap,
    PesPacket(PesPacket<'a>),
    ProgramEnd,
}

/// The elements of `bytes`, one after another, with the bytes of each
fn elements(bytes: &[u8]) -> Vec<(Element<'_>, usize)> {
    let mut elements = Vec::new();
    let mut rest = bytes;
    while let Some(&[0x00, 0x00, 0x01, code]) = rest.first_chunk::<4>() {
        let length = match code {
            0xba => 14 + usize::from(rest[13] & 0x07),
            0xb9 => 4,
            _ => 6 + usize::from(u16::from_be_bytes([rest[4], rest[5]])),
        };
        let (element, after) = rest.split_at(length);
        let element = match code {
            0xba => Element::PackHeader(PackHeader::read(element).unwrap()),
            0xbb => Element::SystemHeader(element),
            0xbc => Element::ProgramStreamMap,
            0xb9 => Element::ProgramEnd,
            _ => Element::PesPacket(PesPacket::read(element).unwrap()),
        };
        elements.push((element, length));
        rest = after;
    }
    assert!(rest.is_empty(), "bytes after the elements: {rest:02x?}");
    elements
}

/// The PES packets of `bytes`
fn pes_packets(bytes: &[u8]) -> Vec<PesPacket<'_>> {
    elements(bytes)
        .into_iter()
        .filter_map(|(element, _)| {
            if let Element::PesPacket(packet) = element {
                Some(packet)
            } else {
                None
            }
        })
        .collect()
}

/// The SCR of each pack of `bytes`, in 27 MHz ticks
fn scrs(bytes: &[u8]) -> Vec<u64> {
    elements(bytes)
        .into_iter()
        .filter_map(|(element, _)| {
            if let Element::PackHeader(header) = element {
                Some(header.system_clock_reference.ticks())
            } else {
                None
            }
        })
        .collect()
}

/// The P-STD buffer of each PES packet of `bytes`, by `stream_id`
fn p_std_buffers(bytes: &[u8]) -> Vec<(u8, Option<PStdBuffer>)> {
    pes_packets(bytes)
        .into_iter()
        .map(|packet| {
            (
                packet.stream_id.get(),
                packet
                    .optional_header
                    .and_then(|header| header.pes_extension)
                    .and_then(|extension| extension.p_std_buffer),
            )
        })
        .collect()
}

/// The 27 MHz ticks the default `mux_delay` comes to
const DELAY: u64 = 18_900_000;

#[test]
fn a_program_mux_rate_of_0_or_past_22_bits_is_refused() {
    let new = |program_mux_rate| Muxer::new(MuxerConfig::new(program_mux_rate)).map(|_| ());
    assert_eq!(
        [0, 1 << 22, (1 << 22) - 1].map(new),
        [
            Err(Error::ProgramMuxRate {
                program_mux_rate: 0
            }),
            Err(Error::ProgramMuxRate {
                program_mux_rate: 1 << 22
            }),
            Ok(()),
        ]
    );
}

#[test]
fn a_pack_size_is_taken_from_the_shortest_pack_to_what_arrives_in_0_7_s() {
    let new = |program_mux_rate, pack_size| {
        let mut config = MuxerConfig::new(program_mux_rate);
        config.pack_size = pack_size;
        Muxer::new(config).map(|_| ())
    };
    // A pack header of 14 bytes, a PES packet header of 6, and a byte
    assert_eq!(
        [
            new(RATE, 20),
            new(RATE, 21),
            new(100, 3_500),
            new(100, 3_501),
            new(59, 2048),
            new(58, 2048),
        ],
        [
            Err(Error::PackSize { pack_size: 20 }),
            Ok(()),
            Ok(()),
            Err(Error::PackSize { pack_size: 3_501 }),
            Ok(()),
            Err(Error::PackSize { pack_size: 2048 }),
        ]
    );
}

#[test]
fn the_first_pack_carries_the_system_header_and_the_map_of_the_streams_registered() {
    let payload = [0xaa; 100];
    let stream = multiplexed_by(muxer(), &[pes_packet(VIDEO, &payload, Some(90_000))]);

    let mut bounds = Vec::new();
    StreamBoundLoop::write_from(
        [
            StreamBound::new(StreamId::from(VIDEO), true, 224),
            StreamBound::new(StreamId::from(AUDIO), false, 32),
        ],
        &mut bounds,
    )
    .unwrap();
    let mut system_header = SystemHeader::new(RATE, 1, 1);
    system_header.stream_bounds = StreamBoundLoop::from(bounds.as_slice());
    let mut streams = Vec::new();
    ElementaryStreamLoop::write_from(
        [
            ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
            ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
        ],
        &mut streams,
    )
    .unwrap();
    let mut map = ProgramStreamMap::new(VersionNumber(0));
    map.elementary_streams = ElementaryStreamLoop::from(streams.as_slice());
    let mut header = OptionalHeader::new();
    header.pts = Some(timestamp(90_000));
    let mut extension = PesExtension::new();
    extension.p_std_buffer = Some(VIDEO_BUFFER);
    header.pes_extension = Some(extension);
    let mut packet = PesPacket::new(StreamId::from(VIDEO), &payload);
    packet.optional_header = Some(header);
    let writer = PackWriter::new();
    let mut expected = Vec::new();
    writer
        .write_pack_header(
            &PackHeader::new(ClockReference::from_ticks(90_000 * 300 - DELAY), RATE),
            &mut expected,
        )
        .unwrap();
    writer
        .write_system_header(&system_header, &mut expected)
        .unwrap();
    writer
        .write_program_stream_map(&map, &mut expected)
        .unwrap();
    writer.write_pes_packets(&packet, &mut expected).unwrap();
    writer.write_program_end_code(&mut expected);
    assert_eq!(stream, expected);
}

#[test]
fn a_pes_packet_too_long_for_a_pack_is_split_into_packs_of_at_most_pack_size() {
    let payload: Vec<u8> = (0..10_000_u32)
        .map(|index| index.to_be_bytes()[3])
        .collect();
    let stream = multiplexed_by(
        muxer(),
        &[
            pes_packet(VIDEO, &payload, Some(90_000)),
            pes_packet(VIDEO, &payload, Some(93_600)),
        ],
    );

    // The bytes of each pack, and the elements after its pack header
    let mut packs: Vec<(usize, Vec<Element<'_>>)> = Vec::new();
    for (element, length) in elements(&stream) {
        match element {
            Element::PackHeader(_) => packs.push((length, Vec::new())),
            Element::ProgramEnd => {}
            Element::SystemHeader(_) | Element::ProgramStreamMap | Element::PesPacket(_) => {
                let pack = packs.last_mut().unwrap();
                pack.0 += length;
                pack.1.push(element);
            }
        }
    }
    let packets = pes_packets(&stream);
    let joined: Vec<u8> = packets
        .iter()
        .flat_map(|packet| packet.payload)
        .copied()
        .collect();
    let ptss: Vec<Option<u64>> = packets
        .iter()
        .map(|packet| {
            packet
                .optional_header
                .as_ref()
                .and_then(|header| header.pts)
                .map(Timestamp::get)
        })
        .collect();
    let shapes: Vec<usize> = packs.iter().map(|(_, elements)| elements.len()).collect();
    assert_eq!(
        (
            packs.iter().all(|&(length, _)| length <= 2048),
            &shapes[..2],
            shapes[1..].iter().all(|&count| count == 1),
            joined == [payload.clone(), payload].concat(),
            ptss.iter().flatten().count(),
            ptss.len(),
        ),
        (true, &[3, 1][..], true, true, 2, packs.len())
    );
}

#[test]
fn the_first_pes_packet_of_each_stream_carries_the_p_std_buffer_registered_unless_it_has_its_own() {
    let mut own = pes_packet(AUDIO, &[0xbb; 10], Some(90_000));
    let mut extension = PesExtension::new();
    extension.p_std_buffer = Some(PStdBuffer::new(false, 8));
    own.optional_header.as_mut().unwrap().pes_extension = Some(extension);
    let mut private = Muxer::new(MuxerConfig::new(RATE)).unwrap();
    private
        .add_stream(
            StreamId::from(VIDEO),
            None,
            StreamType::ISO_IEC_13818_2_VIDEO,
            VIDEO_BUFFER,
            &[],
        )
        .unwrap();
    private
        .add_stream(
            StreamId::PRIVATE_STREAM_2,
            None,
            StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
            PStdBuffer::new(false, 8),
            &[],
        )
        .unwrap();

    assert_eq!(
        (
            p_std_buffers(&multiplexed_by(
                muxer(),
                &[
                    pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                    own,
                    pes_packet(VIDEO, &[0xaa; 10], Some(93_600)),
                    pes_packet(AUDIO, &[0xbb; 10], Some(92_160)),
                ]
            )),
            p_std_buffers(&multiplexed_by(
                private,
                &[
                    pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                    PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc; 10]),
                ]
            ))
        ),
        (
            vec![
                (VIDEO, Some(VIDEO_BUFFER)),
                (AUDIO, Some(PStdBuffer::new(false, 8))),
                (VIDEO, None),
                (AUDIO, None),
            ],
            vec![(VIDEO, Some(VIDEO_BUFFER)), (0xbf, None)]
        )
    );
}

#[test]
fn a_stream_is_refused_for_its_stream_id_its_extension_a_bound_or_its_descriptors() {
    let add = |muxer: &mut Muxer, value: u8, extension: Option<u8>, es_info: &[u8]| {
        muxer.add_stream(
            StreamId::from(value),
            extension,
            StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
            PStdBuffer::new(false, 8),
            es_info,
        )
    };
    let mut muxer = muxer();
    assert_eq!(
        [
            add(&mut muxer, 0xbc, None, &[]),
            add(&mut muxer, 0xfd, None, &[]),
            add(&mut muxer, 0xbd, Some(0x55), &[]),
            add(&mut muxer, VIDEO, None, &[]),
            add(&mut muxer, 0xbd, None, &[0x0a, 0x05, 0x00]),
            muxer.add_stream(
                StreamId::from(0xc1),
                None,
                StreamType::ISO_IEC_11172_3_AUDIO,
                PStdBuffer::new(true, 8),
                &[],
            ),
        ],
        [
            Err(Error::StreamId {
                stream_id: StreamId::from(0xbc)
            }),
            Err(Error::StreamIdExtension {
                stream_id: StreamId::from(0xfd)
            }),
            Err(Error::StreamIdExtension {
                stream_id: StreamId::from(0xbd)
            }),
            Err(Error::StreamInUse {
                stream_id: StreamId::from(VIDEO),
                stream_id_extension: None
            }),
            Err(Error::ProgramStreamMap {
                error: psm::Error::Descriptor(descriptor::Error::Length {
                    descriptor_length: 5,
                    available_bytes: 1
                })
            }),
            Err(Error::SystemHeader {
                error: system_header::Error::Forbidden {
                    field: "P-STD_buffer_bound_scale",
                    value: 1
                }
            }),
        ]
    );
    // The streams refused are not registered
    assert_eq!(
        multiplexed_by(muxer, &[pes_packet(VIDEO, &[0xaa; 10], Some(90_000))]),
        multiplexed_by(
            self::muxer(),
            &[pes_packet(VIDEO, &[0xaa; 10], Some(90_000))]
        )
    );
}

#[test]
fn a_stream_past_32_audio_or_16_video_streams_is_refused() {
    let mut muxer = Muxer::new(MuxerConfig::new(RATE)).unwrap();
    for value in 0xc0..=0xdf {
        muxer
            .add_stream(
                StreamId::from(value),
                None,
                StreamType::ISO_IEC_13818_3_AUDIO,
                AUDIO_BUFFER,
                &[],
            )
            .unwrap();
    }
    for value in 0xe0..=0xef {
        muxer
            .add_stream(
                StreamId::from(value),
                None,
                StreamType::AVC_VIDEO,
                VIDEO_BUFFER,
                &[],
            )
            .unwrap();
    }
    let add = |muxer: &mut Muxer, stream_type| {
        muxer.add_stream(
            StreamId::PRIVATE_STREAM_1,
            None,
            stream_type,
            PStdBuffer::new(true, 8),
            &[],
        )
    };
    assert_eq!(
        [
            add(&mut muxer, StreamType::ISO_IEC_11172_3_AUDIO),
            add(&mut muxer, StreamType::HEVC_VIDEO),
            add(&mut muxer, StreamType::ISO_IEC_13818_7_AUDIO),
        ],
        [
            Err(Error::AudioBound { audio_bound: 33 }),
            Err(Error::VideoBound { video_bound: 17 }),
            Ok(()),
        ]
    );
}

#[test]
fn a_stream_is_refused_whose_first_pack_would_not_fit_pack_size() {
    let mut config = MuxerConfig::new(RATE);
    config.pack_size = 55;
    let mut muxer = Muxer::new(config).unwrap();
    let add = |muxer: &mut Muxer| {
        muxer.add_stream(
            StreamId::from(VIDEO),
            None,
            StreamType::ISO_IEC_13818_2_VIDEO,
            VIDEO_BUFFER,
            &[],
        )
    };
    // 14 of pack header, 15 of system header, 20 of map, 6 of PES packet
    // header and a byte of payload
    let refused = add(&mut muxer);
    config.pack_size = 56;
    let mut fits = Muxer::new(config).unwrap();
    assert_eq!(
        (refused, add(&mut fits)),
        (Err(Error::PackLength { length: 56 }), Ok(()))
    );
}

#[test]
fn a_pes_packet_is_refused_whose_header_and_a_byte_do_not_fit_a_pack() {
    let mut config = MuxerConfig::new(RATE);
    config.pack_size = 100;
    let mut muxer = muxer_of(config);
    let mut packet = pes_packet(VIDEO, &[0xaa; 10], Some(90_000));
    packet.optional_header.as_mut().unwrap().stuffing_bytes = 40;
    // 14 + 18 + 24 + 6 + 3 + 5 of PTS + 3 of P-STD buffer + 40 + 1
    assert_eq!(
        muxer.handle_pes_packet(&packet),
        Err(Error::PackLength { length: 114 })
    );
}

#[test]
fn a_stream_of_stream_id_0xfd_is_told_by_the_stream_id_extension_of_its_packets() {
    let extended = |extension: Option<u8>, payload: &'static [u8]| {
        let mut packet = pes_packet(0xfd, payload, Some(90_000));
        let mut pes_extension = PesExtension::new();
        pes_extension.pes_extension_field = extension.map(|extension| match extension {
            0x55 => &[0x55][..],
            0x56 => &[0x56][..],
            _ => &[0x80][..],
        });
        packet.optional_header.as_mut().unwrap().pes_extension = Some(pes_extension);
        packet
    };
    let mut muxer = Muxer::new(MuxerConfig::new(RATE)).unwrap();
    for extension in [0x55, 0x56] {
        muxer
            .add_stream(
                StreamId::EXTENDED_STREAM_ID,
                Some(extension),
                StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                PStdBuffer::new(false, 8),
                &[],
            )
            .unwrap();
    }
    let mut bounds = Vec::new();
    let bound = |extension| {
        let mut bound = StreamBound::new(StreamBound::WITH_STREAM_ID_EXTENSION, false, 8);
        bound.stream_id_extension = Some(extension);
        bound
    };
    StreamBoundLoop::write_from([bound(0x55), bound(0x56)], &mut bounds).unwrap();
    let mut stream = Vec::new();
    let results = [
        muxer.handle_pes_packet(&extended(None, &[0x01])),
        muxer.handle_pes_packet(&extended(Some(0x80), &[0x01])),
        muxer.handle_pes_packet(&extended(Some(0x56), &[0x02])),
    ];
    muxer.poll_output(&mut stream);
    let mut system_header = SystemHeader::new(RATE, 0, 0);
    system_header.stream_bounds = StreamBoundLoop::from(bounds.as_slice());
    let mut header_bytes = Vec::new();
    system_header.write(&mut header_bytes).unwrap();
    let written_header = elements(&stream).into_iter().find_map(|(element, _)| {
        if let Element::SystemHeader(bytes) = element {
            Some(bytes.to_vec())
        } else {
            None
        }
    });
    let payloads: Vec<Vec<u8>> = pes_packets(&stream)
        .into_iter()
        .map(|packet| packet.payload.to_vec())
        .collect();
    let stream_id = StreamId::EXTENDED_STREAM_ID;
    assert_eq!(
        (results, written_header, payloads),
        (
            [
                Err(Error::StreamIdExtension { stream_id }),
                Err(Error::StreamIdExtension { stream_id }),
                Ok(())
            ],
            Some(header_bytes),
            vec![vec![0x02]]
        )
    );
}

#[test]
fn a_pes_packet_of_no_stream_registered_is_refused() {
    assert_eq!(
        muxer().handle_pes_packet(&pes_packet(0xe1, &[0xaa], Some(90_000))),
        Err(Error::UnregisteredStream {
            stream_id: StreamId::from(0xe1),
            stream_id_extension: None
        })
    );
}

#[test]
fn a_stream_whose_first_pes_packet_has_no_pts_or_whose_time_stamp_goes_back_is_refused() {
    let mut muxer = muxer();
    let first = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], None));
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(90_000)))
        .unwrap();
    let audio_first = muxer.handle_pes_packet(&pes_packet(AUDIO, &[0xbb], None));
    let back = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(89_999)));
    let unstamped = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], None));
    let audio_behind = muxer.handle_pes_packet(&pes_packet(AUDIO, &[0xbb], Some(80_000)));
    let missing_pts = |value| Error::MissingPts {
        stream_id: StreamId::from(value),
        stream_id_extension: None,
    };
    assert_eq!(
        [first, audio_first, back, unstamped, audio_behind],
        [
            Err(missing_pts(VIDEO)),
            Err(missing_pts(AUDIO)),
            Err(Error::DecreasingTimestamp {
                stream_id: StreamId::from(VIDEO),
                stream_id_extension: None,
                timestamp: timestamp(89_999),
                previous: timestamp(90_000)
            }),
            Ok(()),
            Ok(()),
        ]
    );
}

#[test]
fn a_first_pes_packet_without_a_time_stamp_is_refused_on_a_stream_without_an_optional_header() {
    let mut muxer = Muxer::new(MuxerConfig::new(RATE)).unwrap();
    muxer
        .add_stream(
            StreamId::PRIVATE_STREAM_2,
            None,
            StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
            PStdBuffer::new(false, 8),
            &[],
        )
        .unwrap();
    assert_eq!(
        muxer.handle_pes_packet(&PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc])),
        Err(Error::MissingPts {
            stream_id: StreamId::PRIVATE_STREAM_2,
            stream_id_extension: None
        })
    );
}

#[test]
fn a_time_stamp_past_the_wrap_of_33_bits_does_not_go_back() {
    let mut muxer = muxer();
    muxer
        .handle_pes_packet(&pes_packet(
            VIDEO,
            &[0xaa],
            Some(Timestamp::MODULUS - 1_800),
        ))
        .unwrap();
    assert_eq!(
        muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(1_800))),
        Ok(())
    );
}

#[test]
fn a_refused_pes_packet_leaves_the_muxer_as_it_was() {
    let packets = [
        pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
        pes_packet(AUDIO, &[0xbb; 300], Some(91_000)),
        pes_packet(VIDEO, &[0xaa; 3_000], Some(93_600)),
    ];
    let mut config = MuxerConfig::new(RATE);
    config.buffer_limit = 5_000;
    let mut muxer = muxer_of(config);
    let mut stream = Vec::new();
    muxer.handle_pes_packet(&packets[0]).unwrap();
    muxer.handle_pes_packet(&packets[1]).unwrap();
    // 2048 + 1051 + 331 held, and 2048 + 1003 to come
    let full = muxer.handle_pes_packet(&packets[2]);
    muxer.poll_output(&mut stream);
    let mut length_unspecified = packets[2].clone();
    length_unspecified.length_unspecified = true;
    let unspecified = muxer.handle_pes_packet(&length_unspecified);
    muxer.handle_pes_packet(&packets[2]).unwrap();
    muxer.finish();
    muxer.poll_output(&mut stream);
    assert_eq!(
        (full, unspecified, stream),
        (
            Err(Error::BufferFull { length: 6_481 }),
            Err(Error::LengthUnspecified {
                stream_id: StreamId::from(VIDEO)
            }),
            multiplexed_by(self::muxer(), &packets)
        )
    );
}

#[test]
fn a_buffer_limit_of_0_refuses_every_pes_packet_and_of_usize_max_none() {
    let mut config = MuxerConfig::new(RATE);
    config.buffer_limit = 0;
    let refused = muxer_of(config).handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)));
    config.buffer_limit = usize::MAX;
    let mut muxer = muxer_of(config);
    let taken: Vec<_> = (0..100)
        .map(|index| {
            muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 60_000], Some(index * 3_600)))
        })
        .collect();
    assert_eq!(
        (refused, taken),
        (Err(Error::BufferFull { length: 74 }), vec![Ok(()); 100])
    );
}

#[test]
fn a_leap_too_long_to_fill_within_the_buffer_limit_is_refused_however_much_is_taken() {
    let mut config = MuxerConfig::new(RATE);
    config.buffer_limit = 200;
    let mut muxer = muxer_of(config);
    let mut stream = Vec::new();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 10], Some(90_000)))
        .unwrap();
    muxer.poll_output(&mut stream);
    // A leap of 10 s takes 19 packs of a pack header alone, 266 bytes
    let leap = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 10], Some(990_000)));
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 10], Some(93_600)))
        .unwrap();
    muxer.finish();
    muxer.poll_output(&mut stream);
    assert_eq!(
        (leap, stream),
        (
            Err(Error::TimestampGap { gap: 900_000 }),
            multiplexed_by(
                self::muxer(),
                &[
                    pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                    pes_packet(VIDEO, &[0xaa; 10], Some(93_600)),
                ]
            )
        )
    );
}

#[test]
fn a_leap_is_filled_with_packs_of_a_pack_header_alone_no_more_than_0_7_s_apart() {
    let stream = multiplexed_by(
        muxer(),
        &[
            pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
            pes_packet(VIDEO, &[0xaa; 10], Some(270_000)),
        ],
    );
    let packs_alone = elements(&stream)
        .into_iter()
        .map(|(element, _)| element)
        .collect::<Vec<_>>()
        .windows(2)
        .filter(|pair| {
            matches!(
                pair,
                [
                    Element::PackHeader(_),
                    Element::PackHeader(_) | Element::ProgramEnd
                ]
            )
        })
        .count();
    let scrs = scrs(&stream);
    // From 0.3 s to 2.3 s, the first after 0.5 s, then every 0.5 s while
    // 2.3 s is more than 0.7 s on
    assert_eq!(
        (packs_alone, scrs),
        (
            3,
            vec![
                90_000 * 300 - DELAY,
                90_000 * 300 - DELAY + 13_500_000,
                90_000 * 300 - DELAY + 27_000_000,
                90_000 * 300 - DELAY + 40_500_000,
                270_000 * 300 - DELAY,
            ]
        )
    );
}

#[test]
fn the_scr_is_the_time_less_the_mux_delay_or_after_the_bytes_of_the_pack_before() {
    let mut config = MuxerConfig::new(RATE);
    config.mux_delay = Duration::ZERO;
    let stream = multiplexed_by(
        muxer_of(config),
        &[
            pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
            pes_packet(AUDIO, &[0xbb; 10], Some(90_000)),
            pes_packet(VIDEO, &[0xaa; 10], None),
            pes_packet(VIDEO, &[0xaa; 10], Some(93_600)),
        ],
    );
    // The first pack of 14 + 18 + 24 + 27 bytes, the next of 14 + 27, at
    // 1 260 000 bytes a second, 27 000 000 / 1 260 000 ticks a byte, rounded
    // up: 1 779 and 879 ticks
    assert_eq!(
        scrs(&stream),
        [
            27_000_000,
            27_000_000 + 1_779,
            27_000_000 + 1_779 + 879,
            93_600 * 300
        ]
    );
}

#[test]
#[should_panic(expected = "a stream is registered after the first PES packet")]
fn a_stream_registered_after_the_first_pes_packet_panics() {
    let mut muxer = muxer();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
        .unwrap();
    let _ = muxer.add_stream(
        StreamId::from(0xe1),
        None,
        StreamType::ISO_IEC_13818_2_VIDEO,
        VIDEO_BUFFER,
        &[],
    );
}

#[test]
#[should_panic(expected = "a PES packet is given after finish")]
fn a_pes_packet_after_finish_panics() {
    let mut muxer = muxer();
    muxer.finish();
    let _ = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)));
}

#[test]
fn after_finish_poll_output_gives_what_is_held_and_the_end_code_once() {
    let mut muxer = muxer();
    muxer
        .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(90_000)))
        .unwrap();
    let mut held = Vec::new();
    muxer.clone().poll_output(&mut held);
    muxer.finish();
    muxer.finish();
    let mut first = Vec::new();
    muxer.poll_output(&mut first);
    let mut second = Vec::new();
    muxer.poll_output(&mut second);
    let mut nothing_given = Muxer::new(MuxerConfig::new(RATE)).unwrap();
    nothing_given.finish();
    let mut empty = Vec::new();
    nothing_given.poll_output(&mut empty);
    assert_eq!(
        (first, second, empty),
        (
            [held, vec![0x00, 0x00, 0x01, 0xb9]].concat(),
            vec![],
            vec![]
        )
    );
}

#[test]
fn a_stream_is_taken_whose_descriptors_fill_the_map_and_refused_one_byte_past_it() {
    // A map of video and audio: 10 bytes besides its loop, two entries of
    // 4, and 4 for the stream added, leave 996 bytes of descriptors
    let descriptors = |length: usize| {
        let mut bytes = Vec::new();
        let mut rest = length;
        while rest > 0 {
            let body = (rest - 2).min(255);
            bytes.extend([0x80, u8::try_from(body).unwrap()]);
            bytes.extend(core::iter::repeat_n(0xaa, body));
            rest -= 2 + body;
        }
        bytes
    };
    let add = |es_info: &[u8]| {
        muxer().add_stream(
            StreamId::PRIVATE_STREAM_1,
            None,
            StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
            PStdBuffer::new(false, 8),
            es_info,
        )
    };
    assert_eq!(
        [add(&descriptors(996)), add(&descriptors(997))],
        [
            Ok(()),
            Err(Error::ProgramStreamMap {
                error: psm::Error::ProgramStreamMapLength {
                    program_stream_map_length: 1_019,
                    limit: 1_018
                }
            })
        ]
    );
}

#[test]
fn a_pes_packet_too_long_for_the_buffer_limit_is_refused_with_the_bytes_of_all_its_packs() {
    let mut config = MuxerConfig::new(RATE);
    config.pack_size = 128;
    config.buffer_limit = 128;
    let payload = vec![0xaa; 8 << 20];
    // The first pack: 14 + 18 + 24, and a PES packet header of 6 + 3 + 5 of
    // PTS + 3 of P-STD buffer, leaving 55 bytes of payload; the ones after
    // it: 14 and a header of 6 + 3 + 3, leaving 102. 82 241 packs follow
    // the first.
    let length = 42 + 82_242 * 14 + 17 + (8 << 20) + 82_241 * 12;
    assert_eq!(
        muxer_of(config).handle_pes_packet(&pes_packet(VIDEO, &payload, Some(90_000))),
        Err(Error::BufferFull { length })
    );
}
