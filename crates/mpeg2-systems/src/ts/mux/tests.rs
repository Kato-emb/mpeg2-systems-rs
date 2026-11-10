use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::descriptor::DescriptorLoop;
use crate::pes::OptionalHeader;
use crate::psi::VersionNumber;
use crate::psi::pat::{Program as PatProgram, ProgramAssociationSection, ProgramLoop};
use crate::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
use crate::stream_type::StreamType;
use crate::time::Timestamp;
use crate::ts::packet::TransportPacket;

const PMT_PID: u16 = 0x1000;
const VIDEO_PID: u16 = 0x0100;
const AUDIO_PID: u16 = 0x0101;

fn pid(value: u16) -> Pid {
    Pid::try_from(value).unwrap()
}

/// A PES packet of `stream_id` carrying `payload`, with a DTS (and a PTS
/// after it) of `dts` 90 kHz ticks, or no time stamp
fn pes_packet(stream_id: u8, payload: &[u8], dts: Option<u64>) -> PesPacket<'_> {
    let mut header = OptionalHeader::new();
    if let Some(dts) = dts {
        header.pts = Some(Timestamp::try_from(dts + 3_600).unwrap());
        header.dts = Some(Timestamp::try_from(dts).unwrap());
    }
    let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
    packet.optional_header = Some(header);
    packet
}

/// A muxer of `config` with program 1 and a video and an audio stream
fn muxer_of(config: MuxerConfig) -> Muxer {
    let mut muxer = Muxer::new(config).unwrap();
    muxer.add_program(1, pid(PMT_PID)).unwrap();
    muxer
        .add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
        )
        .unwrap();
    muxer
        .add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_3_AUDIO, pid(AUDIO_PID)),
        )
        .unwrap();
    muxer
}

/// The 90 kHz ticks the default `mux_delay` comes to
const DELAY: u64 = 63_000;

/// The PAT and the PMT [`muxer_of`] sends, written with `writer`
fn psi(writer: &mut PacketWriter, out: &mut Vec<u8>) {
    let mut programs = Vec::new();
    ProgramLoop::write_from(
        [PatProgram::ProgramMap {
            program_number: NonZeroU16::new(1).unwrap(),
            program_map_pid: pid(PMT_PID),
        }],
        &mut programs,
    )
    .unwrap();
    let mut pat = ProgramAssociationSection::new(1, VersionNumber(0));
    pat.programs = ProgramLoop::from(programs.as_slice());
    writer.write_program_association_section(&pat, out).unwrap();
    let mut streams = Vec::new();
    ElementaryStreamLoop::write_from(
        [
            ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
            ElementaryStream::new(StreamType::ISO_IEC_13818_3_AUDIO, pid(AUDIO_PID)),
        ],
        &mut streams,
    )
    .unwrap();
    let mut pmt = TsProgramMapSection::new(1, VersionNumber(0));
    pmt.pcr_pid = Some(pid(VIDEO_PID));
    pmt.elementary_streams = ElementaryStreamLoop::from(streams.as_slice());
    writer
        .write_ts_program_map_section(pid(PMT_PID), &pmt, out)
        .unwrap();
}

fn pcr(ticks: u64) -> ClockReference {
    ClockReference::from_fields(ticks / 300, u16::try_from(ticks % 300).unwrap())
}

/// What one input put in the output: the PID of each Transport Packet, and
/// whether it carries a PCR
fn summary(bytes: &[u8]) -> Vec<(u16, bool)> {
    bytes
        .chunks(188)
        .map(|chunk| {
            let packet = TransportPacket::read(chunk).unwrap();
            let carries_pcr = packet
                .adaptation_field
                .is_some_and(|field| field.program_clock_reference.is_some());
            (packet.pid.get(), carries_pcr)
        })
        .collect()
}

#[test]
fn the_first_pes_packet_on_the_pcr_pid_follows_the_pat_and_the_pmt_with_the_pcr_in_its_first_packet()
 {
    let mut muxer = muxer_of(MuxerConfig::new());
    let payload = vec![0xaa; 500];
    let packet = pes_packet(0xe0, &payload, Some(90_000));
    muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);

    let mut writer = PacketWriter::new();
    let mut expected = Vec::new();
    psi(&mut writer, &mut expected);
    writer
        .write_pes_packet(
            pid(VIDEO_PID),
            &packet,
            Some(pcr((90_000 - DELAY) * 300)),
            &mut expected,
        )
        .unwrap();
    assert_eq!(out, expected);
}

#[test]
fn the_first_pes_packet_off_the_pcr_pid_follows_a_packet_of_the_pcr_alone() {
    let mut muxer = muxer_of(MuxerConfig::new());
    let payload = vec![0xbb; 100];
    let packet = pes_packet(0xc0, &payload, Some(90_000));
    muxer.handle_pes_packet(pid(AUDIO_PID), &packet).unwrap();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);

    let mut writer = PacketWriter::new();
    let mut expected = Vec::new();
    psi(&mut writer, &mut expected);
    writer
        .write_program_clock_reference(pid(VIDEO_PID), pcr((90_000 - DELAY) * 300), &mut expected)
        .unwrap();
    writer
        .write_pes_packet(pid(AUDIO_PID), &packet, None, &mut expected)
        .unwrap();
    assert_eq!(out, expected);
}

