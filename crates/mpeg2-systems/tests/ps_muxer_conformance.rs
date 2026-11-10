//! What the Program Stream `Muxer` writes, checked against ffprobe: the
//! streams the program stream map names and the PES packets with their
//! time stamps, with the time stamps as they come, leaping, and across the
//! wrap of 33 bits
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

    use mpeg2_systems::pes::StreamId;
    use mpeg2_systems::pes::extension::PStdBuffer;
    use mpeg2_systems::ps::mux::{Muxer, MuxerConfig};
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
        let mut muxer = Muxer::new(MuxerConfig::new(25_200)).unwrap();
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
        for (stream_id, timing, payload) in inputs {
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
}
