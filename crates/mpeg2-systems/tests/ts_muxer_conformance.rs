//! What the Transport Stream `Muxer` writes, checked against TSDuck and
//! ffprobe: the streams and PES packets, the PAT and the PMT, and how far
//! apart the PCRs and the tables are
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

    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::{ClockReference, Timestamp};
    use mpeg2_systems::ts::Pid;
    use mpeg2_systems::ts::mux::{Muxer, MuxerConfig};

    use crate::tools::{
        ProgramAssociation, ProgramMap, Timing, ffprobe, integer, pes_packet, pes_packets_of_188,
        run, tables_tstables,
    };

    const PMT_PID: u16 = 0x1000;
    const VIDEO_PID: u16 = 0x0100;
    const AUDIO_PID: u16 = 0x0101;

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
        multiplexed_of(name, inputs())
    }

    /// Writes what the muxer of the default settings makes of `inputs` to a
    /// file of `name`, and returns its path
    fn multiplexed_of(name: &str, inputs: Vec<Input>) -> String {
        let pid = |value: u16| Pid::try_from(value).unwrap();
        let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
        muxer.add_program(1, pid(PMT_PID), None, &[]).unwrap();
        muxer
            .add_stream(1, pid(VIDEO_PID), StreamType::ISO_IEC_13818_2_VIDEO, &[])
            .unwrap();
        muxer
            .add_stream(1, pid(AUDIO_PID), StreamType::ISO_IEC_11172_3_AUDIO, &[])
            .unwrap();
        let mut stream = Vec::new();
        for (pid_value, stream_id, timing, payload) in inputs {
            muxer
                .handle_pes_packet(pid(pid_value), &pes_packet(stream_id, timing, &payload))
                .unwrap();
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
    /// from the PCRs on either side of it (ISO/IEC 13818-1 §2.4.2.2,
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
}