#[test]
fn the_pat_and_the_pmt_and_the_pcr_are_sent_again_once_their_interval_has_passed() {
    let mut muxer = muxer_of(MuxerConfig::new());
    let payload = [0xcc; 10];
    let mut inputs = Vec::new();
    for index in 0..8 {
        let dts = 90_000 + index * 1_800;
        let mut out = Vec::new();
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(dts)))
            .unwrap();
        muxer.poll_output(&mut out);
        inputs.push(summary(&out));
        out.clear();
        muxer
            .handle_pes_packet(pid(AUDIO_PID), &pes_packet(0xc0, &payload, Some(dts + 900)))
            .unwrap();
        muxer
            .handle_pes_packet(pid(AUDIO_PID), &pes_packet(0xc0, &payload, None))
            .unwrap();
        muxer.poll_output(&mut out);
        inputs.push(summary(&out));
    }

    let (pat, pmt) = ((0, false), (PMT_PID, false));
    let video = |pcr| vec![(VIDEO_PID, pcr)];
    let audio = vec![(AUDIO_PID, false); 2];
    let with_psi = |mut packets: Vec<(u16, bool)>| {
        packets.splice(0..0, [pat, pmt]);
        packets
    };
    assert_eq!(
        inputs,
        [
            with_psi(video(true)),
            audio.clone(),
            video(false),
            audio.clone(),
            video(true),
            audio.clone(),
            video(false),
            audio.clone(),
            video(true),
            audio.clone(),
            with_psi(video(false)),
            audio.clone(),
            video(true),
            audio.clone(),
            video(false),
            audio.clone(),
        ]
    );
}

#[test]
fn without_a_stream_on_the_pcr_pid_the_pcr_goes_alone() {
    let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
    muxer.add_program(1, pid(PMT_PID)).unwrap();
    muxer.set_pcr_pid(1, pid(0x01ff)).unwrap();
    muxer
        .add_stream(
            1,
            ElementaryStream::new(StreamType::AVC_VIDEO, pid(VIDEO_PID)),
        )
        .unwrap();
    muxer
        .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
        .unwrap();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    assert_eq!(
        summary(&out),
        [
            (0, false),
            (PMT_PID, false),
            (0x01ff, true),
            (VIDEO_PID, false)
        ]
    );
}

#[test]
fn without_a_pcr_pid_the_pcr_goes_on_the_first_stream_of_a_video_stream_type_or_else_the_first_stream()
 {
    let first_pcr_pid = |stream_types: &[StreamType]| {
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        for (pid_value, stream_type) in (0x0100..).zip(stream_types) {
            muxer
                .add_stream(1, ElementaryStream::new(*stream_type, pid(pid_value)))
                .unwrap();
        }
        muxer
            .program
            .as_ref()
            .and_then(ProgramDefinition::pcr_pid)
            .map(Pid::get)
    };
    assert_eq!(
        [
            first_pcr_pid(&[StreamType::ISO_IEC_13818_7_AUDIO, StreamType::HEVC_VIDEO]),
            first_pcr_pid(&[
                StreamType::ISO_IEC_13818_7_AUDIO,
                StreamType::ISO_IEC_13818_2_VIDEO
            ]),
            first_pcr_pid(&[StreamType::ISO_IEC_13818_7_AUDIO, StreamType::AVC_VIDEO]),
            first_pcr_pid(&[
                StreamType::ISO_IEC_13818_7_AUDIO,
                StreamType::SVC_VIDEO_SUB_BITSTREAM
            ]),
            first_pcr_pid(&[
                StreamType::SVC_VIDEO_SUB_BITSTREAM,
                StreamType::ISO_IEC_13818_7_AUDIO
            ]),
            first_pcr_pid(&[
                StreamType::ISO_IEC_13818_7_AUDIO,
                StreamType::LCEVC_VIDEO,
                StreamType::ISO_IEC_13818_3_AUDIO
            ]),
        ],
        [
            Some(0x0101),
            Some(0x0101),
            Some(0x0101),
            Some(0x0100),
            Some(0x0100),
            Some(0x0100)
        ]
    );
}

#[test]
fn an_input_past_the_buffer_limit_is_refused_and_taken_once_the_output_is() {
    let payload = vec![0xdd; 1_000];
    let first = pes_packet(0xe0, &payload, Some(90_000));
    let second = pes_packet(0xe0, &payload, Some(93_600));
    let mut unbounded = muxer_of(MuxerConfig::new());
    unbounded.handle_pes_packet(pid(VIDEO_PID), &first).unwrap();
    let mut first_output = Vec::new();
    unbounded.poll_output(&mut first_output);

    let mut config = MuxerConfig::new();
    config.buffer_limit = first_output.len();
    let mut bounded = muxer_of(config);
    bounded.handle_pes_packet(pid(VIDEO_PID), &first).unwrap();
    let refused = bounded.handle_pes_packet(pid(VIDEO_PID), &second);
    let mut out = Vec::new();
    bounded.poll_output(&mut out);
    bounded.handle_pes_packet(pid(VIDEO_PID), &second).unwrap();
    bounded.poll_output(&mut out);

    unbounded
        .handle_pes_packet(pid(VIDEO_PID), &second)
        .unwrap();
    let mut expected = first_output.clone();
    unbounded.poll_output(&mut expected);
    assert_eq!(
        (refused, out),
        (
            Err(Error::BufferFull {
                length: first_output.len() + 6 * 188
            }),
            expected
        )
    );
}

