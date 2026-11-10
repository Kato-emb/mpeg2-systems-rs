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

    use crate::support::*;

    /// A Transport Packet laid out by hand: the header, an adaptation field
    /// of `adaptation_field` bytes (its length byte not included) if any, then
    /// `payload` if any, padded with 0xFF
    fn packet(
        pid: u16,
        payload_unit_start_indicator: bool,
        continuity_counter: u8,
        adaptation_field: Option<&[u8]>,
        payload: Option<&[u8]>,
    ) -> Vec<u8> {
        let [high, low] = pid.to_be_bytes();
        let adaptation_field_control = match (adaptation_field, payload) {
            (None, _) => 0x10,
            (Some(_), None) => 0x20,
            (Some(_), Some(_)) => 0x30,
        };
        let mut bytes = vec![
            0x47,
            high | if payload_unit_start_indicator {
                0x40
            } else {
                0
            },
            low,
            adaptation_field_control | continuity_counter,
        ];
        if let Some(field) = adaptation_field {
            let length = if payload.is_some() { field.len() } else { 183 };
            bytes.push(u8::try_from(length).unwrap());
            bytes.extend_from_slice(field);
            bytes.resize(5 + length, 0xFF);
        }
        bytes.extend_from_slice(payload.unwrap_or_default());
        bytes.resize(188, 0xFF);
        bytes
    }

    /// A short-form section of `table_id` with `body`, which carries no
    /// `CRC_32`
    fn short_section(table_id: u8, body: &[u8]) -> Vec<u8> {
        let [high, low] = u16::try_from(body.len()).unwrap().to_be_bytes();
        let mut bytes = vec![table_id, 0x30 | high, low];
        bytes.extend_from_slice(body);
        bytes
    }

    /// Null packets enough to acquire the grid, then `packets`
    fn behind_null_packets(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut stream = Vec::new();
        for _ in 0..5 {
            stream.extend(packet(0x1FFF, false, 0, None, Some(&[])));
        }
        for bytes in packets {
            stream.extend_from_slice(bytes);
        }
        stream
    }

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

    /// The events of each stream in turn through `demuxer`, the input ended
    /// after each
    fn demux_in_turn(mut demuxer: Demuxer, streams: &[&[u8]]) -> Vec<Vec<Seen>> {
        let mut turns = Vec::new();
        for stream in streams {
            let mut seen = Vec::new();
            demuxer.handle_input(stream);
            drain(&mut demuxer, &mut seen);
            demuxer.finish();
            drain(&mut demuxer, &mut seen);
            turns.push(seen);
        }
        turns
    }

    /// A long-form section of `table_id`, with `table_id_extension`, at
    /// version `version_number`, in force, its `CRC_32` computed
    fn table_section(
        table_id: u8,
        table_id_extension: u16,
        version_number: u8,
        body: &[u8],
    ) -> Vec<u8> {
        let [high, low] = u16::try_from(body.len() + 9).unwrap().to_be_bytes();
        let mut bytes = vec![table_id, 0xB0 | high, low];
        bytes.extend_from_slice(&table_id_extension.to_be_bytes());
        bytes.extend_from_slice(&[0xC1 | (version_number << 1), 0x00, 0x00]);
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(&mpeg2_systems::crc::crc_32(&bytes).to_be_bytes());
        bytes
    }

    /// A PAT section of `transport_stream_id` 1 at `version_number`, naming
    /// `programs` (`program_number`, PID)
    fn pat_section(version_number: u8, programs: &[(u16, u16)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (program_number, pid) in programs {
            body.extend_from_slice(&program_number.to_be_bytes());
            body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
        }
        table_section(0x00, 0x0001, version_number, &body)
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

    fn pcrs() -> Vec<Seen> {
        vec![pcr(63_000), pcr(81_000), pcr(99_000)]
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

    /// A demuxer subscribed to the sections of `pids`
    fn subscribed_to(pids: &[u16]) -> Demuxer {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        for &pid in pids {
            demuxer.subscribe_sections(Pid::try_from(pid).unwrap());
        }
        demuxer
    }

    /// The events of `stream` handed over in one piece and ended, the
    /// sections of `pids` subscribed to first
    fn demux_subscribed(stream: &[u8], pids: &[u16]) -> Vec<Seen> {
        demux_in_turn(subscribed_to(pids), &[stream]).concat()
    }

    fn read(bytes: &[u8]) -> Section<'_> {
        Section::read(bytes).unwrap()
    }

    /// A private section, `table_id` 0x80, at `version_number`, its
    /// `current_next_indicator` as given
    fn private_section(version_number: u8, current_next_indicator: bool, body: &[u8]) -> Vec<u8> {
        let mut bytes = table_section(0x80, 0x1234, version_number, body);
        if !current_next_indicator {
            bytes[5] &= 0xFE;
            let crc_start = bytes.len() - 4;
            let crc = mpeg2_systems::crc::crc_32(&bytes[..crc_start]).to_be_bytes();
            bytes[crc_start..].copy_from_slice(&crc);
        }
        bytes
    }

    #[test]
    fn the_sections_of_a_subscribed_pid_are_reported_long_and_short_and_across_packets() {
        let long = private_section(3, true, &[0xAA; 300]);
        let short = short_section(0x81, &[0xBB; 10]);
        let (head, tail) = long.split_at(184 - 1);
        let second = [&[u8::try_from(tail.len()).unwrap()][..], tail, &short].concat();
        let stream = behind_null_packets(&[
            packet(0x0300, true, 0, None, Some(&[&[0x00][..], head].concat())),
            packet(0x0300, true, 1, None, Some(&second)),
        ]);
        assert_eq!(
            (
                demux_subscribed(&stream, &[0x0300]),
                demux_subscribed(&stream, &[])
            ),
            (
                acquired_then(
                    PacketSize::Standard188,
                    vec![
                        seen_section(0x0300, &read(&long)),
                        seen_section(0x0300, &read(&short)),
                    ]
                ),
                acquired_then(PacketSize::Standard188, Vec::new()),
            )
        );
    }

    #[test]
    fn every_section_of_a_subscribed_pid_is_reported_a_repeated_one_and_one_not_in_force_included()
    {
        let in_force = private_section(3, true, &[0xAA; 20]);
        let next = private_section(4, false, &[0xBB; 20]);
        let sections = [&in_force, &in_force, &next];
        let packets: Vec<Vec<u8>> = (0u8..)
            .zip(sections)
            .map(|(continuity_counter, section)| {
                packet(
                    0x0300,
                    true,
                    continuity_counter,
                    None,
                    Some(&[&[0x00][..], section].concat()),
                )
            })
            .collect();
        assert_eq!(
            demux_subscribed(&behind_null_packets(&packets), &[0x0300]),
            acquired_then(
                PacketSize::Standard188,
                sections
                    .iter()
                    .map(|section| seen_section(0x0300, &read(section)))
                    .collect()
            )
        );
    }

    #[test]
    fn subscribing_the_pat_cat_and_program_map_pids_changes_none_of_their_events() {
        assert_eq!(
            demux_subscribed(STANDARD_188, &[0x0000, 0x0001, 0x1000, 0x0300]),
            acquired_then(PacketSize::Standard188, fixture_events())
        );
    }

    /// A packet of the PAT [`pat_section`] gives
    fn pat_packet(version_number: u8, programs: &[(u16, u16)], continuity_counter: u8) -> Vec<u8> {
        let section = pat_section(version_number, programs);
        packet(
            0x0000,
            true,
            continuity_counter,
            None,
            Some(&[&[0x00][..], &section].concat()),
        )
    }

    /// A PMT section of program 1, its PCR on PID 0x100 and no stream
    fn program_map_section() -> Vec<u8> {
        table_section(0x02, 0x0001, 0, &[0xE1, 0x00, 0xF0, 0x00])
    }

    /// A packet of the PMT [`program_map_section`] gives, on PID 0x1000
    fn program_map_packet(continuity_counter: u8) -> Vec<u8> {
        packet(
            0x1000,
            true,
            continuity_counter,
            None,
            Some(&[&[0x00][..], &program_map_section()].concat()),
        )
    }

    #[test]
    fn a_subscribed_program_map_pid_the_pat_stops_naming_goes_on_reporting_its_sections() {
        let stream = behind_null_packets(&[
            pat_packet(0, &[(1, 0x1000)], 0),
            program_map_packet(0),
            pat_packet(1, &[], 1),
            program_map_packet(1),
        ]);
        let typed = Seen::TsProgramMapSection {
            pid: 0x1000,
            program_number: 1,
            version_number: version(0),
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            pcr_pid: Some(0x0100),
            program_info: Vec::new(),
            elementary_streams: Vec::new(),
        };
        let reported = |rest: Vec<Seen>| {
            acquired_then(
                PacketSize::Standard188,
                [
                    vec![
                        pat_of(0, vec![program(1, 0x1000)]),
                        typed.clone(),
                        pat_of(1, Vec::new()),
                    ],
                    rest,
                ]
                .concat(),
            )
        };
        let section = program_map_section();
        assert_eq!(
            (
                demux_subscribed(&stream, &[0x1000]),
                demux_subscribed(&stream, &[])
            ),
            (
                reported(vec![seen_section(0x1000, &read(&section))]),
                reported(Vec::new()),
            )
        );
    }

    #[test]
    fn unsubscribing_drops_the_sections_not_yet_polled_and_the_one_in_flight() {
        let polled = private_section(0, true, &[0xAA; 10]);
        let queued = private_section(1, true, &[0xBB; 10]);
        let in_flight = private_section(2, true, &[0xCC; 200]);
        let after = private_section(3, true, &[0xDD; 10]);
        let head_length = 184 - 1 - polled.len() - queued.len();
        let (head, tail) = in_flight.split_at(head_length);
        let stream = behind_null_packets(&[
            packet(
                0x0300,
                true,
                0,
                None,
                Some(&[&[0x00][..], &polled, &queued, head].concat()),
            ),
            packet(
                0x0300,
                true,
                1,
                None,
                Some(&[&[u8::try_from(tail.len()).unwrap()][..], tail, &after].concat()),
            ),
        ]);
        let pid = Pid::try_from(0x0300).unwrap();
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.subscribe_sections(pid);
        demuxer.handle_input(&stream);
        let mut seen = vec![
            seen_event(demuxer.poll_event().unwrap()),
            seen_event(demuxer.poll_event().unwrap()),
        ];
        demuxer.unsubscribe_sections(pid);
        demuxer.subscribe_sections(pid);
        drain(&mut demuxer, &mut seen);
        demuxer.finish();
        drain(&mut demuxer, &mut seen);
        assert_eq!(
            seen,
            acquired_then(
                PacketSize::Standard188,
                vec![
                    seen_section(0x0300, &read(&polled)),
                    seen_section(0x0300, &read(&after)),
                ]
            )
        );
    }

    #[test]
    fn unsubscribing_a_program_map_pid_the_pat_names_keeps_its_sections_reported() {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        let pid = Pid::try_from(0x1000).unwrap();
        demuxer.subscribe_sections(pid);
        demuxer.handle_input(STANDARD_188);
        let mut seen = vec![
            seen_event(demuxer.poll_event().unwrap()),
            seen_event(demuxer.poll_event().unwrap()),
        ];
        demuxer.unsubscribe_sections(pid);
        drain(&mut demuxer, &mut seen);
        demuxer.finish();
        drain(&mut demuxer, &mut seen);
        assert_eq!(
            seen,
            acquired_then(PacketSize::Standard188, fixture_events())
        );
    }

    #[test]
    fn a_pid_subscribed_to_for_sections_and_pes_packets_is_reassembled_both_ways() {
        let section = short_section(0x81, &[0xBB; 10]);
        let payload = [&[0x00][..], &section].concat();
        let stream = behind_null_packets(&[packet(0x0300, true, 0, None, Some(&payload))]);
        let mut demuxer = subscribed_to(&[0x0300]);
        demuxer.subscribe_pes(Pid::try_from(0x0300).unwrap());
        let seen = demux_in_turn(demuxer, &[&stream]).concat();
        let mut bytes = payload.clone();
        bytes.resize(184, 0xFF);
        assert_eq!(
            seen,
            acquired_then(
                PacketSize::Standard188,
                vec![
                    seen_section(0x0300, &read(&section)),
                    Seen::TruncatedPesPacket {
                        pid: 0x0300,
                        bytes,
                        start_offset: 5 * 188,
                    },
                ]
            )
        );
    }

    #[test]
    fn a_subscription_stays_across_a_sync_loss_and_the_end_of_input() {
        let section = private_section(0, true, &[0xAA; 10]);
        let carrying = packet(
            0x0300,
            true,
            0,
            None,
            Some(&[&[0x00][..], &section].concat()),
        );
        let mut broken = packet(0x1FFF, false, 0, None, Some(&[]));
        broken[0] = 0x00;
        let mut resynchronized = vec![carrying.clone(), broken.clone(), broken.clone()];
        resynchronized.extend(core::iter::repeat_n(
            packet(0x1FFF, false, 0, None, Some(&[])),
            5,
        ));
        resynchronized.push(carrying.clone());
        let streams = [
            behind_null_packets(&resynchronized),
            behind_null_packets(&[carrying]),
        ];
        let turns = demux_in_turn(subscribed_to(&[0x0300]), &[&streams[0], &streams[1]]);
        let reported = seen_section(0x0300, &read(&section));
        assert_eq!(
            turns,
            [
                acquired_then(
                    PacketSize::Standard188,
                    vec![
                        reported.clone(),
                        Seen::Error(Error::SyncByteMissing { found: 0x00 }),
                        Seen::Error(Error::SyncLost),
                        Seen::Unsynchronized(broken),
                        Seen::SyncAcquired(PacketSize::Standard188),
                        reported.clone(),
                    ]
                ),
                vec![reported],
            ]
        );
    }

    #[test]
    fn the_sections_of_a_pid_not_subscribed_to_are_not_reported_beside_one_subscribed_to() {
        let section = private_section(0, true, &[0xAA; 10]);
        let payload = [&[0x00][..], &section].concat();
        let stream = behind_null_packets(&[
            packet(0x0300, true, 0, None, Some(&payload)),
            packet(0x0400, true, 0, None, Some(&payload)),
        ]);
        assert_eq!(
            demux_subscribed(&stream, &[0x0400]),
            acquired_then(
                PacketSize::Standard188,
                vec![seen_section(0x0400, &read(&section))]
            )
        );
    }

    #[test]
    fn subscribing_the_pat_and_program_map_pids_keeps_their_resends_unreported_and_the_pmt_typed() {
        assert_eq!(
            demux_in_turn(
                subscribed_to(&[0x0000, 0x1000]),
                &[STANDARD_188, STANDARD_188]
            ),
            [
                acquired_then(PacketSize::Standard188, fixture_events()),
                pcrs()
            ]
        );
    }

    #[test]
    fn unsubscribing_drops_the_section_errors_not_yet_polled() {
        let good = private_section(0, true, &[0xAA; 10]);
        let mut broken = private_section(1, true, &[0xBB; 10]);
        *broken.last_mut().unwrap() ^= 0x01;
        let stream = behind_null_packets(&[packet(
            0x0300,
            true,
            0,
            None,
            Some(&[&[0x00][..], &good, &broken].concat()),
        )]);
        let mut demuxer = subscribed_to(&[0x0300]);
        demuxer.handle_input(&stream);
        let mut seen = vec![
            seen_event(demuxer.poll_event().unwrap()),
            seen_event(demuxer.poll_event().unwrap()),
        ];
        demuxer.unsubscribe_sections(Pid::try_from(0x0300).unwrap());
        drain(&mut demuxer, &mut seen);
        demuxer.finish();
        drain(&mut demuxer, &mut seen);
        assert_eq!(
            seen,
            acquired_then(
                PacketSize::Standard188,
                vec![seen_section(0x0300, &read(&good))]
            )
        );
    }

    /// A PAT section of `transport_stream_id` 1, section 0 of 2, naming
    /// `programs`
    fn first_of_two_pat_sections(version_number: u8, programs: &[(u16, u16)]) -> Vec<u8> {
        let mut bytes = pat_section(version_number, programs);
        bytes[7] = 0x01;
        let crc_start = bytes.len() - 4;
        let crc = mpeg2_systems::crc::crc_32(&bytes[..crc_start]).to_be_bytes();
        bytes[crc_start..].copy_from_slice(&crc);
        bytes
    }

    #[test]
    fn unsubscribing_a_pid_not_subscribed_changes_nothing_on_a_pid_left_from_a_pat_version_dropped()
    {
        let section = short_section(0x80, &[0xAA; 10]);
        let pat_packet = |version_number: u8, programs: &[(u16, u16)], continuity_counter: u8| {
            let pat = first_of_two_pat_sections(version_number, programs);
            packet(
                0x0000,
                true,
                continuity_counter,
                None,
                Some(&[&[0x00][..], &pat].concat()),
            )
        };
        let stream = behind_null_packets(&[
            pat_packet(0, &[(1, 0x1000)], 0),
            pat_packet(1, &[], 1),
            packet(
                0x1000,
                true,
                0,
                None,
                Some(&[&[0x00][..], &section].concat()),
            ),
        ]);
        let run = |unsubscribe: bool| {
            let mut demuxer = Demuxer::new(DemuxerConfig::default());
            demuxer.handle_input(&stream);
            let mut seen = Vec::new();
            for _ in 0..3 {
                seen.push(seen_event(demuxer.poll_event().unwrap()));
            }
            if unsubscribe {
                demuxer.unsubscribe_sections(Pid::try_from(0x1000).unwrap());
            }
            drain(&mut demuxer, &mut seen);
            demuxer.finish();
            drain(&mut demuxer, &mut seen);
            seen
        };
        let unchanged = run(false);
        assert_eq!(
            (run(true), unchanged.last().cloned()),
            (unchanged, Some(seen_section(0x1000, &read(&section))))
        );
    }

    #[test]
    fn a_program_map_table_id_without_the_syntax_is_reported_as_read_on_a_pid_the_pat_does_not_name()
     {
        let section = short_section(0x02, &[0xAA; 10]);
        let payload = [&[0x00][..], &section].concat();
        let on = |pid: u16| {
            behind_null_packets(&[
                pat_packet(0, &[(1, 0x1000)], 0),
                packet(pid, true, 0, None, Some(&payload)),
            ])
        };
        let section_on_0x0300 = demux_subscribed(&on(0x0300), &[0x0300]);
        let section_on_the_program_map_pid = demux_subscribed(&on(0x1000), &[]);
        assert_eq!(
            (
                section_on_0x0300.last().cloned(),
                matches!(
                    section_on_the_program_map_pid.last(),
                    Some(Seen::Error(Error::TsProgramMapSection { .. }))
                )
            ),
            (Some(seen_section(0x0300, &read(&section))), true)
        );
    }
}
