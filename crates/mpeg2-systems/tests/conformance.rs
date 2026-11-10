//! This crate's reading of the fixtures, and what its multiplexing and
//! packetizing layers write, checked against TSDuck and ffprobe, which read
//! the same bytes independently
//!
//! The tests run `tstables`, `tsanalyze` and `ffprobe` from `PATH`, so they
//! are ignored unless asked for; once asked for, a tool missing is a failure.

#[cfg(test)]
#[cfg(feature = "alloc")]
#[path = "support/tools.rs"]
#[expect(
    dead_code,
    reason = "each test file takes a part of what the tests share"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "what a tool prints is read at the keys and indexes it always prints"
)]
mod tools;

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "streams are cut at offsets their own headers give, well inside their length"
)]
mod tests {
    use std::collections::BTreeMap;

    use mpeg2_systems::pes::StreamId;
    use mpeg2_systems::ps::mux::PackWriter;
    use mpeg2_systems::ps::psm::{ElementaryStream, ElementaryStreamLoop};
    use mpeg2_systems::ps::{PackHeader, ProgramStreamMap, demux as ps_demux};
    use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
    use mpeg2_systems::psi::pmt::{self, TsProgramMapSection};
    use mpeg2_systems::psi::section::{self, Section, SectionSyntax};
    use mpeg2_systems::psi::{TableId, VersionNumber};
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::ClockReference;
    use mpeg2_systems::ts::Pid;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event, PacketSize};
    use mpeg2_systems::ts::mux as ts_mux;
    use mpeg2_systems::ts::packet::TransportPacket;

    use crate::tools::{
        ProgramAssociation, ProgramMap, Timing, ffprobe, fixture_path, integer, pes_packet,
        pes_packets_of_188, run, run_for_output, tables_tstables, timing, ts_events,
        ts_events_with,
    };

    /// A Transport Stream fixture: its name, the offset of its first
    /// `sync_byte` and the bytes from one `sync_byte` to the next
    const TRANSPORT_STREAMS: [(&str, usize, usize); 3] = [
        ("188.ts", 0, PacketSize::Standard188.stride()),
        ("192.ts", 4, PacketSize::Timestamped192.stride()),
        ("204.ts", 0, PacketSize::ReedSolomon204.stride()),
    ];

    #[expect(
        clippy::unreachable,
        reason = "Program is non_exhaustive, and the crate reads no other kind of entry"
    )]
    fn tables_read(stream: &[u8]) -> (Vec<ProgramAssociation>, Vec<ProgramMap>) {
        let mut associations = Vec::new();
        let maps = ts_events(stream, &[], |event| {
            if let Event::ProgramAssociationSection(section) = event {
                associations.push(ProgramAssociation {
                    transport_stream_id: section.transport_stream_id.into(),
                    programs: section
                        .programs
                        .iter()
                        .map(|program| match program.unwrap() {
                            Program::Network { network_pid } => (0, network_pid.get().into()),
                            Program::ProgramMap {
                                program_number,
                                program_map_pid,
                            } => (program_number.get().into(), program_map_pid.get().into()),
                            other => unreachable!("a PAT entry of no known kind: {other:?}"),
                        })
                        .collect(),
                });
                return None;
            }
            let Event::TsProgramMapSection { section, .. } = event else {
                return None;
            };
            Some(ProgramMap {
                program_number: section.program_number.into(),
                pcr_pid: section.pcr_pid.map(|pid| pid.get().into()),
                elementary_streams: section
                    .elementary_streams
                    .iter()
                    .map(|stream| {
                        let stream = stream.unwrap();
                        (
                            stream.stream_type.get().into(),
                            stream.elementary_pid.get().into(),
                        )
                    })
                    .collect(),
            })
        });
        (associations, maps)
    }

    /// The `id` ffmpeg's Program Stream demuxer gives a stream: the PES start
    /// code, `packet_start_code_prefix` followed by `stream_id`
    fn program_stream_id(stream_id: u8) -> u64 {
        0x100 | u64::from(stream_id)
    }

    /// The length of the MPEG-1 Layer II audio frame whose header `frame`
    /// starts with, and its duration in 90 kHz units, as ISO/IEC 11172-3
    /// gives them
    fn layer_ii_frame(frame: &[u8]) -> (usize, u64) {
        const BIT_RATES_KBPS: [usize; 15] = [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ];
        const SAMPLING_FREQUENCIES: [usize; 3] = [44_100, 48_000, 32_000];
        const SAMPLES_PER_FRAME: usize = 1152;
        assert_eq!(
            (frame[0], frame[1] & 0xFE),
            (0xFF, 0xFC),
            "not an MPEG-1 Layer II frame"
        );
        let bit_rate = BIT_RATES_KBPS[usize::from(frame[2] >> 4)] * 1000;
        let sampling_frequency = SAMPLING_FREQUENCIES[usize::from((frame[2] >> 2) & 0b11)];
        let length = 144 * bit_rate / sampling_frequency + usize::from((frame[2] >> 1) & 1);
        let duration = SAMPLES_PER_FRAME * 90_000 / sampling_frequency;
        (length, duration.try_into().unwrap())
    }

    #[test]
    #[ignore = "runs tstables from TSDuck"]
    fn each_transport_stream_has_the_tables_tstables_reads() {
        for (name, _, _) in TRANSPORT_STREAMS {
            let path = fixture_path(name);
            let stream = std::fs::read(&path).unwrap();
            assert_eq!(tables_read(&stream), tables_tstables(&path), "{name}");
        }
    }

    #[test]
    #[ignore = "runs tsanalyze from TSDuck"]
    fn each_transport_stream_has_the_packets_per_pid_tsanalyze_counts() {
        for (name, first_sync_byte, stride) in TRANSPORT_STREAMS {
            let path = fixture_path(name);
            let stream = std::fs::read(&path).unwrap();
            let mut counted = BTreeMap::new();
            for unit in stream[first_sync_byte..].chunks(stride) {
                let packet = TransportPacket::read(&unit[..188]).unwrap();
                *counted.entry(u64::from(packet.pid.get())).or_default() += 1;
            }
            let analyzed: BTreeMap<_, _> = run("tsanalyze", &["--json", &path])["pids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pid| (integer(pid, "id"), integer(&pid["packets"], "total")))
                .collect();
            assert_eq!(counted, analyzed, "{name}");
        }
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn each_transport_stream_has_the_pes_packets_ffprobe_reads() {
        for (name, _, _) in TRANSPORT_STREAMS {
            let path = fixture_path(name);
            let stream = std::fs::read(&path).unwrap();
            let (_, maps) = tables_read(&stream);
            let pids: Vec<_> = maps
                .iter()
                .flat_map(|map| map.elementary_streams.iter().map(|&(_, pid)| pid))
                .collect();
            let mut read: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
            for (pid, timing) in ts_events(&stream, &pids, |event| {
                let Event::PesPacket { pid, packet, .. } = event else {
                    return None;
                };
                Some((u64::from(pid.get()), timing(&packet)))
            }) {
                read.entry(pid).or_default().push(timing);
            }
            let (streams, probed) = ffprobe(&path, None, false);
            let mut pmt_pids = pids;
            pmt_pids.sort_unstable();
            let probed_pids: Vec<_> = streams.into_keys().collect();
            assert_eq!((pmt_pids, read), (probed_pids, probed), "{name}");
        }
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn each_transport_stream_has_the_pes_packets_ffprobe_reads_with_no_pid_subscribed_to() {
        for (name, _, _) in TRANSPORT_STREAMS {
            let path = fixture_path(name);
            let stream = std::fs::read(&path).unwrap();
            let config = DemuxerConfig::default().with_subscribe_elementary_pids(true);
            let mut read: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
            for (pid, timing) in ts_events_with(config, &stream, &[], |event| {
                let Event::PesPacket { pid, packet, .. } = event else {
                    return None;
                };
                Some((u64::from(pid.get()), timing(&packet)))
            }) {
                read.entry(pid).or_default().push(timing);
            }
            let (_, probed) = ffprobe(&path, None, false);
            assert_eq!(read, probed, "{name}");
        }
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_program_stream_has_the_streams_and_pes_packets_ffprobe_reads() {
        let path = fixture_path("program.ps");
        let stream = std::fs::read(&path).unwrap();
        let mut bounded = Vec::new();
        let mut read: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        let mut demuxer = ps_demux::Demuxer::new();
        demuxer.handle_input(&stream);
        demuxer.finish();
        while let Some(event) = demuxer.poll_event() {
            if let ps_demux::Event::SystemHeader(header) = &event {
                bounded.extend(
                    header
                        .stream_bounds
                        .iter()
                        .map(|bound| program_stream_id(bound.unwrap().stream_id.get())),
                );
            }
            // Why not keep the padding stream: ffmpeg skips it, as it
            // carries no elementary stream.
            if let ps_demux::Event::PesPacket(packet) = &event
                && packet.stream_id != StreamId::PADDING_STREAM
            {
                read.entry(program_stream_id(packet.stream_id.get()))
                    .or_default()
                    .push(timing(packet));
            }
        }
        bounded.sort_unstable();
        let (streams, probed) = ffprobe(&path, None, false);
        let probed_ids: Vec<_> = streams.into_keys().collect();
        assert_eq!((bounded, read), (probed_ids, probed));
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn a_program_stream_the_pack_writer_writes_reads_in_ffprobe_as_written() {
        const VIDEO: u8 = 0xE0;
        const AUDIO: u8 = 0xC0;
        let (mut video, audio) = pes_packets_of_188();
        // Why zero bytes, to make the first picture too long for one PES
        // packet: any number of them may stand before a start code
        // (`next_start_code` in ISO/IEC 13818-2), so the picture stays
        // whole and ends where it did.
        video[0].1.resize(200_000, 0);
        let ((first_pts, _), frames) = &audio[0];
        let (first_length, duration) = layer_ii_frame(frames);
        let (second_length, _) = layer_ii_frame(&frames[first_length..]);
        let first_pts = first_pts.unwrap();
        let access_units = [
            (VIDEO, video[0].0, &video[0].1[..]),
            (AUDIO, (Some(first_pts), None), &frames[..first_length]),
            (VIDEO, video[1].0, &video[1].1[..]),
            (
                AUDIO,
                (Some(first_pts + duration), None),
                &frames[first_length..first_length + second_length],
            ),
            (VIDEO, video[2].0, &video[2].1[..]),
        ];

        let writer = PackWriter::new();
        let mut written = Vec::new();
        writer
            .write_pack_header(
                &PackHeader::new(ClockReference::new(0, 0).unwrap(), 2_000),
                &mut written,
            )
            .unwrap();
        let mut map = ProgramStreamMap::new(VersionNumber::try_from(0).unwrap());
        let mut streams = Vec::new();
        ElementaryStreamLoop::write_from(
            [
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
            ],
            &mut streams,
        )
        .unwrap();
        map.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
        writer.write_program_stream_map(&map, &mut written).unwrap();
        let mut expected: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        let mut pes_packets = Vec::new();
        for (stream_id, (pts, dts), payload) in access_units {
            let packet = pes_packet(stream_id, (pts, dts), payload);
            pes_packets.push(writer.write_pes_packets(&packet, &mut written).unwrap());
            expected
                .entry(program_stream_id(stream_id))
                .or_default()
                .push((pts, dts.or(pts)));
        }
        writer.write_program_end_code(&mut written);
        assert!(
            pes_packets[0] > 1,
            "the first picture must be split to check splitting"
        );
        let path = format!("{}/muxed.mpg", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&path, &written).unwrap();

        // Why name the format: a stream of one pack is below the confidence
        // at which ffprobe's probe takes it for a Program Stream, and the
        // probe heuristics are not what this checks.
        let (streams, probed) = ffprobe(&path, Some("mpeg"), true);
        let expected_streams = BTreeMap::from([
            (program_stream_id(AUDIO), "audio".to_owned()),
            (program_stream_id(VIDEO), "video".to_owned()),
        ]);
        assert_eq!((expected_streams, expected), (streams, probed));
    }

    #[test]
    #[ignore = "runs tstables from TSDuck"]
    fn the_sections_of_a_subscribed_pid_are_the_ones_tstables_reads() {
        let pid = Pid::try_from(0x0300).unwrap();
        let version = |value: u8| VersionNumber::try_from(value).unwrap();
        let long_body = [0xAA; 300];
        let long = Section::new(TableId::from(0x80), &long_body).with_syntax(SectionSyntax::new(
            0x1234,
            version(3),
            true,
            0,
            0,
        ));
        let next = Section::new(TableId::from(0x80), &long_body[..20])
            .with_syntax(SectionSyntax::new(0x1234, version(4), false, 0, 0));
        let short = Section::new(TableId::from(0x81), &[0xBB; 20]);
        let mut writer = ts_mux::PacketWriter::new();
        let mut written = Vec::new();
        for section in [&long, &short, &long, &next] {
            writer.write_section(pid, section, &mut written).unwrap();
        }
        let path = format!("{}/private-sections.ts", env!("CARGO_TARGET_TMPDIR"));
        let sections_path = format!("{}/private-sections.bin", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&path, &written).unwrap();
        run_for_output(
            "tstables",
            &[
                "--all-sections",
                "--include-next",
                "--pid",
                "0x0300",
                "--binary-output",
                &sections_path,
                &path,
            ],
        );
        let mut tstables = Vec::new();
        let mut rest = &std::fs::read(&sections_path).unwrap()[..];
        while let Some(length) = section::encoded_len(rest) {
            let (bytes, after) = rest.split_at(length);
            tstables.push(bytes.to_vec());
            rest = after;
        }

        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.subscribe_sections(pid);
        demuxer.handle_input(&written);
        demuxer.finish();
        let mut read = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            if let Event::Section { section, .. } = event {
                let mut bytes = Vec::new();
                section.write(&mut bytes).unwrap();
                read.push(bytes);
            }
        }
        assert_eq!((read, rest.len()), (tstables, 0));
    }

    /// Writes the PAT naming program 1 on PMT PID 0x1000, then its PMT
    /// naming `streams` and `pcr_pid`
    fn write_program_tables<'stream>(
        writer: &mut ts_mux::PacketWriter,
        streams: impl IntoIterator<Item = pmt::ElementaryStream<'stream>>,
        pcr_pid: Option<Pid>,
        out: &mut Vec<u8>,
    ) {
        let pmt_pid = Pid::try_from(0x1000).unwrap();
        let mut programs = Vec::new();
        ProgramLoop::write_from(
            [Program::ProgramMap {
                program_number: 1.try_into().unwrap(),
                program_map_pid: pmt_pid,
            }],
            &mut programs,
        )
        .unwrap();
        let mut pat = ProgramAssociationSection::new(1, VersionNumber::try_from(0).unwrap());
        pat.programs = ProgramLoop::from(&programs[..]);
        writer.write_program_association_section(&pat, out).unwrap();
        let mut entries = Vec::new();
        pmt::ElementaryStreamLoop::write_from(streams, &mut entries).unwrap();
        let mut pmt = TsProgramMapSection::new(1, VersionNumber::try_from(0).unwrap());
        pmt.pcr_pid = pcr_pid;
        pmt.elementary_streams = pmt::ElementaryStreamLoop::from(&entries[..]);
        writer
            .write_ts_program_map_section(pmt_pid, &pmt, out)
            .unwrap();
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg and tsanalyze from TSDuck"]
    fn a_transport_stream_the_packet_writer_writes_reads_in_ffprobe_and_tsanalyze_as_written() {
        const VIDEO_PID: u16 = 0x100;
        const AUDIO_PID: u16 = 0x101;
        let pid = |value: u16| Pid::try_from(value).unwrap();
        let (mut video, audio) = pes_packets_of_188();
        // Why zero bytes, to make the first picture longer than
        // PES_packet_length can state: any number of them may stand before a
        // start code (`next_start_code` in ISO/IEC 13818-2).
        video[0].1.resize(100_000, 0);

        let mut writer = ts_mux::PacketWriter::new();
        let mut written = Vec::new();
        write_program_tables(
            &mut writer,
            [
                pmt::ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
                pmt::ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, pid(AUDIO_PID)),
            ],
            Some(pid(VIDEO_PID)),
            &mut written,
        );
        let mut expected: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        for (index, (timing, payload)) in video.iter().enumerate() {
            let mut packet = pes_packet(0xE0, *timing, payload);
            packet.length_unspecified = true;
            let pcr = ClockReference::new(timing.1.unwrap() - 9_000, 0).unwrap();
            writer
                .write_pes_packet(pid(VIDEO_PID), &packet, Some(pcr), &mut written)
                .unwrap();
            expected.entry(VIDEO_PID.into()).or_default().push(*timing);
            if let Some((timing, payload)) = audio.get(index) {
                writer
                    .write_pes_packet(
                        pid(AUDIO_PID),
                        &pes_packet(0xC0, *timing, payload),
                        None,
                        &mut written,
                    )
                    .unwrap();
                expected.entry(AUDIO_PID.into()).or_default().push(*timing);
            }
            // Why a PCR on its own as well as on each PES packet: tsanalyze
            // then checks that a packet without a payload keeps the
            // continuity counter of the packet before it (§2.4.3.3).
            writer
                .write_program_clock_reference(
                    pid(VIDEO_PID),
                    ClockReference::new(pcr.base() + 4_500, 0).unwrap(),
                    &mut written,
                )
                .unwrap();
        }
        let path = format!("{}/muxed.ts", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&path, &written).unwrap();

        let (streams, probed) = ffprobe(&path, None, false);
        let expected_streams = BTreeMap::from([
            (VIDEO_PID.into(), "video".to_owned()),
            (AUDIO_PID.into(), "audio".to_owned()),
        ]);
        assert_eq!((expected_streams, expected), (streams, probed));

        let analyzed = run("tsanalyze", &["--json", &path]);
        let mut errors = BTreeMap::new();
        for key in ["invalid-syncs", "transport-errors", "suspect-ignored"] {
            errors.insert(
                format!("ts {key}"),
                integer(&analyzed["ts"]["packets"], key),
            );
        }
        errors.insert(
            "unreferenced PIDs".to_owned(),
            integer(&analyzed["ts"]["pids"], "unreferenced"),
        );
        let mut starts = BTreeMap::new();
        for analyzed_pid in analyzed["pids"].as_array().unwrap() {
            let id = integer(analyzed_pid, "id");
            let packets = &analyzed_pid["packets"];
            for key in [
                "discontinuities",
                "duplicated",
                "invalid-scrambling",
                "pcr-leap",
            ] {
                errors.insert(format!("PID {id:#06x} {key}"), integer(packets, key));
            }
            if [VIDEO_PID, AUDIO_PID].map(u64::from).contains(&id) {
                starts.insert(id, (integer(analyzed_pid, "pes"), integer(packets, "pcr")));
            }
        }
        let no_errors: BTreeMap<_, _> = errors.keys().map(|key| (key.clone(), 0)).collect();
        let written_starts = BTreeMap::from([
            (
                VIDEO_PID.into(),
                (
                    u64::try_from(video.len()).unwrap(),
                    u64::try_from(2 * video.len()).unwrap(),
                ),
            ),
            (AUDIO_PID.into(), (u64::try_from(audio.len()).unwrap(), 0)),
        ]);
        assert_eq!((errors, starts), (no_errors, written_starts));
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg and tsanalyze from TSDuck"]
    fn a_program_stream_map_the_packet_writer_writes_reads_in_ffprobe_and_tsanalyze_as_given() {
        const PSM_PID: u16 = 0x100;
        const COUNT: usize = 5;
        let pid = |value: u16| Pid::try_from(value).unwrap();
        let mut writer = ts_mux::PacketWriter::new();
        let mut written = Vec::new();
        let mut entries = Vec::new();
        ElementaryStreamLoop::write_from(
            [ElementaryStream::new(
                StreamType::ISO_IEC_13818_2_VIDEO,
                StreamId::from(0xE0),
            )],
            &mut entries,
        )
        .unwrap();
        let mut map = ProgramStreamMap::new(VersionNumber::try_from(0).unwrap());
        map.elementary_streams = ElementaryStreamLoop::from(&entries[..]);
        let mut map_bytes = Vec::new();
        map.write(&mut map_bytes).unwrap();
        let body = &map_bytes[6..];
        let packet = mpeg2_systems::pes::PesPacket::new(StreamId::PROGRAM_STREAM_MAP, body);
        for _ in 0..COUNT {
            write_program_tables(
                &mut writer,
                [pmt::ElementaryStream::new(
                    StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                    pid(PSM_PID),
                )],
                None,
                &mut written,
            );
            writer
                .write_pes_packet(pid(PSM_PID), &packet, None, &mut written)
                .unwrap();
        }
        let path = format!("{}/program_stream_map.ts", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&path, &written).unwrap();

        let probed = run(
            "ffprobe",
            &[
                "-v",
                "error",
                "-of",
                "json",
                "-show_packets",
                "-show_data",
                &path,
            ],
        );
        let payloads: Vec<_> = probed["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|packet| {
                let stream_id = integer(&packet["side_data_list"][0], "id");
                let data = packet["data"].as_str().unwrap();
                let hex: String = data
                    .lines()
                    .filter_map(|line| line.split_once(": "))
                    .flat_map(|(_, rest)| rest.split("  ").next().unwrap().split(' '))
                    .collect();
                let bytes: Vec<u8> = hex
                    .as_bytes()
                    .chunks(2)
                    .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                    .collect();
                (stream_id, bytes)
            })
            .collect();
        assert_eq!(payloads, vec![(0xBC, body.to_vec()); COUNT]);

        let analyzed = run("tsanalyze", &["--json", &path]);
        let analyzed_pid = analyzed["pids"]
            .as_array()
            .unwrap()
            .iter()
            .find(|analyzed_pid| integer(analyzed_pid, "id") == u64::from(PSM_PID))
            .unwrap();
        let packets = &analyzed_pid["packets"];
        assert_eq!(
            [
                integer(&analyzed["ts"]["packets"], "invalid-syncs"),
                integer(&analyzed["ts"]["packets"], "transport-errors"),
                integer(packets, "discontinuities"),
                integer(analyzed_pid, "invalid-pes-prefix"),
                integer(analyzed_pid, "pes"),
                integer(analyzed_pid, "pes-stream-id"),
            ],
            [0, 0, 0, 0, u64::try_from(COUNT).unwrap(), 0xBC]
        );
    }
}