#[test]
fn an_input_alone_past_the_buffer_limit_is_refused() {
    let mut config = MuxerConfig::new();
    config.buffer_limit = 3 * 188;
    let mut muxer = muxer_of(config);
    let payload = vec![0xee; 200];
    let refused =
        muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(90_000)));
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    assert_eq!(
        (refused, out),
        (Err(Error::BufferFull { length: 4 * 188 }), Vec::new())
    );
}

#[test]
fn a_refused_pes_packet_leaves_the_muxer_as_it_was() {
    let mut muxer = muxer_of(MuxerConfig::new());
    let payload = vec![0x11; 70_000];
    let too_long = pes_packet(0xc0, &payload, Some(90_000));
    let too_long_error = too_long.write(&mut Vec::new()).unwrap_err();
    let results = [
        muxer.handle_pes_packet(pid(0x0200), &pes_packet(0xe0, &[], Some(90_000))),
        muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xbb, &[], Some(90_000))),
        muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xbc, &[], Some(90_000))),
        muxer.handle_pes_packet(pid(AUDIO_PID), &too_long),
    ];
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    let mut fresh = muxer_of(MuxerConfig::new());
    let packet = pes_packet(0xe0, &[], Some(90_000));
    muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
    fresh.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
    let (mut written, mut expected) = (Vec::new(), Vec::new());
    muxer.poll_output(&mut written);
    fresh.poll_output(&mut expected);
    assert_eq!(
        (results, out, written),
        (
            [
                Err(Error::UnregisteredPid { pid: pid(0x0200) }),
                Err(Error::StreamId {
                    stream_id: StreamId::from(0xbb)
                }),
                Err(Error::StreamId {
                    stream_id: StreamId::from(0xbc)
                }),
                Err(Error::PesPacket {
                    error: too_long_error
                }),
            ],
            Vec::new(),
            expected
        )
    );
}

#[test]
fn a_program_is_refused_for_program_number_0_or_a_pid_not_free_to_assign() {
    let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
    let results = [
        muxer.add_program(0, pid(PMT_PID)),
        muxer.add_program(1, pid(0x000f)),
        muxer.add_program(1, pid(PMT_PID)),
    ];
    assert_eq!(
        results,
        [
            Err(Error::ProgramNumber { program_number: 0 }),
            Err(Error::Pid { pid: pid(0x000f) }),
            Ok(()),
        ]
    );
}

#[test]
fn the_pcr_pid_is_refused_for_a_program_not_registered_or_a_pid_not_free_to_assign() {
    let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
    let before_program = muxer.set_pcr_pid(1, pid(VIDEO_PID));
    muxer.add_program(1, pid(PMT_PID)).unwrap();
    assert_eq!(
        [
            before_program,
            muxer.set_pcr_pid(2, pid(VIDEO_PID)),
            muxer.set_pcr_pid(1, Pid::NULL),
        ],
        [
            Err(Error::UnregisteredProgram { program_number: 1 }),
            Err(Error::UnregisteredProgram { program_number: 2 }),
            Err(Error::Pid { pid: Pid::NULL }),
        ]
    );
}

#[test]
fn the_pcr_pid_set_last_is_the_one_the_pcr_goes_on() {
    let output = |pcr_pids: &[u16]| {
        let mut muxer = muxer_of(MuxerConfig::new());
        for &pcr_pid in pcr_pids {
            muxer.set_pcr_pid(1, pid(pcr_pid)).unwrap();
        }
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
            .unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        out
    };
    assert_eq!(output(&[0x01fe, AUDIO_PID]), output(&[AUDIO_PID]));
}

#[test]
fn program_info_is_written_in_the_pmt() {
    let descriptors = [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56];
    let mut muxer = muxer_of(MuxerConfig::new());
    muxer
        .set_program_info(1, DescriptorLoop::from(&[0x05, 0x00][..]))
        .unwrap();
    muxer
        .set_program_info(1, DescriptorLoop::from(descriptors.as_slice()))
        .unwrap();
    muxer
        .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
        .unwrap();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    let pmt_packet = TransportPacket::read(out.get(188..376).unwrap()).unwrap();
    let section_bytes = pmt_packet.payload.unwrap().get(1..).unwrap();
    let section_length = section::encoded_len(section_bytes).unwrap();
    let section = section::Section::read(section_bytes.get(..section_length).unwrap()).unwrap();
    assert_eq!(
        TsProgramMapSection::try_from(&section)
            .unwrap()
            .program_info,
        DescriptorLoop::from(descriptors.as_slice())
    );
}

