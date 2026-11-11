//! What the Program Stream `Muxer` writes, checked against ffprobe: the
//! streams the program stream map names and the PES packets with their
//! time stamps, with the time stamps as they come, leaping, and across the
//! wrap of 33 bits, with the system header and the map sent again or
//! updated, and at a fixed rate with padding
//!
//! The tests run `ffprobe` from `PATH`, so they are ignored unless asked
//! for; once asked for, a tool missing is a failure.

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
    clippy::arithmetic_side_effects,
    reason = "the times are well inside their range"
)]
mod tests {
    use std::collections::BTreeMap;

    use core::time::Duration;

    use mpeg2_systems::descriptor::DescriptorLoop;
    use mpeg2_systems::pes::StreamId;
    use mpeg2_systems::pes::extension::PStdBuffer;
    use mpeg2_systems::ps::demux::{Demuxer, Event};
    use mpeg2_systems::ps::mux::{DecoderBufferBound, Muxer, MuxerConfig};
    use mpeg2_systems::ps::psm::ElementaryStream;
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::Timestamp;

    use crate::tools::{Timing, ffprobe, pes_packet, pes_packets_of_program_stream};

    const VIDEO: u8 = 0xe0;
    const AUDIO: u8 = 0xc0;

    /// Times the PES packets of the fixture are given, one after the other
    const REPEATS: u64 = 20;

    /// 90 kHz ticks from one input to the next, 20 ms
    const STEP: u64 = 1_800;

    /// A PES packet to give: its `stream_id`, timing and payload
    type Input = (u8, Timing, Vec<u8>);

    /// The PES packets of the fixture in the order of their DTS, repeated
    /// [`REPEATS`] times, the DTS of the one at each index `dts_of` it,
    /// round the 33-bit circle, a picture with its PTS two steps after its
    /// DTS and an audio frame with its PTS alone
    fn inputs_at(dts_of: impl Fn(u64) -> u64) -> Vec<Input> {
        let mut fixture = pes_packets_of_program_stream();
        fixture.sort_by_key(|(_, ((_, dts), _))| *dts);
        (0..REPEATS)
            .flat_map(|_| fixture.clone())
            .zip((0..).map(|index| dts_of(index) % Timestamp::MODULUS))
            .map(|((stream_id, (_, payload)), dts)| {
                let timing = if stream_id == VIDEO {
                    (Some((dts + 2 * STEP) % Timestamp::MODULUS), Some(dts))
                } else {
                    (Some(dts), None)
                };
                (stream_id, timing, payload)
            })
            .collect()
    }

    /// The `id` ffmpeg's Program Stream demuxer gives a stream: the PES start
    /// code, `packet_start_code_prefix` followed by `stream_id`
    fn program_stream_id(stream_id: u8) -> u64 {
        0x100 | u64::from(stream_id)
    }

    /// Writes what the muxer, at 1 260 000 bytes a second and the other
    /// settings by default, makes of `inputs` to a file of `name`, and
    /// checks that ffprobe reads the streams registered and the PES packets
    /// given
    fn assert_read_as_given(name: &str, inputs: &[Input]) {
        multiplex_read_as_given(name, MuxerConfig::new(25_200), inputs, |_, _| {});
    }

