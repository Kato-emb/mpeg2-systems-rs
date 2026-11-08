//! This crate's reading of the fixtures, and what its Program Stream muxer
//! writes, checked against TSDuck and ffprobe, which read the same bytes
//! independently
//!
//! The tests run `tstables`, `tsanalyze` and `ffprobe` from `PATH`, so they
//! are ignored unless asked for; once asked for, a tool missing is a failure.

#[cfg(test)]
#[cfg(feature = "alloc")]
#[path = "support/json.rs"]
mod json;

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "streams are cut at offsets their own headers give, well inside their length"
)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::Command;

    use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    use mpeg2_systems::ps::mux::Muxer;
    use mpeg2_systems::ps::psm::{ElementaryStream, ElementaryStreamLoop};
    use mpeg2_systems::ps::{PackHeader, ProgramStreamMap, demux as ps_demux};
    use mpeg2_systems::psi::VersionNumber;
    use mpeg2_systems::psi::pat::Program;
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::{ClockReference, Timestamp};
    use mpeg2_systems::ts::Pid;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event, PacketSize};
    use mpeg2_systems::ts::packet::TransportPacket;

    use super::json::Json;

    /// A Transport Stream fixture: its name, the offset of its first
    /// `sync_byte` and the bytes from one `sync_byte` to the next
    const TRANSPORT_STREAMS: [(&str, usize, usize); 3] = [
        ("188.ts", 0, PacketSize::Standard188.stride()),
        ("192.ts", 4, PacketSize::Timestamped192.stride()),
        ("204.ts", 0, PacketSize::ReedSolomon204.stride()),
    ];

    /// The PTS and the DTS of a PES packet, a DTS absent being the PTS, as
    /// ffprobe reports it
    type Timing = (Option<u64>, Option<u64>);

    /// What ffprobe reads: the `id` and `codec_type` of each stream, and the
    /// timing of the packets of each stream by `id`
    type Probed = (BTreeMap<u64, String>, BTreeMap<u64, Vec<Timing>>);

    /// The PAT: `transport_stream_id`, and the `program_number` and PID of
    /// each entry, `program_number` 0 for the network PID
    #[derive(Debug, PartialEq, Eq)]
    struct ProgramAssociation {
        transport_stream_id: u64,
        programs: Vec<(u64, u64)>,
    }

    /// A PMT: `program_number`, `PCR_PID`, and the `stream_type` and
    /// `elementary_PID` of each stream
    #[derive(Debug, PartialEq, Eq)]
    struct ProgramMap {
        program_number: u64,
        pcr_pid: Option<u64>,
        elementary_streams: Vec<(u64, u64)>,
    }

    fn fixture_path(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    /// What `tool` prints on its standard output for `arguments`, read as JSON
    fn run(tool: &str, arguments: &[&str]) -> Json {
        let output = Command::new(tool)
            .args(arguments)
            .output()
            .map_err(|error| format!("{tool} did not run ({error}); put it on PATH"))
            .unwrap();
        assert!(
            output.status.success(),
            "{tool} {arguments:?}: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        Json::parse(&String::from_utf8(output.stdout).unwrap())
    }

    fn integer(json: &Json, key: &str) -> u64 {
        json.get(key)
            .and_then(Json::as_u64)
            .ok_or_else(|| format!("no integer {key} in {json:?}"))
            .unwrap()
    }

    /// Hands `stream` whole to a demuxer subscribed to `pids` and returns
    /// what `take` keeps of each event
    fn ts_events<T>(
        stream: &[u8],
        pids: &[u64],
        mut take: impl FnMut(Event<'_>) -> Option<T>,
    ) -> Vec<T> {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        for &pid in pids {
            demuxer.subscribe_pes(Pid::try_from(u16::try_from(pid).unwrap()).unwrap());
        }
        demuxer.handle_input(stream);
        demuxer.finish();
        let mut kept = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            kept.extend(take(event));
        }
        kept
    }

    fn timing(packet: &PesPacket<'_>) -> Timing {
        let header = packet.optional_header.as_ref();
        let pts = header.and_then(|header| header.pts).map(Timestamp::get);
        let dts = header.and_then(|header| header.dts).map(Timestamp::get);
        (pts, dts.or(pts))
    }

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

    fn tables_tstables(path: &str) -> (Vec<ProgramAssociation>, Vec<ProgramMap>) {
        let tables = run("tstables", &["--json-output", "-", path]);
        let mut associations: Vec<_> = tables
            .named("PAT")
            .into_iter()
            .map(|pat| {
                let mut programs: Vec<_> = pat
                    .get("network_pid")
                    .and_then(Json::as_u64)
                    .map(|pid| (0, pid))
                    .into_iter()
                    .collect();
                programs.extend(pat.named("service").into_iter().map(|service| {
                    (
                        integer(service, "service_id"),
                        integer(service, "program_map_pid"),
                    )
                }));
                ProgramAssociation {
                    transport_stream_id: integer(pat, "transport_stream_id"),
                    programs,
                }
            })
            .collect();
        let mut maps: Vec<_> = tables
            .named("PMT")
            .into_iter()
            .map(|pmt| ProgramMap {
                program_number: integer(pmt, "service_id"),
                // Why not take PCR_PID as it is: TSDuck prints the null PID for
                // a program without a PCR, which the crate reads as none.
                pcr_pid: Some(integer(pmt, "pcr_pid"))
                    .filter(|&pid| pid != u64::from(Pid::NULL.get())),
                elementary_streams: pmt
                    .named("component")
                    .into_iter()
                    .map(|component| {
                        (
                            integer(component, "stream_type"),
                            integer(component, "elementary_pid"),
                        )
                    })
                    .collect(),
            })
            .collect();
        // Why not compare every table tstables prints: it may print a table
        // each time the stream repeats it, where the demuxer reports a table
        // only when it changes.
        associations.dedup();
        maps.dedup();
        (associations, maps)
    }

    /// What ffprobe reads from the file at `path`, with or without the
    /// packets that do not start a PES packet
    ///
    /// `format` names the demuxer, when not left to ffprobe's probe.
    fn ffprobe(path: &str, format: Option<&str>, all_packets: bool) -> Probed {
        // Why not let ffprobe fill in the timestamps (its default): it
        // computes one for every packet its parser cuts out of a PES packet,
        // which would hide which packets carried a PTS of their own.
        let mut arguments = vec!["-v", "error", "-fflags", "+nofillin"];
        if let Some(format) = format {
            arguments.extend(["-f", format]);
        }
        arguments.extend(["-of", "json", "-show_streams", "-show_packets", path]);
        let probed = run("ffprobe", &arguments);
        let streams: Vec<_> = probed
            .get("streams")
            .unwrap()
            .elements()
            .iter()
            .map(|stream| {
                let codec_type = stream.get("codec_type").and_then(Json::as_str).unwrap();
                (integer(stream, "id"), codec_type.to_owned())
            })
            .collect();
        let mut packets: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        for packet in probed.get("packets").unwrap().elements() {
            // Why not count every packet as a PES packet: ffprobe's parser
            // cuts a PES packet into the frames it holds, and only the first
            // frame to start in a PES packet takes its position and its
            // timestamps; the others have no `pos`.
            if !all_packets && packet.get("pos").is_none() {
                continue;
            }
            let index = usize::try_from(integer(packet, "stream_index")).unwrap();
            let timing = (
                packet.get("pts").and_then(Json::as_u64),
                packet.get("dts").and_then(Json::as_u64),
            );
            packets.entry(streams[index].0).or_default().push(timing);
        }
        (streams.into_iter().collect(), packets)
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
            let analyzed: BTreeMap<_, _> = run("tsanalyze", &["--json", &path])
                .get("pids")
                .unwrap()
                .elements()
                .iter()
                .map(|pid| {
                    let packets = pid.get("packets").unwrap();
                    (integer(pid, "id"), integer(packets, "total"))
                })
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
    fn a_program_stream_the_muxer_writes_reads_in_ffprobe_as_written() {
        // Elementary streams ffprobe can parse: the MPEG-2 video pictures and
        // the MPEG-1 Layer II audio frames FFmpeg wrote into 188.ts.
        const VIDEO: u8 = 0xE0;
        const AUDIO: u8 = 0xC0;
        let source = std::fs::read(fixture_path("188.ts")).unwrap();
        let mut video = Vec::new();
        let mut audio = Vec::new();
        for (pid, packet) in ts_events(&source, &[0x100, 0x101], |event| {
            let Event::PesPacket { pid, packet, .. } = event else {
                return None;
            };
            Some((pid.get(), (timing(&packet), packet.payload.to_vec())))
        }) {
            if pid == 0x100 { &mut video } else { &mut audio }.push(packet);
        }
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

        let muxer = Muxer::new();
        let mut written = Vec::new();
        muxer
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
        muxer.write_program_stream_map(&map, &mut written).unwrap();
        let mut expected: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        let mut pes_packets = Vec::new();
        for (stream_id, (pts, dts), payload) in access_units {
            let mut header = OptionalHeader::new();
            header.pts = pts.map(|pts| Timestamp::try_from(pts).unwrap());
            header.dts = dts.map(|dts| Timestamp::try_from(dts).unwrap());
            let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
            packet.optional_header = Some(header);
            pes_packets.push(muxer.write_pes_packets(&packet, &mut written).unwrap());
            expected
                .entry(program_stream_id(stream_id))
                .or_default()
                .push((pts, dts.or(pts)));
        }
        muxer.write_program_end_code(&mut written);
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
}