#[test]
fn program_info_refused_leaves_the_muxer_as_it_was() {
    let broken = [0x05, 0x04, 0x00];
    let mut pmt = TsProgramMapSection::new(1, VersionNumber(0));
    pmt.program_info = broken.as_slice().into();
    let broken_error = pmt.write(&mut Vec::new()).unwrap_err();
    let past_the_limit = descriptors(1_009);
    let output = |refused: &[&[u8]]| {
        let mut muxer = muxer_of(MuxerConfig::new());
        let errors: Vec<_> = refused
            .iter()
            .map(|program_info| muxer.set_program_info(1, DescriptorLoop::from(*program_info)))
            .collect();
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[1, 2, 3], Some(90_000)))
            .unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        (errors, out)
    };
    let (errors, out) = output(&[&broken, &past_the_limit]);
    assert_eq!(
        (
            errors,
            Muxer::new(MuxerConfig::new())
                .unwrap()
                .set_program_info(1, DescriptorLoop::from(&[][..])),
            out
        ),
        (
            vec![
                Err(Error::TsProgramMapSection {
                    error: broken_error
                }),
                Err(Error::TsProgramMapSection {
                    error: pmt::Error::Section(section::Error::SectionLength {
                        section_length: 1_032,
                        limit: 1_021
                    })
                }),
            ],
            Err(Error::UnregisteredProgram { program_number: 1 }),
            output(&[]).1
        )
    );
}

#[test]
fn a_stream_is_refused_off_the_program_on_a_pid_in_use_or_for_its_descriptors() {
    let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
    let before_the_program = muxer.add_stream(
        1,
        ElementaryStream::new(StreamType::AVC_VIDEO, pid(VIDEO_PID)),
    );
    muxer.add_program(1, pid(PMT_PID)).unwrap();
    muxer
        .add_stream(
            1,
            ElementaryStream::new(StreamType::AVC_VIDEO, pid(VIDEO_PID)),
        )
        .unwrap();
    let too_long = vec![0u8; 1_024];
    let results = [
        before_the_program,
        muxer.add_stream(
            2,
            ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(AUDIO_PID)),
        ),
        muxer.add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, Pid::NULL),
        ),
        muxer.add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(VIDEO_PID)),
        ),
        muxer.add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(PMT_PID)),
        ),
        muxer.add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(AUDIO_PID))
                .with_es_info(DescriptorLoop::from(too_long.as_slice())),
        ),
        muxer.add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_7_AUDIO, pid(AUDIO_PID)),
        ),
    ];
    assert_eq!(
        results,
        [
            Err(Error::UnregisteredProgram { program_number: 1 }),
            Err(Error::UnregisteredProgram { program_number: 2 }),
            Err(Error::Pid { pid: Pid::NULL }),
            Err(Error::PidInUse {
                pid: pid(VIDEO_PID)
            }),
            Err(Error::PidInUse { pid: pid(PMT_PID) }),
            Err(Error::TsProgramMapSection {
                error: pmt::Error::EsInfoLength { length: 1_024 }
            }),
            Ok(()),
        ]
    );
}

#[test]
#[should_panic(expected = "a program is registered already")]
fn a_second_program_panics() {
    let mut muxer = muxer_of(MuxerConfig::new());
    let _ = muxer.add_program(2, pid(0x1001));
}

#[test]
#[should_panic(expected = "a stream is registered after the first PES packet")]
fn a_stream_after_the_first_pes_packet_panics() {
    let mut muxer = muxer_of(MuxerConfig::new());
    muxer
        .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)))
        .unwrap();
    let _ = muxer.add_stream(1, ElementaryStream::new(StreamType::AVC_VIDEO, pid(0x0102)));
}

#[test]
#[should_panic(expected = "the PCR PID is set after the first PES packet")]
fn the_pcr_pid_after_the_first_pes_packet_panics() {
    let mut muxer = muxer_of(MuxerConfig::new());
    muxer
        .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)))
        .unwrap();
    let _ = muxer.set_pcr_pid(1, pid(AUDIO_PID));
}

#[test]
#[should_panic(expected = "the program info is set after the first PES packet")]
fn program_info_after_the_first_pes_packet_panics() {
    let mut muxer = muxer_of(MuxerConfig::new());
    muxer
        .handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)))
        .unwrap();
    let _ = muxer.set_program_info(1, DescriptorLoop::from(&[][..]));
}

#[test]
#[should_panic(expected = "a PES packet is given after finish")]
fn a_pes_packet_after_finish_panics() {
    let mut muxer = muxer_of(MuxerConfig::new());
    muxer.finish();
    let _ = muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &[], Some(90_000)));
}

#[test]
fn after_finish_poll_output_gives_what_is_held_and_a_second_finish_does_nothing() {
    let mut muxer = muxer_of(MuxerConfig::new());
    let packet = pes_packet(0xe0, &[1], Some(90_000));
    muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
    muxer.finish();
    muxer.finish();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    let mut fresh = muxer_of(MuxerConfig::new());
    fresh.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
    fresh.finish();
    let mut expected = Vec::new();
    fresh.poll_output(&mut expected);
    assert_eq!(out, expected);
}