    /// Writes what the muxer of `config` makes of `inputs`, `before` called
    /// with the index of each input before it is given, to a file of `name`,
    /// checks that ffprobe reads the streams registered and the PES packets
    /// given, and returns what was written
    fn multiplex_read_as_given(
        name: &str,
        config: MuxerConfig,
        inputs: &[Input],
        before: impl Fn(&mut Muxer, usize),
    ) -> Vec<u8> {
        let mut muxer = Muxer::new(config).unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                PStdBuffer::new(true, 224),
            )
            .unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
                PStdBuffer::new(false, 32),
            )
            .unwrap();
        let mut stream = Vec::new();
        let mut expected: BTreeMap<u64, Vec<Timing>> = BTreeMap::new();
        for (index, (stream_id, timing, payload)) in inputs.iter().enumerate() {
            before(&mut muxer, index);
            muxer
                .handle_pes_packet(&pes_packet(*stream_id, *timing, payload))
                .unwrap();
            muxer.poll_output(&mut stream);
            expected
                .entry(program_stream_id(*stream_id))
                .or_default()
                .push((timing.0, timing.1.or(timing.0)));
        }
        muxer.finish();
        muxer.poll_output(&mut stream);
        let path = format!("{}/{name}", env!("CARGO_TARGET_TMPDIR"));
        std::fs::write(&path, &stream).unwrap();

        // Why name the format: the probe heuristics are not what this checks.
        let (streams, probed) = ffprobe(&path, Some("mpeg"), false);
        let expected_streams = BTreeMap::from([
            (program_stream_id(AUDIO), "audio".to_owned()),
            (program_stream_id(VIDEO), "video".to_owned()),
        ]);
        assert_eq!((streams, probed), (expected_streams, expected), "{name}");
        stream
    }

    /// The system headers and the `program_stream_map_version` of the
    /// program stream maps the demuxer reports of `stream`, each when it
    /// differs from the one before
    fn headers_reported(stream: &[u8]) -> (usize, Vec<u8>) {
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(stream);
        demuxer.finish();
        let mut system_headers = 0;
        let mut versions = Vec::new();
        while let Some(event) = demuxer.poll_event() {
            if let Event::SystemHeader(_) = event {
                system_headers += 1;
            } else if let Event::ProgramStreamMap(map) = event {
                versions.push(map.program_stream_map_version.get());
            }
        }
        (system_headers, versions)
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_streams_and_pes_packets_given_read_in_ffprobe_as_given() {
        assert_read_as_given("ps_muxer.mpg", &inputs_at(|index| 90_000 + index * STEP));
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_across_a_leap_of_the_time_stamps() {
        assert_read_as_given(
            "ps_muxer_leap.mpg",
            &inputs_at(|index| 90_000 + index * STEP + if index >= 50 { 180_000 } else { 0 }),
        );
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_across_the_wrap_of_33_bits() {
        assert_read_as_given(
            "ps_muxer_wrap.mpg",
            &inputs_at(|index| Timestamp::MODULUS - 30 * STEP + index * STEP),
        );
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_with_the_headers_sent_again() {
        let stream = multiplex_read_as_given(
            "ps_muxer_headers_again.mpg",
            MuxerConfig::new(25_200).with_header_interval(Duration::from_millis(100)),
            &inputs_at(|index| 90_000 + index * STEP),
            |_, _| {},
        );

        let system_headers = stream
            .windows(4)
            .filter(|code| code == &[0, 0, 1, 0xbb])
            .count();
        assert!(system_headers > 1, "{system_headers} system headers");
        // Every system header the same, and the map unchanged (§2.7.8, §2.5.4.2)
        assert_eq!(headers_reported(&stream), (1, vec![0]));
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_across_an_update_of_the_map() {
        let descriptor = [0x80, 0x02, 0xaa, 0xbb];
        let stream = multiplex_read_as_given(
            "ps_muxer_map_updated.mpg",
            MuxerConfig::new(25_200),
            &inputs_at(|index| 90_000 + index * STEP),
            |muxer, index| {
                if index == 30 {
                    muxer
                        .update_stream(
                            ElementaryStream::new(
                                StreamType::ISO_IEC_13818_2_VIDEO,
                                StreamId::from(VIDEO),
                            )
                            .with_elementary_stream_info(
                                DescriptorLoop::from(descriptor.as_slice()),
                            ),
                        )
                        .unwrap();
                    muxer
                        .set_program_stream_info(DescriptorLoop::from(descriptor.as_slice()))
                        .unwrap();
                }
            },
        );

        // One change of the map, version 0 then 1 (§2.5.4.2); the system
        // header unchanged (§2.7.8)
        assert_eq!(headers_reported(&stream), (1, vec![0, 1]));
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_at_a_fixed_rate_across_a_leap() {
        let stream = multiplex_read_as_given(
            "ps_muxer_fixed_rate.mpg",
            MuxerConfig::new(25_200).with_fixed_rate(true),
            &inputs_at(|index| 90_000 + index * STEP + if index >= 50 { 180_000 } else { 0 }),
            |_, _| {},
        );

        let padding = stream
            .windows(4)
            .filter(|code| code == &[0, 0, 1, 0xbe])
            .count();
        assert!(padding > 0, "{padding} padding packets");
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_at_a_fixed_rate_in_packs_past_the_longest_pes_packet()
     {
        let stream = multiplex_read_as_given(
            "ps_muxer_fixed_rate_large_packs.mpg",
            MuxerConfig::new(25_200)
                .with_fixed_rate(true)
                .with_pack_size(200_000),
            &inputs_at(|index| 90_000 + index * STEP + if index >= 50 { 180_000 } else { 0 }),
            |_, _| {},
        );

        let padding = stream
            .windows(4)
            .filter(|code| code == &[0, 0, 1, 0xbe])
            .count();
        assert!(padding > 0, "{padding} padding packets");
    }

    /// Declares the VBV of the video, under the 224 KiB registered for it by
    /// at most the 6 KiB `BSadd` of a CSPS allows, before the first input
    fn declare_vbv(muxer: &mut Muxer, index: usize) {
        if index == 0 {
            muxer
                .set_decoder_buffer_bound(
                    StreamId::from(VIDEO),
                    None,
                    DecoderBufferBound::Vbv {
                        size: 220 * 1024,
                        rv_max: 1_150_000,
                    },
                )
                .unwrap();
        }
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_in_a_csps() {
        multiplex_read_as_given(
            "ps_muxer_csps.mpg",
            MuxerConfig::new(3_528)
                .with_pack_size(2_324)
                .with_csps(true),
            &inputs_at(|index| 90_000 + index * STEP),
            declare_vbv,
        );
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn the_pes_packets_read_in_ffprobe_as_given_in_a_csps_at_a_fixed_rate() {
        let stream = multiplex_read_as_given(
            "ps_muxer_csps_fixed_rate.mpg",
            MuxerConfig::new(3_528)
                .with_pack_size(2_324)
                .with_fixed_rate(true)
                .with_csps(true),
            &inputs_at(|index| 90_000 + index * STEP),
            declare_vbv,
        );

        assert_eq!(headers_reported(&stream), (1, vec![0]));
    }
}
