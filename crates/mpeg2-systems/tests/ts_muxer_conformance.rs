//! What the Transport Stream `Muxer` writes, checked against TSDuck and
//! ffprobe: the streams and PES packets, a stream of `private_stream_2`
//! among them, the PAT and the PMT, and how far apart the PCRs and the
//! tables are
//!
//! The tests run `tstables`, `tsanalyze`, `tsp` and `ffprobe` from `PATH`, so
//! they are ignored unless asked for; once asked for, a tool missing is a
//! failure.

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
    reason = "the times and indexes the tools print are well inside their range"
)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::Command;

    use mpeg2_systems::pes::{PesPacket, StreamId};
    use mpeg2_systems::psi::pmt::ElementaryStream;
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::{ClockReference, Timestamp};
    use mpeg2_systems::ts::Pid;
    use mpeg2_systems::ts::mux::{Muxer, MuxerConfig, TransportPacketFields};

    use crate::tools::{
        ProgramAssociation, ProgramMap, Timing, ffprobe, integer, pes_packet, pes_packets_of_188,
        run, tables_tstables,
    };

    const PMT_PID: u16 = 0x1000;
    const VIDEO_PID: u16 = 0x0100;
    const AUDIO_PID: u16 = 0x0101;
    const PRIVATE_PID: u16 = 0x0102;

    /// The PID and `stream_type` of the video and the audio of the fixture
    const STREAMS: [(u16, StreamType); 2] = [
        (VIDEO_PID, StreamType::ISO_IEC_13818_2_VIDEO),
        (AUDIO_PID, StreamType::ISO_IEC_11172_3_AUDIO),
    ];

    /// Times the PES packets of the fixture are given, one after the other
    const REPEATS: u64 = 30;

    /// 90 kHz ticks from one input to the next, 20 ms: half the PCR interval,
    /// so that the time stamps leap nowhere
    const STEP: u64 = 1_800;

    /// A PES packet to give: its PID, `stream_id`, timing and payload
    type Input = (u16, u8, Timing, Vec<u8>);

    /// The PES packets of the fixture in the order of their DTS, repeated
    /// [`REPEATS`] times, each given [`STEP`] after the one before it from a
    /// second in, a picture with its PTS two steps after its DTS
    fn inputs() -> Vec<Input> {
        inputs_at(|index| 90_000 + index * STEP)
    }

    /// The PES packets of [`inputs`], the DTS of the one at each index
    /// `dts_of` it, round the 33-bit circle
    fn inputs_at(dts_of: impl Fn(u64) -> u64) -> Vec<Input> {
        let (video, audio) = pes_packets_of_188();
        let mut fixture: Vec<(u16, u8, Timing, Vec<u8>)> = video
            .into_iter()
            .map(|(timing, payload)| (VIDEO_PID, 0xe0, timing, payload))
            .chain(
                audio
                    .into_iter()
                    .map(|(timing, payload)| (AUDIO_PID, 0xc0, timing, payload)),
            )
            .collect();
        fixture.sort_by_key(|(_, _, (_, dts), _)| *dts);
        (0..REPEATS)
            .flat_map(|_| fixture.clone())
            .zip((0..).map(|index| dts_of(index) % Timestamp::MODULUS))
            .map(|((pid, stream_id, _, payload), dts)| {
                let timing = if pid == VIDEO_PID {
                    (Some((dts + 2 * STEP) % Timestamp::MODULUS), Some(dts))
                } else {
                    (Some(dts), Some(dts))
                };
                (pid, stream_id, timing, payload)
            })
            .collect()
    }

    /// Writes what the muxer of the default settings makes of [`inputs`] to
    /// a file of `name`, and returns its path
    fn multiplexed(name: &str) -> String {
        multiplexed_of(name, &STREAMS, inputs())
    }

    /// Writes what the muxer of the default settings, `streams` registered,
    /// makes of `inputs` to a file of `name`, and returns its path
    ///
    /// An input of `private_stream_2` is given without an optional header.
    fn multiplexed_of(name: &str, streams: &[(u16, StreamType)], inputs: Vec<Input>) -> String {
        let pid = |value: u16| Pid::try_from(value).unwrap();
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        for &(stream_pid, stream_type) in streams {
            muxer
                .add_stream(1, ElementaryStream::new(stream_type, pid(stream_pid)))
                .unwrap();
        }
        let mut stream = Vec::new();
        for (pid_value, stream_id, timing, payload) in inputs {
            let packet = if StreamId::from(stream_id) == StreamId::PRIVATE_STREAM_2 {
                PesPacket::new(StreamId::PRIVATE_STREAM_2, &payload)
            } else {
                pes_packet(stream_id, timing, &payload)
            };
            muxer.handle_pes_packet(pid(pid_value), &packet).unwrap();
            muxer.poll_output(&mut stream);
        }
        muxer.finish();
        muxer.poll_output(&mut stream);
        let path = format!("{}/{name}", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&path, &stream).unwrap();
        path
    }

    /// The lines `tsp` prints of the plugin `arguments` over the file at
    /// `path`, on its standard output and its log
    fn tsp(path: &str, arguments: &[&str]) -> Vec<String> {
        let output = Command::new("tsp")
            .args(["-I", "file", path])
            .args(arguments)
            .args(["-O", "drop"])
            .output()
            .map_err(|error| format!("tsp did not run ({error}); put it on PATH"))
            .unwrap();
        assert!(
            output.status.success(),
            "tsp {arguments:?}: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        [output.stdout, output.stderr]
            .iter()
            .flat_map(|bytes| {
                String::from_utf8_lossy(bytes)
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The index in the stream and the value of each PCR, as `pcrextract`
    /// reads them
    fn pcrs(path: &str) -> Vec<(u64, u64)> {
        tsp(path, &["-P", "pcrextract", "--pcr", "--csv"])
            .iter()
            .skip(1)
            .map(|line| {
                let fields: Vec<&str> = line.split(',').collect();
                (fields[1].parse().unwrap(), fields[5].parse().unwrap())
            })
            .collect()
    }

    /// The index in the stream of the first packet of each section on `pid`,
    /// as `tables` reads them
    fn section_indexes(path: &str, pid: u16) -> Vec<u64> {
        tsp(
            path,
            &[
                "-P",
                "tables",
                "--pid",
                &pid.to_string(),
                "--all-sections",
                "--packet-index",
                "--log",
            ],
        )
        .iter()
        .filter_map(|line| {
            line.split("Packet ")
                .nth(1)?
                .split(' ')
                .next()?
                .parse()
                .ok()
        })
        .collect()
    }

    /// The time of the 27 MHz clock at which the packet at `index` arrives,
    /// from the PCRs on either side of it (ISO/IEC 13818-1 §2.4.2.3,
    /// equation 2-4), `None` outside the first and the last PCR
    fn arrival(pcrs: &[(u64, u64)], index: u64) -> Option<u64> {
        let after = pcrs.iter().position(|&(at, _)| at > index)?;
        let (before_index, before_value) = *pcrs.get(after.checked_sub(1)?)?;
        let (after_index, after_value) = pcrs[after];
        Some(
            before_value
                + (index - before_index) * (after_value - before_value)
                    / (after_index - before_index),
        )
    }

    /// The count of each error `tsanalyze` finds in the file at `path`, by
    /// what it counts
    fn tsanalyze_errors(path: &str) -> BTreeMap<String, u64> {
        let analyzed = run("tsanalyze", &["--json", path]);
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
        for analyzed_pid in analyzed["pids"].as_array().unwrap() {
            let id = integer(analyzed_pid, "id");
            for key in [
                "discontinuities",
                "duplicated",
                "invalid-scrambling",
                "pcr-leap",
            ] {
                errors.insert(
                    format!("PID {id:#06x} {key}"),
                    integer(&analyzed_pid["packets"], key),
                );
            }
        }
        errors
    }

    /// 27 MHz ticks of `milliseconds`
    const fn ticks(milliseconds: u64) -> u64 {
        milliseconds * 27_000
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_streams_and_pes_packets_given_read_in_ffprobe_as_given() {
        let path = multiplexed("muxer_ffprobe.ts");
        let mut expected: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        for (pid, _, timing, _) in inputs() {
            expected.entry(pid.into()).or_default().push(timing);
        }
        let expected_streams = BTreeMap::from([
            (VIDEO_PID.into(), "video".to_owned()),
            (AUDIO_PID.into(), "audio".to_owned()),
        ]);
        assert_eq!((expected_streams, expected), ffprobe(&path, None, false));
    }

    #[test]
    #[ignore = "runs tstables and tsanalyze from TSDuck"]
    fn the_pat_and_the_pmt_read_in_tstables_as_registered_and_tsanalyze_finds_no_error() {
        let path = multiplexed("muxer_tables.ts");
        let registered = (
            vec![ProgramAssociation {
                transport_stream_id: 1,
                programs: vec![(1, PMT_PID.into())],
            }],
            vec![ProgramMap {
                program_number: 1,
                pcr_pid: Some(VIDEO_PID.into()),
                elementary_streams: vec![(0x02, VIDEO_PID.into()), (0x03, AUDIO_PID.into())],
            }],
        );

        let errors = tsanalyze_errors(&path);
        let no_errors: BTreeMap<_, _> = errors.keys().map(|key| (key.clone(), 0)).collect();
        assert_eq!((tables_tstables(&path), errors), (registered, no_errors));
    }

    #[test]
    #[ignore = "runs tsp from TSDuck"]
    fn the_pcrs_are_no_further_apart_than_the_pcr_interval() {
        let path = multiplexed("muxer_pcr.ts");
        let pcrs = pcrs(&path);
        let longest = pcrs
            .windows(2)
            .map(|pair| pair[1].1 - pair[0].1)
            .max()
            .unwrap();
        println!("PCRs: {}, longest interval: {longest} ticks", pcrs.len());
        assert!(longest <= ticks(40), "{longest} ticks between PCRs");
    }

    #[test]
    #[ignore = "runs tsp from TSDuck"]
    fn the_pat_and_the_pmt_are_no_further_apart_than_the_psi_interval_and_one_input() {
        let path = multiplexed("muxer_psi.ts");
        let pcrs = pcrs(&path);
        let longest_step = inputs()
            .windows(2)
            .map(|pair| (pair[1].2).1.unwrap() - (pair[0].2).1.unwrap())
            .max()
            .unwrap()
            * 300;
        for pid in [0, PMT_PID] {
            let arrivals: Vec<u64> = section_indexes(&path, pid)
                .into_iter()
                .filter_map(|index| arrival(&pcrs, index))
                .collect();
            let longest = arrivals
                .windows(2)
                .map(|pair| pair[1] - pair[0])
                .max()
                .unwrap();
            println!(
                "PID {pid:#06x}: {} sections timed, longest interval {longest} ticks, bound {}",
                arrivals.len(),
                ticks(100) + longest_step
            );
            assert!(
                longest <= ticks(100) + longest_step,
                "PID {pid:#06x}: {longest} ticks"
            );
        }
    }

    /// The longest time from one PCR to the next in the file at `path`,
    /// round the circle of [`ClockReference::MODULUS`]
    fn longest_pcr_interval(path: &str) -> u64 {
        pcrs(path)
            .windows(2)
            .map(|pair| (pair[1].1 + ClockReference::MODULUS - pair[0].1) % ClockReference::MODULUS)
            .max()
            .unwrap()
    }

    #[test]
    #[ignore = "runs tsp and tsanalyze from TSDuck"]
    fn the_pcrs_stay_within_the_pcr_interval_across_a_leap_of_the_time_stamps() {
        let path = multiplexed_of(
            "muxer_leap.ts",
            &STREAMS,
            inputs_at(|index| 90_000 + index * STEP + if index >= 75 { 90_000 } else { 0 }),
        );
        let longest = longest_pcr_interval(&path);
        println!("leap: longest PCR interval {longest} ticks");
        let errors = tsanalyze_errors(&path);
        let no_errors: BTreeMap<_, _> = errors.keys().map(|key| (key.clone(), 0)).collect();
        assert_eq!(
            (longest <= ticks(40), errors),
            (true, no_errors),
            "{longest} ticks"
        );
    }

    #[test]
    #[ignore = "runs tsp and tsanalyze from TSDuck"]
    fn the_pcrs_stay_within_the_pcr_interval_across_the_wrap_of_33_bits() {
        let path = multiplexed_of(
            "muxer_wrap.ts",
            &STREAMS,
            inputs_at(|index| Timestamp::MODULUS - 30 * STEP + index * STEP),
        );
        let longest = longest_pcr_interval(&path);
        println!("wrap: longest PCR interval {longest} ticks");
        let mut expected: BTreeMap<_, _> = tsanalyze_errors(&path)
            .keys()
            .map(|key| (key.clone(), 0))
            .collect();
        // Why not 0 leaps of the PCR: tsanalyze counts the wrap of the
        // counter as a leap, as it does that of the PTS and the DTS of the
        // same stream, which this crate writes as given.
        expected.insert(format!("PID {VIDEO_PID:#06x} pcr-leap"), 1);
        assert_eq!(
            (longest <= ticks(40), tsanalyze_errors(&path)),
            (true, expected),
            "{longest} ticks"
        );
    }

    #[test]
    #[ignore = "runs tstables, tsanalyze and ffprobe"]
    fn a_stream_of_private_stream_2_without_time_stamps_reads_in_tstables_and_ffprobe_as_given() {
        let mut inputs = Vec::new();
        for input in self::inputs() {
            let after_video = input.0 == VIDEO_PID;
            inputs.push(input);
            if after_video {
                inputs.push((PRIVATE_PID, 0xbf, (None, None), vec![0xcc; 300]));
            }
        }
        let streams = [
            STREAMS[0],
            STREAMS[1],
            (PRIVATE_PID, StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA),
        ];
        let path = multiplexed_of("muxer_private_stream_2.ts", &streams, inputs.clone());
        let mut expected: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        for (pid, _, timing, _) in inputs {
            expected.entry(pid.into()).or_default().push(timing);
        }
        let registered = vec![ProgramMap {
            program_number: 1,
            pcr_pid: Some(VIDEO_PID.into()),
            elementary_streams: vec![
                (0x02, VIDEO_PID.into()),
                (0x03, AUDIO_PID.into()),
                (0x06, PRIVATE_PID.into()),
            ],
        }];
        let errors = tsanalyze_errors(&path);
        let no_errors: BTreeMap<_, _> = errors.keys().map(|key| (key.clone(), 0)).collect();
        let (_, probed) = ffprobe(&path, None, false);
        assert_eq!(
            (tables_tstables(&path).1, errors, probed),
            (registered, no_errors, expected)
        );
    }

    /// What `tsp -P dump` prints of a Transport Packet: its PID,
    /// `payload_unit_start_indicator`, `transport_priority`, whether it has
    /// a payload, `random_access_indicator`,
    /// `elementary_stream_priority_indicator`, whether it carries a PCR, and
    /// its transport private data
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct Dumped {
        pid: u64,
        payload_unit_start_indicator: bool,
        transport_priority: bool,
        payload: bool,
        random_access_indicator: bool,
        elementary_stream_priority_indicator: bool,
        program_clock_reference: bool,
        transport_private_data: Option<Vec<u8>>,
    }

    /// The value after `key: ` in a line `tsp -P dump` prints, up to the next
    /// comma or space
    fn after<'line>(line: &'line str, key: &str) -> &'line str {
        line.split(&format!("{key}: "))
            .nth(1)
            .and_then(|rest| rest.split([',', ' ']).next())
            .unwrap()
    }

    /// Each Transport Packet of the file at `path`, as `tsp -P dump` reads it
    fn dumped(path: &str) -> Vec<Dumped> {
        let mut packets: Vec<Dumped> = Vec::new();
        let mut private_data_left = 0;
        for line in tsp(
            path,
            &["-P", "dump", "--headers-only", "--adaptation-field"],
        ) {
            let line = line.trim();
            if line.starts_with("* Packet") {
                packets.push(Dumped::default());
            }
            let Some(packet) = packets.last_mut() else {
                continue;
            };
            if line.starts_with("PID: ") {
                packet.pid =
                    u64::from_str_radix(after(line, "PID").trim_start_matches("0x"), 16).unwrap();
            } else if line.starts_with("Error: ") {
                packet.payload_unit_start_indicator = after(line, "unit start") == "1";
                packet.transport_priority = after(line, "priority") == "1";
            } else if line.starts_with("Adaptation field: ") {
                packet.payload = after(line, "payload") == "yes";
            } else if line.starts_with("Discontinuity: ") {
                packet.random_access_indicator = after(line, "random access") == "1";
                packet.elementary_stream_priority_indicator = after(line, "ES priority") == "1";
            } else if line.starts_with("PCR: ") {
                packet.program_clock_reference = true;
            } else if let Some(rest) = line.strip_prefix("Private data (") {
                private_data_left = rest.split(' ').next().unwrap().parse().unwrap();
                packet.transport_private_data = Some(Vec::new());
            } else if let (Some((offset, bytes)), Some(data)) =
                (line.split_once(':'), packet.transport_private_data.as_mut())
                && private_data_left > 0
                && offset.len() == 4
            {
                // Why take no more than a line holds, and no more than are
                // left: the hexadecimal bytes of a line, 16 at most, are
                // followed by the same bytes in ASCII
                for byte in bytes.split_whitespace().take(private_data_left.min(16)) {
                    data.push(u8::from_str_radix(byte, 16).unwrap());
                    private_data_left -= 1;
                }
            }
        }
        packets
    }

    #[test]
    #[ignore = "runs tsp from TSDuck"]
    fn the_transport_packet_fields_set_on_each_pes_packet_read_in_tsp_dump() {
        let pid = |value: u16| Pid::try_from(value).unwrap();
        let inputs = inputs();
        let private_data: Vec<Vec<u8>> = (0..inputs.len())
            .map(|index| match index % 6 {
                1 => vec![u8::try_from(index % 256).unwrap(); 4],
                4 => (0..180)
                    .map(|byte| u8::try_from((byte + index) % 256).unwrap())
                    .collect(),
                5 => vec![u8::try_from(index % 256).unwrap(); 174],
                _ => Vec::new(),
            })
            .collect();
        let fields_of = |index: usize| {
            let data = private_data[index].as_slice();
            match index % 6 {
                0 => TransportPacketFields::new().with_random_access_indicator(true),
                1 => TransportPacketFields::new()
                    .with_elementary_stream_priority_indicator(true)
                    .with_transport_private_data(data),
                2 => TransportPacketFields::new().with_transport_priority(true),
                3 => TransportPacketFields::new(),
                4 => TransportPacketFields::new()
                    .with_transport_priority(true)
                    .with_transport_private_data(data),
                _ => TransportPacketFields::new()
                    .with_random_access_indicator(true)
                    .with_transport_private_data(data),
            }
        };
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        muxer.add_program(1, pid(PMT_PID)).unwrap();
        for (stream_pid, stream_type) in STREAMS {
            muxer
                .add_stream(1, ElementaryStream::new(stream_type, pid(stream_pid)))
                .unwrap();
        }
        let mut stream = Vec::new();
        for (index, (pid_value, stream_id, timing, payload)) in inputs.iter().enumerate() {
            muxer
                .handle_pes_packet_with_transport_packet_fields(
                    pid(*pid_value),
                    &pes_packet(*stream_id, *timing, payload),
                    &fields_of(index),
                )
                .unwrap();
        }
        muxer.finish();
        muxer.poll_output(&mut stream);
        let path = format!(
            "{}/muxer_transport_packet_fields.ts",
            env!("CARGO_TARGET_TMPDIR")
        );
        std::fs::write(&path, &stream).unwrap();

        let mut expected = Vec::new();
        let mut read = Vec::new();
        let mut starts = 0..;
        let mut fields = TransportPacketFields::new();
        for packet in dumped(&path) {
            if ![VIDEO_PID, AUDIO_PID].map(u64::from).contains(&packet.pid) {
                continue;
            }
            if packet.payload_unit_start_indicator {
                fields = fields_of(starts.next().unwrap());
            }
            let first = packet.payload_unit_start_indicator;
            expected.push(Dumped {
                pid: packet.pid,
                payload_unit_start_indicator: first,
                // Why not set the priority on a packet without a payload:
                // that is a PCR the muxer sends alone, not a packet of the
                // PES packet
                transport_priority: fields.transport_priority && packet.payload,
                payload: packet.payload,
                random_access_indicator: first && fields.random_access_indicator,
                elementary_stream_priority_indicator: first
                    && fields.elementary_stream_priority_indicator,
                // Why not check every PCR: their times are checked against
                // the PCR interval elsewhere; here only the one
                // random_access_indicator needs on the PCR PID (§2.4.3.5)
                program_clock_reference: packet.program_clock_reference,
                transport_private_data: fields
                    .transport_private_data
                    .filter(|_| first)
                    .map(<[u8]>::to_vec),
            });
            read.push(packet);
        }
        let random_access_on_the_pcr_pid_without_a_pcr = read
            .iter()
            .filter(|packet| {
                packet.pid == u64::from(VIDEO_PID)
                    && packet.random_access_indicator
                    && !packet.program_clock_reference
            })
            .count();
        let pcrs_alone_on_the_pcr_pid = read
            .iter()
            .filter(|packet| packet.pid == u64::from(VIDEO_PID) && !packet.payload)
            .count();
        assert_eq!(
            (
                read,
                starts.next(),
                random_access_on_the_pcr_pid_without_a_pcr,
                pcrs_alone_on_the_pcr_pid > 0
            ),
            (expected, Some(inputs.len()), 0, true)
        );
    }
}