#[test]
fn the_packets_counted_are_those_a_payload_unit_is_cut_into() {
    let mut adaptation_field = AdaptationField::new();
    adaptation_field.program_clock_reference = Some(pcr(0));
    let mut counted = Vec::new();
    let mut written = Vec::new();
    for length in 1..=600 {
        for field in [None, Some(adaptation_field.clone())] {
            let field_length = field.as_ref().map_or(0, AdaptationField::encoded_len);
            counted.push(packet_count(length, field_length));
            let mut out = Vec::new();
            let unit = vec![0; length];
            written.push(
                PacketWriter::new()
                    .write_payload_unit(
                        pid(VIDEO_PID),
                        |out| {
                            out.extend_from_slice(&unit);
                            Ok(())
                        },
                        field,
                        Stuffing::AdaptationField,
                        &mut out,
                    )
                    .unwrap(),
            );
        }
    }
    assert_eq!(counted, written);
}

#[test]
fn the_pcr_is_the_dts_less_the_mux_delay_and_never_goes_back() {
    let mut config = MuxerConfig::new();
    config.pcr_interval = Duration::from_millis(100);
    let mut muxer = muxer_of(config);
    let mut pcrs = Vec::new();
    for (stream_pid, dts) in [
        (VIDEO_PID, 90_000),
        (VIDEO_PID, 99_000),
        (AUDIO_PID, 95_000),
        (VIDEO_PID, 108_000),
    ] {
        muxer
            .handle_pes_packet(pid(stream_pid), &pes_packet(0xe0, &[0], Some(dts)))
            .unwrap();
        let mut out = Vec::new();
        muxer.poll_output(&mut out);
        pcrs.extend(out.chunks(188).filter_map(|chunk| {
            TransportPacket::read(chunk)
                .unwrap()
                .adaptation_field
                .and_then(|field| field.program_clock_reference)
                .map(ClockReference::ticks)
        }));
    }
    assert_eq!(pcrs, [27_000 * 300, 36_000 * 300, 45_000 * 300]);
}

/// A PES packet of `stream_id` whose PTS and DTS are both `timestamp`, taken
/// round the 33-bit circle
fn stamped(stream_id: u8, timestamp: u64) -> PesPacket<'static> {
    let timestamp = Timestamp::try_from(timestamp % Timestamp::MODULUS).unwrap();
    let mut header = OptionalHeader::new();
    header.pts = Some(timestamp);
    header.dts = Some(timestamp);
    let mut packet = PesPacket::new(StreamId::from(stream_id), &[0x5a; 10]);
    packet.optional_header = Some(header);
    packet
}

/// How many PCRs `stream` carries, the longest time from one to the next
/// round the circle, and how many PATs it carries
fn timing_of(stream: &[u8]) -> (usize, u64, usize) {
    let packets: Vec<TransportPacket<'_>> = stream
        .chunks(188)
        .map(|chunk| TransportPacket::read(chunk).unwrap())
        .collect();
    let pcrs: Vec<u64> = packets
        .iter()
        .filter_map(|packet| {
            packet
                .adaptation_field
                .as_ref()
                .and_then(|field| field.program_clock_reference)
                .map(ClockReference::ticks)
        })
        .collect();
    let longest = pcrs
        .windows(2)
        .map(|pair| match pair {
            [earlier, later] => {
                (later + ClockReference::MODULUS - earlier) % ClockReference::MODULUS
            }
            _ => 0,
        })
        .max()
        .unwrap_or(0);
    let pats = packets
        .iter()
        .filter(|packet| packet.pid == Pid::PAT)
        .count();
    (pcrs.len(), longest, pats)
}

/// The stream a muxer of the default settings writes of one video stream
/// whose PTS and DTS are each of `timestamps`
fn multiplexed(timestamps: impl IntoIterator<Item = u64>) -> Vec<u8> {
    let mut muxer = muxer_of(MuxerConfig::new());
    let mut out = Vec::new();
    for timestamp in timestamps {
        muxer
            .handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, timestamp))
            .unwrap();
        muxer.poll_output(&mut out);
    }
    out
}

/// 27 MHz ticks of 40 ms, the default `pcr_interval`
const PCR_INTERVAL: u64 = 40 * 27_000;

#[test]
fn time_stamps_below_the_mux_delay_keep_the_pcr_and_the_pat_and_the_pmt_coming() {
    let stream = multiplexed((0..=100).map(|index| index * 1_800));
    assert_eq!(timing_of(&stream), (51, PCR_INTERVAL, 21));
}

#[test]
fn time_stamps_across_the_wrap_of_33_bits_keep_the_pcr_and_the_pat_and_the_pmt_coming() {
    let stream = multiplexed((0..=100).map(|index| Timestamp::MODULUS - 45_000 + index * 1_800));
    assert_eq!(timing_of(&stream), (51, PCR_INTERVAL, 21));
}

#[test]
fn a_pes_packet_past_the_buffer_limit_is_refused_before_it_is_copied() {
    let mut config = MuxerConfig::new();
    config.buffer_limit = 3 * 188;
    let mut muxer = muxer_of(config);
    let payload = vec![0x77; 10_000];
    let refused =
        muxer.handle_pes_packet(pid(VIDEO_PID), &pes_packet(0xe0, &payload, Some(90_000)));
    assert_eq!((refused.is_err(), muxer.scratch.capacity()), (true, 0));
}

#[test]
fn a_leap_of_the_time_stamps_is_filled_with_pcrs_and_the_pat_and_the_pmt_at_their_intervals() {
    let stream = multiplexed([90_000, 180_000]);
    assert_eq!(timing_of(&stream), (26, PCR_INTERVAL, 11));
}

