//! The Transport Stream demuxer over the sections of the PIDs a caller
//! subscribes to

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
    use mpeg2_systems::psi::section::{self, Section};
    use mpeg2_systems::ts::Pid;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Error, PacketSize};

    use crate::sections::*;
    use crate::support::*;

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
                        Seen::Unsynchronized(broken.clone()),
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

    #[test]
    fn a_section_in_flight_when_the_pid_is_subscribed_to_is_not_reported_and_the_next_one_is() {
        let in_flight = private_section(0, true, &[0xAA; 300]);
        let after = short_section(0x81, &[0xBB; 10]);
        let (head, tail) = in_flight.split_at(184 - 1);
        let before = behind_null_packets(&[packet(
            0x0300,
            true,
            0,
            None,
            Some(&[&[0x00][..], head].concat()),
        )]);
        let next = packet(
            0x0300,
            true,
            1,
            None,
            Some(&[&[u8::try_from(tail.len()).unwrap()][..], tail, &after].concat()),
        );
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        let mut seen = Vec::new();
        // Why hand over the first byte of the next packet: the packet before
        // it is cut, and its section started, only once that sync_byte
        // confirms the grid
        demuxer.handle_input(&[&before[..], &next[..1]].concat());
        drain(&mut demuxer, &mut seen);
        demuxer.subscribe_sections(Pid::try_from(0x0300).unwrap());
        demuxer.handle_input(&next[1..]);
        drain(&mut demuxer, &mut seen);
        demuxer.finish();
        drain(&mut demuxer, &mut seen);
        assert_eq!(
            seen,
            acquired_then(
                PacketSize::Standard188,
                vec![seen_section(0x0300, &read(&after))]
            )
        );
    }

    #[test]
    fn subscribing_the_cat_pid_keeps_its_resends_unreported() {
        let descriptors = [0x09, 0x04, 0x0B, 0x00, 0xE1, 0x23];
        let cat = |version_number: u8| table_section(0x01, 0xFFFF, version_number, &descriptors);
        let packets: Vec<Vec<u8>> = (0u8..)
            .zip([cat(4), cat(4), cat(5)])
            .map(|(continuity_counter, section)| {
                packet(
                    0x0001,
                    true,
                    continuity_counter,
                    None,
                    Some(&[&[0x00][..], &section].concat()),
                )
            })
            .collect();
        let reported = |version_number: u8| Seen::CaSection {
            version_number: version(version_number),
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            descriptors: descriptors.to_vec(),
        };
        assert_eq!(
            demux_subscribed(&behind_null_packets(&packets), &[0x0001]),
            acquired_then(PacketSize::Standard188, vec![reported(4), reported(5)])
        );
    }

    #[test]
    fn a_section_of_a_subscribed_pid_that_cannot_be_read_is_reported_as_an_error() {
        let mut broken = private_section(0, true, &[0xAA; 10]);
        let crc_start = broken.len() - 4;
        let computed = u32::from_be_bytes(broken[crc_start..].try_into().unwrap());
        broken[crc_start + 3] ^= 0x01;
        let crc_32 = u32::from_be_bytes(broken[crc_start..].try_into().unwrap());
        let stream = behind_null_packets(&[packet(
            0x0300,
            true,
            0,
            None,
            Some(&[&[0x00][..], &broken].concat()),
        )]);
        assert_eq!(
            demux_subscribed(&stream, &[0x0300]),
            acquired_then(
                PacketSize::Standard188,
                vec![Seen::Error(Error::Section {
                    pid: Pid::try_from(0x0300).unwrap(),
                    error: section::Error::Crc32 { crc_32, computed },
                })]
            )
        );
    }
}
