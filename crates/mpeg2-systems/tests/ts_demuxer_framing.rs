//! The Transport Stream demuxer over streams FFmpeg wrote: the packet grid
//! in each framing, behind garbage, cut short, and with packets altered

#[cfg(test)]
#[cfg(feature = "alloc")]
#[path = "support/demux.rs"]
#[expect(
    dead_code,
    reason = "each test file takes a part of what the tests share"
)]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a packet is cut at constant offsets well inside the fixture"
)]
mod support;

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    reason = "streams are cut at constant offsets well inside their length"
)]
mod tests {
    use mpeg2_systems::psi::section;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Error, PacketSize};
    use mpeg2_systems::ts::{ContinuityCounter, Pid};

    use crate::support::*;

    #[test]
    fn the_pat_pmt_and_pcrs_of_every_framing_are_reported_whatever_the_chunk_length() {
        for (stream, packet_size) in [
            (STANDARD_188, PacketSize::Standard188),
            (TIMESTAMPED_192, PacketSize::Timestamped192),
            (REED_SOLOMON_204, PacketSize::ReedSolomon204),
        ] {
            for chunk_length in [1, 7, 188, 1000, stream.len()] {
                assert_eq!(
                    demux(stream, chunk_length),
                    acquired_then(packet_size, fixture_events()),
                    "{packet_size:?} in chunks of {chunk_length}"
                );
            }
        }
    }

    #[test]
    fn garbage_before_the_stream_is_reported_with_its_bytes_before_sync() {
        let mut stream = vec![0xFF; 500];
        stream.extend_from_slice(STANDARD_188);
        let mut expected = vec![Seen::Unsynchronized(vec![0xFF; 500])];
        expected.extend(acquired_then(PacketSize::Standard188, fixture_events()));
        assert_eq!(demux(&stream, 1000), expected);
    }

    #[test]
    fn a_stream_cut_inside_its_last_packet_reports_the_partial_packet_at_the_end() {
        let stream = &STANDARD_188[..STANDARD_188.len() - 100];
        let mut expected = acquired_then(PacketSize::Standard188, fixture_events());
        expected.push(Seen::Unsynchronized(stream[stream.len() - 88..].to_vec()));
        assert_eq!(demux(stream, 1000), expected);
    }

    #[test]
    fn a_192_byte_stream_cut_inside_a_timestamp_is_unsynchronized_up_to_its_next_unit() {
        let stream = &TIMESTAMPED_192[2..];
        let mut expected = vec![Seen::Unsynchronized(stream[..190].to_vec())];
        expected.extend(acquired_then(PacketSize::Timestamped192, fixture_events()));
        for chunk_length in [1, 7, 188, 1000, stream.len()] {
            assert_eq!(
                demux(stream, chunk_length),
                expected,
                "in chunks of {chunk_length}"
            );
        }
    }

    #[test]
    fn a_192_byte_stream_cut_between_units_leaves_nothing_at_the_end() {
        let stream = &TIMESTAMPED_192[..TIMESTAMPED_192.len() - 192];
        assert_eq!(
            demux(stream, 1000),
            acquired_then(PacketSize::Timestamped192, fixture_events())
        );
    }

    #[test]
    fn a_stream_resynchronizes_after_two_missing_sync_bytes_without_a_false_discontinuity() {
        let mut stream = STANDARD_188.to_vec();
        stream[188 * 10] = 0x00;
        stream[188 * 11] = 0x00;
        let mut expected = acquired_then(PacketSize::Standard188, vec![pat(), pmt(), pcr(63_000)]);
        expected.extend([
            Seen::Error(Error::SyncByteMissing { found: 0x00 }),
            Seen::Error(Error::SyncLost),
            Seen::Unsynchronized(stream[188 * 11..188 * 12].to_vec()),
            Seen::SyncAcquired(PacketSize::Standard188),
        ]);
        expected.extend(fixture_events().into_iter().skip(3));
        assert_eq!(demux(&stream, 1000), expected);
    }

    #[test]
    fn a_skipped_continuity_counter_is_reported_once() {
        let mut expected = acquired_then(PacketSize::Standard188, vec![pat(), pmt(), pcr(63_000)]);
        expected.push(Seen::Error(Error::Discontinuity {
            pid: Pid::try_from(0x0100).unwrap(),
            expected: ContinuityCounter::try_from(2).unwrap(),
            found: ContinuityCounter::try_from(3).unwrap(),
        }));
        expected.extend(fixture_events().into_iter().skip(3));
        assert_eq!(demux(&without_packet(5), 1000), expected);
    }

    #[test]
    fn a_section_whose_crc_32_fails_is_reported_and_the_next_one_is_reassembled() {
        let mut stream = STANDARD_188.to_vec();
        stream[188 + 20] ^= 0x01;
        let seen = demux(&stream, 1000);
        let crc_error = Error::Section {
            pid: Pid::PAT,
            error: section::Error::Crc32 {
                crc_32: 0x2AB1_04B3,
                computed: 0x2AB1_04B2,
            },
        };
        // Why no PMT from packet 2: the PAT that names its PID is the one that failed
        let expected = acquired_then(
            PacketSize::Standard188,
            vec![
                Seen::Error(crc_error),
                pcr(63_000),
                pat(),
                pmt(),
                pcr(81_000),
                pcr(99_000),
            ],
        );
        assert_eq!(seen, expected);
    }

    #[test]
    fn a_packet_with_transport_error_indicator_set_reports_nothing_else() {
        let mut stream = STANDARD_188.to_vec();
        stream[188 * 3 + 1] |= 0x80;
        let mut expected = acquired_then(PacketSize::Standard188, vec![pat(), pmt()]);
        expected.push(Seen::Error(Error::TransportErrorIndicator {
            pid: Pid::try_from(0x0100).unwrap(),
        }));
        expected.extend(fixture_events().into_iter().skip(3));
        assert_eq!(demux(&stream, 1000), expected);
    }

    #[test]
    fn one_duplicate_packet_is_passed_over_and_a_second_is_a_discontinuity() {
        let packet = &STANDARD_188[188..188 * 2];
        let with_copies = |copies: usize| {
            let mut stream = STANDARD_188[..188 * 2].to_vec();
            for _ in 0..copies {
                stream.extend_from_slice(packet);
            }
            stream.extend_from_slice(&STANDARD_188[188 * 2..]);
            stream
        };
        assert_eq!(
            demux(&with_copies(1), 1000),
            acquired_then(PacketSize::Standard188, fixture_events())
        );
        let mut expected = acquired_then(PacketSize::Standard188, vec![pat()]);
        expected.push(Seen::Error(Error::Discontinuity {
            pid: Pid::PAT,
            expected: ContinuityCounter::try_from(1).unwrap(),
            found: ContinuityCounter::try_from(0).unwrap(),
        }));
        expected.extend(fixture_events().into_iter().skip(1));
        assert_eq!(demux(&with_copies(2), 1000), expected);
    }

    #[test]
    #[should_panic(expected = "after finish")]
    fn input_handed_over_before_the_end_is_drained_is_refused() {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.handle_input(STANDARD_188);
        demuxer.finish();
        demuxer.handle_input(STANDARD_188);
    }
}