#[test]
fn a_leap_too_long_to_fill_within_the_buffer_limit_is_refused_however_much_is_taken() {
    let mut config = MuxerConfig::new();
    config.buffer_limit = 100 * 188;
    let mut muxer = muxer_of(config);
    muxer
        .handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 90_000))
        .unwrap();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    let leap = stamped(0xe0, 90_000 + 10 * 90_000);
    let refused = muxer.handle_pes_packet(pid(VIDEO_PID), &leap);
    muxer.poll_output(&mut out);
    let refused_again = muxer.handle_pes_packet(pid(VIDEO_PID), &leap);
    let next = stamped(0xe0, 91_800);
    muxer.handle_pes_packet(pid(VIDEO_PID), &next).unwrap();
    muxer.poll_output(&mut out);

    let mut expected = Vec::new();
    let mut fresh = muxer_of(config);
    for packet in [stamped(0xe0, 90_000), next] {
        fresh.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
        fresh.poll_output(&mut expected);
    }
    let gap = Err(Error::TimestampGap { gap: 10 * 90_000 });
    assert_eq!((refused, refused_again, out), (gap, gap, expected));
}

/// A PES packet of `stream_id` without a PTS or a DTS
fn unstamped(stream_id: u8) -> PesPacket<'static> {
    let mut packet = PesPacket::new(StreamId::from(stream_id), &[0x5a; 10]);
    packet.optional_header = Some(OptionalHeader::new());
    packet
}

#[test]
fn a_stream_whose_first_pes_packet_has_no_pts_or_whose_dts_goes_back_is_refused() {
    let mut muxer = muxer_of(MuxerConfig::new());
    let results = [
        muxer.handle_pes_packet(pid(VIDEO_PID), &unstamped(0xe0)),
        muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 180_000)),
        muxer.handle_pes_packet(pid(AUDIO_PID), &unstamped(0xc0)),
        muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 90_000)),
        muxer.handle_pes_packet(pid(AUDIO_PID), &stamped(0xc0, 90_000)),
        muxer.handle_pes_packet(pid(VIDEO_PID), &unstamped(0xe0)),
        muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 181_800)),
    ];
    let mut out = Vec::new();
    muxer.poll_output(&mut out);

    let mut fresh = muxer_of(MuxerConfig::new());
    let mut expected = Vec::new();
    for (stream_pid, packet) in [
        (VIDEO_PID, stamped(0xe0, 180_000)),
        (AUDIO_PID, stamped(0xc0, 90_000)),
        (VIDEO_PID, unstamped(0xe0)),
        (VIDEO_PID, stamped(0xe0, 181_800)),
    ] {
        fresh.handle_pes_packet(pid(stream_pid), &packet).unwrap();
    }
    fresh.poll_output(&mut expected);
    let timestamp = |value| Timestamp::try_from(value).unwrap();
    assert_eq!(
        (results, out),
        (
            [
                Err(Error::MissingPts {
                    pid: pid(VIDEO_PID)
                }),
                Ok(()),
                Err(Error::MissingPts {
                    pid: pid(AUDIO_PID)
                }),
                Err(Error::DecreasingTimestamp {
                    pid: pid(VIDEO_PID),
                    timestamp: timestamp(90_000),
                    previous: timestamp(180_000),
                }),
                Ok(()),
                Ok(()),
                Ok(()),
            ],
            expected
        )
    );
}

#[test]
fn a_dts_past_the_wrap_of_33_bits_does_not_go_back() {
    let mut muxer = muxer_of(MuxerConfig::new());
    let results = [Timestamp::MODULUS - 1_800, Timestamp::MODULUS + 1_800]
        .map(|timestamp| muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, timestamp)));
    assert_eq!(results, [Ok(()), Ok(())]);
}

#[test]
fn a_stream_of_one_pcr_ends_with_a_second_one_pcr_interval_on_and_a_longer_one_with_none_added() {
    let finished = |timestamps: &[u64]| {
        let mut muxer = muxer_of(MuxerConfig::new());
        let mut out = Vec::new();
        for &timestamp in timestamps {
            muxer
                .handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, timestamp))
                .unwrap();
        }
        muxer.poll_output(&mut out);
        let before = timing_of(&out).0;
        muxer.finish();
        muxer.poll_output(&mut out);
        muxer.poll_output(&mut out);
        let pcrs: Vec<u64> = out
            .chunks(188)
            .filter_map(|chunk| {
                TransportPacket::read(chunk)
                    .unwrap()
                    .adaptation_field
                    .and_then(|field| field.program_clock_reference)
                    .map(ClockReference::ticks)
            })
            .collect();
        (before, pcrs.len(), pcrs.last().copied())
    };
    let first = (90_000 - DELAY) * 300;
    assert_eq!(
        [
            finished(&[90_000]),
            finished(&[90_000, 91_800, 93_600, 95_400])
        ],
        [
            (1, 2, Some(first + PCR_INTERVAL)),
            (2, 2, Some(first + PCR_INTERVAL))
        ]
    );
}

