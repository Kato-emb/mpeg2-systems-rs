//! The Transport Stream demuxer over the PSI sections it reassembles and the
//! PAT, PMT and CAT versions it reports

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
#[path = "support/sections.rs"]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "a packet and a section are laid out at constant offsets well inside them"
)]
mod sections;

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "streams are cut at constant offsets well inside their length"
)]
mod tests {
    use mpeg2_systems::psi::TableId;
    use mpeg2_systems::psi::section::Section;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Error, PacketSize};
    use mpeg2_systems::ts::{ContinuityCounter, Pid};

    use crate::sections::*;
    use crate::support::*;

    #[test]
    fn sections_inside_one_packet_and_across_packets_are_reported_in_order() {
        let whole = short_section(0x80, &[0xAA; 20]);
        let across = short_section(0x81, &[0xBB; 300]);
        let after = short_section(0x82, &[0xCC; 10]);
        // Why split there: the head must run to the end of the packet, or
        // the padding after it would be read as more of the section
        let (head, tail) = across.split_at(184 - 1 - whole.len());
        let first = [&[0x00][..], &whole, head].concat();
        let second = [&[u8::try_from(tail.len()).unwrap()][..], tail, &after].concat();
        let stream = behind_null_packets(&[
            packet(0x0000, true, 0, None, Some(&first)),
            packet(0x0000, true, 1, None, Some(&second)),
        ]);
        let expected = acquired_then(
            PacketSize::Standard188,
            vec![
                seen_section(0x0000, &Section::new(TableId::from(0x80), &[0xAA; 20])),
                seen_section(0x0000, &Section::new(TableId::from(0x81), &[0xBB; 300])),
                seen_section(0x0000, &Section::new(TableId::from(0x82), &[0xCC; 10])),
            ],
        );
        for chunk_length in [1, 7, 188, 1000, stream.len()] {
            assert_eq!(
                demux(&stream, chunk_length),
                expected,
                "in chunks of {chunk_length}"
            );
        }
    }

    #[test]
    fn a_discontinuity_indicator_without_payload_drops_the_section_in_flight_and_sets_the_counter()
    {
        let across = short_section(0x80, &[0xBB; 300]);
        let (head, tail) = across.split_at(184 - 1);
        let stream = behind_null_packets(&[
            packet(0x0000, true, 0, None, Some(&[&[0x00][..], head].concat())),
            packet(0x0000, false, 5, Some(&[0x80]), None),
            packet(0x0000, false, 6, None, Some(tail)),
        ]);
        assert_eq!(
            demux(&stream, 1000),
            acquired_then(PacketSize::Standard188, Vec::new())
        );
    }

    #[test]
    fn a_section_lent_from_a_packet_keeps_its_bytes_across_more_input() {
        let first = short_section(0x80, &[0xAA; 7]);
        let second = short_section(0x80, &[0xBB; 5]);
        let stream = behind_null_packets(&[packet(
            0x0001,
            true,
            0,
            None,
            Some(&[&[0x00][..], &first, &second].concat()),
        )]);
        let more = packet(0x1FFF, false, 0, None, Some(&[]));
        for next_input in [&[][..], &more[..]] {
            let mut demuxer = Demuxer::new(DemuxerConfig::default());
            demuxer.handle_input(&stream);
            let mut seen = vec![
                seen_event(demuxer.poll_event().unwrap()),
                seen_event(demuxer.poll_event().unwrap()),
            ];
            demuxer.handle_input(next_input);
            seen.push(seen_event(demuxer.poll_event().unwrap()));
            assert_eq!(
                seen,
                [
                    Seen::SyncAcquired(PacketSize::Standard188),
                    seen_section(0x0001, &Section::new(TableId::from(0x80), &[0xAA; 7])),
                    seen_section(0x0001, &Section::new(TableId::from(0x80), &[0xBB; 5])),
                ],
                "after {} more bytes",
                next_input.len()
            );
        }
    }

    /// The fixture with each PAT replaced by one of `transport_stream_id` 1
    /// at `version_number`, naming `programs` (`program_number`, PID)
    fn with_pat(version_number: u8, programs: &[(u16, u16)]) -> Vec<u8> {
        let section = pat_section(version_number, programs);
        let mut stream = STANDARD_188.to_vec();
        for packet in stream.chunks_mut(188) {
            if packet[1] & 0x1F == 0 && packet[2] == 0 {
                packet[4] = 0x00;
                packet[5..5 + section.len()].copy_from_slice(&section);
                packet[5 + section.len()..].fill(0xFF);
            }
        }
        stream
    }

    #[test]
    fn the_pat_and_pmt_the_stream_resends_are_not_reported_again() {
        assert_eq!(
            demux_in_turn(
                Demuxer::new(DemuxerConfig::default()),
                &[STANDARD_188, STANDARD_188]
            ),
            [
                acquired_then(PacketSize::Standard188, fixture_events()),
                pcrs()
            ]
        );
    }

    #[test]
    fn a_new_pat_version_is_reported_once() {
        assert_eq!(
            demux_in_turn(
                Demuxer::new(DemuxerConfig::default()),
                &[STANDARD_188, &with_pat(1, &[(1, 0x1000)])]
            ),
            [
                acquired_then(PacketSize::Standard188, fixture_events()),
                [vec![pat_of(1, vec![program(1, 0x1000)])], pcrs()].concat(),
            ]
        );
    }

    #[test]
    fn a_program_dropped_from_the_pat_and_named_again_reports_its_pmt_again() {
        assert_eq!(
            demux_in_turn(
                Demuxer::new(DemuxerConfig::default()),
                &[
                    STANDARD_188,
                    &with_pat(1, &[]),
                    &with_pat(2, &[(1, 0x1000)])
                ]
            ),
            [
                acquired_then(PacketSize::Standard188, fixture_events()),
                [vec![pat_of(1, Vec::new())], pcrs()].concat(),
                [vec![pat_of(2, vec![program(1, 0x1000)]), pmt()], pcrs()].concat(),
            ]
        );
    }

    #[test]
    fn a_cat_is_reported() {
        let descriptors = [0x09, 0x04, 0x0B, 0x00, 0xE1, 0x23];
        let cat = table_section(0x01, 0xFFFF, 4, &descriptors);
        let stream = behind_null_packets(&[packet(
            0x0001,
            true,
            0,
            None,
            Some(&[&[0x00][..], &cat].concat()),
        )]);
        assert_eq!(
            demux(&stream, 1000),
            acquired_then(
                PacketSize::Standard188,
                vec![Seen::CaSection {
                    version_number: version(4),
                    current_next_indicator: true,
                    section_number: 0,
                    last_section_number: 0,
                    descriptors: descriptors.to_vec(),
                }]
            )
        );
    }

    #[test]
    fn the_packets_after_a_pmt_are_analyzed_once_the_pmt_is_polled() {
        let mut stream = STANDARD_188[..188 * 3].to_vec();
        let mut skipping = STANDARD_188[188 * 2..188 * 3].to_vec();
        skipping[3] = (skipping[3] & 0xF0) | ((skipping[3] + 2) & 0x0F);
        stream.extend_from_slice(&skipping);
        // Why a packet after: the framer cuts a packet once the sync_byte
        // after it confirms the grid
        stream.extend(packet(0x1FFF, false, 0, None, Some(&[])));
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.handle_input(&stream);
        let mut seen = Vec::new();
        for _ in 0..3 {
            seen.push(seen_event(demuxer.poll_event().unwrap()));
        }
        let pmt_polled = seen.last() == Some(&pmt());
        seen.push(seen_event(demuxer.poll_event().unwrap()));
        let continuity_counter = ContinuityCounter::try_from(skipping[3] & 0x0F).unwrap();
        assert_eq!(
            (pmt_polled, seen),
            (
                true,
                acquired_then(
                    PacketSize::Standard188,
                    vec![
                        pat(),
                        pmt(),
                        Seen::Error(Error::Discontinuity {
                            pid: Pid::try_from(0x1000).unwrap(),
                            expected: ContinuityCounter::try_from((skipping[3] + 15) & 0x0F)
                                .unwrap(),
                            found: continuity_counter,
                        }),
                    ]
                )
            )
        );
    }
}