#[test]
fn a_mux_delay_longer_than_the_time_stamp_gives_the_pcr_round_the_circle() {
    let mut config = MuxerConfig::new();
    config.mux_delay = Duration::from_secs(96_000);
    let mut muxer = muxer_of(config);
    let mut header = OptionalHeader::new();
    header.pts = Some(Timestamp::try_from(0).unwrap());
    let mut packet = PesPacket::new(StreamId::from(0xe0), &[1]);
    packet.optional_header = Some(header);
    muxer.handle_pes_packet(pid(VIDEO_PID), &packet).unwrap();
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    let pcrs: Vec<u64> = out
        .chunks(188)
        .filter_map(|chunk| {
            TransportPacket::read(chunk)
                .unwrap()
                .adaptation_field
                .and_then(|field| field.program_clock_reference)
                .map(ClockReference::ticks)
        })
        .collect();
    let delay = 96_000 * 27_000_000 % ClockReference::MODULUS;
    assert_eq!(pcrs, [ClockReference::MODULUS - delay]);
}

/// The shortest `pcr_interval` a muxer takes: one tick of the 27 MHz system
/// clock, rounded up to whole nanoseconds
const ONE_TICK: Duration = Duration::from_nanos(38);

#[test]
fn a_pcr_interval_of_no_tick_of_the_stc_or_past_0_1_s_is_refused() {
    let with = |pcr_interval| {
        let mut config = MuxerConfig::new();
        config.pcr_interval = pcr_interval;
        Muxer::new(config).map(|_| ())
    };
    let refused = |pcr_interval| Err(Error::PcrInterval { pcr_interval });
    let longest = Duration::from_millis(100);
    let past_longest = longest + Duration::from_nanos(1);
    assert_eq!(
        [
            Duration::ZERO,
            Duration::from_nanos(1),
            past_longest,
            longest,
            ONE_TICK
        ]
        .map(with),
        [
            refused(Duration::ZERO),
            refused(Duration::from_nanos(1)),
            refused(past_longest),
            Ok(()),
            Ok(())
        ]
    );
}

#[test]
fn the_shortest_pcr_interval_never_sends_two_pcrs_of_one_value() {
    let mut config = MuxerConfig::new();
    config.pcr_interval = ONE_TICK;
    let mut muxer = muxer_of(config);
    let mut out = Vec::new();
    for (stream_pid, stream_id, timestamp) in [
        (VIDEO_PID, 0xe0, 90_000),
        (AUDIO_PID, 0xc0, 90_000),
        (VIDEO_PID, 0xe0, 90_001),
    ] {
        muxer
            .handle_pes_packet(pid(stream_pid), &stamped(stream_id, timestamp))
            .unwrap();
        muxer.poll_output(&mut out);
    }
    muxer.finish();
    muxer.poll_output(&mut out);
    let pcrs: Vec<u64> = out
        .chunks(188)
        .filter_map(|chunk| {
            TransportPacket::read(chunk)
                .unwrap()
                .adaptation_field
                .and_then(|field| field.program_clock_reference)
                .map(ClockReference::ticks)
        })
        .collect();
    let rising = pcrs.windows(2).all(|pair| match pair {
        [earlier, later] => later > earlier,
        _ => false,
    });
    assert_eq!((pcrs.len(), rising), (301, true));
}

/// For each input given a muxer of `config`, how many PATs and PMTs its
/// output carries, and whether they all come before the PES packet
fn psi_of_each(config: MuxerConfig, inputs: &[(u16, u8, u64)]) -> Vec<(usize, usize, bool)> {
    let mut muxer = muxer_of(config);
    inputs
        .iter()
        .map(|&(stream_pid, stream_id, timestamp)| {
            muxer
                .handle_pes_packet(pid(stream_pid), &stamped(stream_id, timestamp))
                .unwrap();
            let mut out = Vec::new();
            muxer.poll_output(&mut out);
            let packets: Vec<TransportPacket<'_>> = out
                .chunks(188)
                .map(|chunk| TransportPacket::read(chunk).unwrap())
                .collect();
            let is_psi =
                |packet: &TransportPacket<'_>| packet.pid == Pid::PAT || packet.pid == pid(PMT_PID);
            let pes_start = packets
                .iter()
                .position(|packet| {
                    packet.pid == pid(stream_pid) && packet.payload_unit_start_indicator
                })
                .unwrap();
            (
                packets
                    .iter()
                    .filter(|packet| packet.pid == Pid::PAT)
                    .count(),
                packets
                    .iter()
                    .filter(|packet| packet.pid == pid(PMT_PID))
                    .count(),
                packets.iter().skip(pes_start).all(|packet| !is_psi(packet)),
            )
        })
        .collect()
}

#[test]
fn a_psi_interval_of_0_sends_the_pat_and_the_pmt_before_every_pes_packet() {
    let mut config = MuxerConfig::new();
    config.psi_interval = Duration::ZERO;
    let inputs = [
        (VIDEO_PID, 0xe0, 90_000),
        (AUDIO_PID, 0xc0, 90_000),
        (VIDEO_PID, 0xe0, 91_800),
        (VIDEO_PID, 0xe0, 181_800),
    ];
    assert_eq!(psi_of_each(config, &inputs), vec![(1, 1, true); 4]);
}

#[test]
fn a_psi_interval_of_duration_max_sends_the_pat_and_the_pmt_before_the_first_pes_packet_only() {
    let mut config = MuxerConfig::new();
    config.psi_interval = Duration::MAX;
    let inputs: Vec<(u16, u8, u64)> = (0..1_000)
        .map(|index| (VIDEO_PID, 0xe0, 90_000 + index * 1_800))
        .chain([(VIDEO_PID, 0xe0, 90_000 + 999 * 1_800 + 10 * 90_000)])
        .collect();
    let expected: Vec<(usize, usize, bool)> = core::iter::once((1, 1, true))
        .chain(core::iter::repeat_n((0, 0, true), inputs.len() - 1))
        .collect();
    assert_eq!(psi_of_each(config, &inputs), expected);
}

/// Whole descriptors of tag 0x80 that come to `length` bytes, which is not 1
fn descriptors(length: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut rest = length;
    while rest > 0 {
        let body = (rest - 2).min(255);
        bytes.extend([0x80, u8::try_from(body).unwrap()]);
        bytes.extend(core::iter::repeat_n(0xaa, body));
        rest -= 2 + body;
    }
    bytes
}

#[test]
fn program_info_is_taken_that_fills_the_pmt_and_refused_one_byte_past_it() {
    // A PMT without streams: 13 bytes of `section_length` besides its
    // descriptors leave 1008 of the 1021
    let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
    muxer.add_program(1, pid(PMT_PID)).unwrap();
    assert_eq!(
        [
            muxer.set_program_info(1, DescriptorLoop::from(descriptors(1_009).as_slice())),
            muxer.set_program_info(1, DescriptorLoop::from(descriptors(1_008).as_slice())),
        ],
        [
            Err(Error::TsProgramMapSection {
                error: pmt::Error::Section(section::Error::SectionLength {
                    section_length: 1_022,
                    limit: 1_021
                })
            }),
            Ok(()),
        ]
    );
}

#[test]
fn a_stream_is_taken_whose_descriptors_fill_the_pmt_and_refused_one_byte_past_it() {
    // A PMT of video and audio: 13 bytes besides its loops, two entries of
    // 5, and 5 for the stream added, leave 993 bytes of descriptors
    let mut muxer = muxer_of(MuxerConfig::new());
    let mut add = |es_info: &[u8]| {
        muxer.add_stream(
            1,
            ElementaryStream::new(StreamType::AVC_VIDEO, pid(0x0102))
                .with_es_info(DescriptorLoop::from(es_info)),
        )
    };
    assert_eq!(
        [add(&descriptors(994)), add(&descriptors(993))],
        [
            Err(Error::TsProgramMapSection {
                error: pmt::Error::Section(section::Error::SectionLength {
                    section_length: 1_022,
                    limit: 1_021
                })
            }),
            Ok(()),
        ]
    );
}

const PRIVATE_PID: u16 = 0x0102;

/// A muxer of the default settings with program 1, a video stream and a
/// stream of `private_stream_2`
fn muxer_with_private_stream_2() -> Muxer {
    let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
    muxer.add_program(1, pid(PMT_PID)).unwrap();
    muxer
        .add_stream(
            1,
            ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
        )
        .unwrap();
    muxer
        .add_stream(
            1,
            ElementaryStream::new(
                StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                pid(PRIVATE_PID),
            ),
        )
        .unwrap();
    muxer
}

#[test]
fn a_stream_without_an_optional_header_is_taken_without_a_pts_after_the_first_pes_packet() {
    let mut muxer = muxer_with_private_stream_2();
    let private = PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc; 10]);
    let results = [
        muxer.handle_pes_packet(pid(VIDEO_PID), &stamped(0xe0, 90_000)),
        muxer.handle_pes_packet(pid(PRIVATE_PID), &private),
        muxer.handle_pes_packet(pid(PRIVATE_PID), &private),
    ];
    let mut out = Vec::new();
    muxer.poll_output(&mut out);
    let private_packets = out
        .chunks(188)
        .filter(|chunk| TransportPacket::read(chunk).unwrap().pid == pid(PRIVATE_PID))
        .count();
    assert_eq!((results, private_packets), ([Ok(()), Ok(()), Ok(())], 2));
}

#[test]
fn a_first_pes_packet_without_a_time_stamp_is_refused_on_a_stream_without_an_optional_header() {
    let mut muxer = muxer_with_private_stream_2();
    assert_eq!(
        muxer.handle_pes_packet(
            pid(PRIVATE_PID),
            &PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc])
        ),
        Err(Error::MissingPts {
            pid: pid(PRIVATE_PID)
        })
    );
}

#[test]
fn a_config_built_from_new_and_every_with_method_equals_one_with_every_field_assigned() {
    let mut assigned = MuxerConfig::new();
    assigned.transport_stream_id = 0x7fe0;
    assigned.psi_interval = Duration::from_millis(50);
    assigned.pcr_interval = Duration::from_millis(30);
    assigned.mux_delay = Duration::from_millis(500);
    assigned.buffer_limit = 1024;

    let built = MuxerConfig::new()
        .with_transport_stream_id(0x7fe0)
        .with_psi_interval(Duration::from_millis(50))
        .with_pcr_interval(Duration::from_millis(30))
        .with_mux_delay(Duration::from_millis(500))
        .with_buffer_limit(1024);

    assert_eq!(built, assigned);
}
