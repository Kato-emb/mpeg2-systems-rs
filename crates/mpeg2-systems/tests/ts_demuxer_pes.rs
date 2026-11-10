//! The Transport Stream demuxer over the PES packets of the PIDs subscribed
//! to, in streams FFmpeg wrote

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
    use core::num::NonZeroU16;

    use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    use mpeg2_systems::psi::TableId;
    use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
    use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
    use mpeg2_systems::psi::section::{Section, SectionSyntax};
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::Timestamp;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Error, Event};
    use mpeg2_systems::ts::mux::PacketWriter;
    use mpeg2_systems::ts::packet::TransportPacket;
    use mpeg2_systems::ts::{ContinuityCounter, Pid};

    use crate::support::*;

    const VIDEO_PID: u16 = 0x0100;
    const AUDIO_PID: u16 = 0x0101;

    /// A demuxer subscribed to the video and audio PIDs of the fixtures
    fn subscribed(config: DemuxerConfig) -> Demuxer {
        let mut demuxer = Demuxer::new(config);
        for pid in [VIDEO_PID, AUDIO_PID] {
            demuxer.subscribe_pes(Pid::try_from(pid).unwrap());
        }
        demuxer
    }

    fn demux_subscribed(demuxer: &mut Demuxer, stream: &[u8], chunk_length: usize) -> Vec<Seen> {
        let mut seen = Vec::new();
        for chunk in stream.chunks(chunk_length) {
            demuxer.handle_input(chunk);
            drain(demuxer, &mut seen);
        }
        demuxer.finish();
        drain(demuxer, &mut seen);
        seen
    }

    /// The events of `seen` about the PES packets of `pid`, an error on the
    /// PID included
    fn of_pid(seen: Vec<Seen>, pid: u16) -> Vec<Seen> {
        seen.into_iter()
            .filter(|seen| {
                matches!(
                    seen,
                    Seen::PesPacket { pid: of, .. }
                    | Seen::UnreadablePesPacket { pid: of, .. }
                    | Seen::TruncatedPesPacket { pid: of, .. } if *of == pid
                ) || matches!(
                    seen,
                    Seen::Error(
                        Error::PesBufferOverrun { pid: of, .. } | Error::Discontinuity { pid: of, .. }
                    ) if of.get() == pid
                )
            })
            .collect()
    }

    /// The PES events of `seen`, of either PID
    fn pes_packets(seen: Vec<Seen>) -> Vec<Seen> {
        seen.into_iter()
            .filter(|seen| matches!(seen, Seen::PesPacket { .. }))
            .collect()
    }

    /// The PES packet of the fixture that starts in the packet at `index`,
    /// as the previous implementation reported it, the unit `unit_length`
    /// long and `leading` bytes before the stream
    fn fixture_pes(index: u64, unit_length: u64, leading: u64) -> Seen {
        let start_offset = leading + index * unit_length;
        let (_, pid, stream_id, pts, dts, payload_length) = [
            (3, VIDEO_PID, 0xE0, 144_000, Some(126_000), 1803),
            (15, VIDEO_PID, 0xE0, 162_000, Some(144_000), 1050),
            (23, VIDEO_PID, 0xE0, 180_000, Some(162_000), 716),
            (28, AUDIO_PID, 0xC0, 143_018, None, 2925),
            (47, AUDIO_PID, 0xC0, 175_933, None, 1881),
        ]
        .into_iter()
        .find(|(starts_in, ..)| *starts_in == index)
        .unwrap();
        Seen::PesPacket {
            pid,
            stream_id,
            pts: Some(pts),
            dts,
            payload_length,
            start_offset,
        }
    }

    /// The PES packets of the fixture in the order they end: the video
    /// packets of an unspecified length end at the next one, the last at
    /// the end of input
    fn fixture_pes_packets(unit_length: u64, leading: u64) -> Vec<Seen> {
        [3, 15, 28, 47, 23]
            .into_iter()
            .map(|index| fixture_pes(index, unit_length, leading))
            .collect()
    }

    /// The payloads of the fixture's packets on `pid`, in order, read with
    /// the packet layer alone
    fn payloads(stream: &[u8], pid: u16) -> Vec<(usize, Vec<u8>)> {
        stream
            .chunks(188)
            .enumerate()
            .filter_map(|(index, bytes)| {
                let packet = TransportPacket::read(bytes).unwrap();
                (packet.pid.get() == pid)
                    .then(|| (index, packet.payload.unwrap_or_default().to_vec()))
            })
            .collect()
    }

    #[test]
    fn the_pes_packets_of_every_framing_are_reported_whatever_the_chunk_length() {
        for (stream, unit_length) in [
            (STANDARD_188, 188),
            (TIMESTAMPED_192, 192),
            (REED_SOLOMON_204, 204),
        ] {
            for chunk_length in [1, 7, 188, 1000, stream.len()] {
                let seen = demux_subscribed(
                    &mut subscribed(DemuxerConfig::default()),
                    stream,
                    chunk_length,
                );
                assert_eq!(
                    pes_packets(seen),
                    fixture_pes_packets(unit_length, 0),
                    "{unit_length} bytes in chunks of {chunk_length}"
                );
            }
        }
    }

    #[test]
    fn a_subscription_taken_on_the_program_map_takes_the_pes_packets_after_it() {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        let mut seen = Vec::new();
        let take = |demuxer: &mut Demuxer, seen: &mut Vec<Seen>| {
            while let Some(event) = demuxer.poll_event() {
                let mut pids = Vec::new();
                if let Event::TsProgramMapSection { section, .. } = &event {
                    pids.extend(
                        section
                            .elementary_streams
                            .iter()
                            .map(|stream| stream.unwrap().elementary_pid),
                    );
                }
                seen.push(seen_event(event));
                for pid in pids {
                    demuxer.subscribe_pes(pid);
                }
            }
        };
        demuxer.handle_input(STANDARD_188);
        take(&mut demuxer, &mut seen);
        demuxer.finish();
        take(&mut demuxer, &mut seen);
        assert_eq!(pes_packets(seen), fixture_pes_packets(188, 0));
    }

    #[test]
    fn a_pes_packet_past_the_limit_is_dropped_and_the_next_one_is_reassembled() {
        let mut config = DemuxerConfig::default();
        config.pes_buffer_limit = 2000;
        let seen = demux_subscribed(&mut subscribed(config), STANDARD_188, STANDARD_188.len());
        let mut gathered = 0;
        let discarded_bytes = payloads(STANDARD_188, AUDIO_PID)
            .into_iter()
            .skip_while(|(index, _)| *index < 28)
            .map(|(_, payload)| {
                gathered += payload.len();
                gathered
            })
            .find(|gathered| *gathered > 2000)
            .unwrap();
        assert_eq!(
            (of_pid(seen.clone(), AUDIO_PID), of_pid(seen, VIDEO_PID)),
            (
                vec![
                    Seen::Error(Error::PesBufferOverrun {
                        pid: Pid::try_from(AUDIO_PID).unwrap(),
                        discarded_bytes,
                    }),
                    fixture_pes(47, 188, 0),
                ],
                vec![
                    fixture_pes(3, 188, 0),
                    fixture_pes(15, 188, 0),
                    fixture_pes(23, 188, 0),
                ]
            )
        );
    }

    #[test]
    fn a_continuity_error_inside_a_pes_packet_ends_it_as_truncated_before_the_next() {
        let audio = payloads(STANDARD_188, AUDIO_PID);
        let (start, head) = audio.iter().find(|(index, _)| *index == 28).unwrap();
        let (lost, _) = audio.iter().find(|(index, _)| index > start).unwrap();
        let counter = |index: usize| STANDARD_188[188 * index + 3] & 0x0F;
        let seen = demux_subscribed(
            &mut subscribed(DemuxerConfig::default()),
            &without_packet(*lost),
            188,
        );
        assert_eq!(
            of_pid(seen, AUDIO_PID),
            [
                Seen::Error(Error::Discontinuity {
                    pid: Pid::try_from(AUDIO_PID).unwrap(),
                    expected: ContinuityCounter::try_from(counter(*lost)).unwrap(),
                    found: ContinuityCounter::try_from((counter(*lost) + 1) & 0x0F).unwrap(),
                }),
                Seen::TruncatedPesPacket {
                    pid: AUDIO_PID,
                    bytes: head.clone(),
                    start_offset: 28 * 188,
                },
                Seen::PesPacket {
                    pid: AUDIO_PID,
                    stream_id: 0xC0,
                    pts: Some(175_933),
                    dts: None,
                    payload_length: 1881,
                    start_offset: 46 * 188,
                },
            ]
        );
    }

    #[test]
    fn the_end_of_input_ends_a_pes_packet_of_an_unspecified_length_and_truncates_a_short_one() {
        let stream = &STANDARD_188[..48 * 188];
        let (_, head) = payloads(stream, AUDIO_PID).pop().unwrap();
        let seen = demux_subscribed(&mut subscribed(DemuxerConfig::default()), stream, 1000);
        assert_eq!(
            (of_pid(seen.clone(), AUDIO_PID), of_pid(seen, VIDEO_PID)),
            (
                vec![
                    fixture_pes(28, 188, 0),
                    Seen::TruncatedPesPacket {
                        pid: AUDIO_PID,
                        bytes: head,
                        start_offset: 47 * 188,
                    },
                ],
                vec![
                    fixture_pes(3, 188, 0),
                    fixture_pes(15, 188, 0),
                    fixture_pes(23, 188, 0),
                ]
            )
        );
    }

    #[test]
    fn bytes_before_the_grid_are_counted_into_the_start_offset() {
        let mut stream = vec![0xFF; 500];
        stream.extend_from_slice(STANDARD_188);
        let seen = demux_subscribed(&mut subscribed(DemuxerConfig::default()), &stream, 1000);
        assert_eq!(pes_packets(seen), fixture_pes_packets(188, 500));
    }

    #[test]
    fn start_offsets_go_on_across_an_end_and_a_resume() {
        let (before, after) = STANDARD_188.split_at(25 * 188 + 100);
        let mut demuxer = subscribed(DemuxerConfig::default());
        let mut seen = demux_subscribed(&mut demuxer, before, 1000);
        seen.extend(demux_subscribed(&mut demuxer, after, 1000));
        let start_offsets: Vec<u64> = seen
            .into_iter()
            .filter_map(|seen| {
                if let Seen::PesPacket { start_offset, .. } = seen {
                    Some(start_offset)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            start_offsets,
            [3 * 188, 15 * 188, 23 * 188, 28 * 188, 47 * 188]
        );
    }

    #[test]
    fn a_packet_cut_at_the_end_of_input_keeps_the_start_offset_of_its_unit() {
        // The 192-byte stream ends right after the packet that starts the
        // second video PES packet, whose header takes 19 bytes of its payload
        let (_, payload) = payloads(STANDARD_188, VIDEO_PID)
            .into_iter()
            .find(|(index, _)| *index == 15)
            .unwrap();
        let seen = demux_subscribed(
            &mut subscribed(DemuxerConfig::default()),
            &TIMESTAMPED_192[..16 * 192],
            1000,
        );
        assert_eq!(
            pes_packets(seen),
            [
                fixture_pes(3, 192, 0),
                Seen::PesPacket {
                    pid: VIDEO_PID,
                    stream_id: 0xE0,
                    pts: Some(162_000),
                    dts: Some(144_000),
                    payload_length: payload.len() - 19,
                    start_offset: 15 * 192,
                }
            ]
        );
    }

    fn pid(value: u16) -> Pid {
        Pid::try_from(value).unwrap()
    }

    /// The events of `stream`, handed over in chunks of `chunk_length`, with
    /// `on_program_map` given the demuxer and the elementary PIDs not carried
    /// in sections of each [`Event::TsProgramMapSection`], once taken
    fn demux_reacting(
        config: DemuxerConfig,
        stream: &[u8],
        chunk_length: usize,
        on_program_map: impl Fn(&mut Demuxer, Vec<Pid>),
    ) -> Vec<Seen> {
        let mut demuxer = Demuxer::new(config);
        let mut seen = Vec::new();
        let take = |demuxer: &mut Demuxer, seen: &mut Vec<Seen>| {
            while let Some(event) = demuxer.poll_event() {
                let mut pids = Vec::new();
                if let Event::TsProgramMapSection { section, .. } = &event {
                    pids.extend(
                        section
                            .elementary_streams
                            .iter()
                            .map_while(Result::ok)
                            .filter(|stream| !stream.stream_type.is_carried_in_sections())
                            .map(|stream| stream.elementary_pid),
                    );
                }
                let is_program_map = matches!(event, Event::TsProgramMapSection { .. });
                seen.push(seen_event(event));
                if is_program_map {
                    on_program_map(demuxer, pids);
                }
            }
        };
        for chunk in stream.chunks(chunk_length) {
            demuxer.handle_input(chunk);
            take(&mut demuxer, &mut seen);
        }
        demuxer.finish();
        take(&mut demuxer, &mut seen);
        seen
    }

    fn subscribing(demuxer: &mut Demuxer, pids: Vec<Pid>) {
        for pid in pids {
            demuxer.subscribe_pes(pid);
        }
    }

    fn automatic() -> DemuxerConfig {
        DemuxerConfig::default().with_subscribe_elementary_pids(true)
    }

    /// A stream built packet by packet: null packets enough for the demuxer
    /// to find the packet grid, a PAT of program 1, its PMTs on
    /// `program_map_pid`, and PES packets
    struct Stream {
        writer: PacketWriter,
        program_map_pid: Pid,
        bytes: Vec<u8>,
    }

    impl Stream {
        fn new(program_map_pid: u16) -> Self {
            let mut stream = Self {
                writer: PacketWriter::new(),
                program_map_pid: pid(program_map_pid),
                bytes: Vec::new(),
            };
            for _ in 0..5 {
                stream.bytes.extend([0x47, 0x1f, 0xff, 0x10]);
                stream.bytes.extend([0xff; 184]);
            }
            let mut programs = Vec::new();
            ProgramLoop::write_from(
                [Program::ProgramMap {
                    program_number: NonZeroU16::MIN,
                    program_map_pid: stream.program_map_pid,
                }],
                &mut programs,
            )
            .unwrap();
            let pat = ProgramAssociationSection::new(1, version(0))
                .with_programs(ProgramLoop::from(programs.as_slice()));
            stream
                .writer
                .write_program_association_section(&pat, &mut stream.bytes)
                .unwrap();
            stream
        }

        /// Appends a PMT of `version_number` naming `(stream_type, pid)`s
        fn program_map(mut self, version_number: u8, streams: &[(u8, u16)]) -> Self {
            let mut entries = Vec::new();
            ElementaryStreamLoop::write_from(
                streams.iter().map(|&(stream_type, elementary_pid)| {
                    ElementaryStream::new(StreamType::from(stream_type), pid(elementary_pid))
                }),
                &mut entries,
            )
            .unwrap();
            let pmt = TsProgramMapSection::new(1, version(version_number))
                .with_elementary_streams(ElementaryStreamLoop::from(entries.as_slice()));
            self.writer
                .write_ts_program_map_section(self.program_map_pid, &pmt, &mut self.bytes)
                .unwrap();
            self
        }

        /// Appends a section of `TS_program_map_section`'s `table_id` and
        /// syntax for program 1, version `version_number`, whose body is
        /// `body` as given
        fn raw_program_map(mut self, version_number: u8, body: &[u8]) -> Self {
            let section = Section::new(TableId::TS_PROGRAM_MAP, body)
                .with_syntax(SectionSyntax::new(1, version(version_number), true, 0, 0));
            self.writer
                .write_section(self.program_map_pid, &section, &mut self.bytes)
                .unwrap();
            self
        }

        /// Appends a PES packet of three payload bytes with a PTS on `pid`
        fn pes(mut self, elementary_pid: u16) -> Self {
            let packet = PesPacket::new(StreamId::from(0xe0), &[0xaa; 3]).with_optional_header(
                OptionalHeader::new().with_pts(Timestamp::try_from(90_000).unwrap()),
            );
            self.writer
                .write_pes_packet(pid(elementary_pid), &packet, None, &mut self.bytes)
                .unwrap();
            self
        }
    }

    #[test]
    fn with_the_setting_the_elementary_streams_of_the_program_map_are_reassembled_unasked() {
        let seen = demux_reacting(automatic(), STANDARD_188, STANDARD_188.len(), |_, _| {});

        assert_eq!(pes_packets(seen), fixture_pes_packets(188, 0));
    }

    #[test]
    fn the_setting_gives_the_events_of_subscribing_on_each_program_map_event() {
        for stream in [STANDARD_188, TIMESTAMPED_192, REED_SOLOMON_204] {
            for chunk_length in [1, 188, 1000, stream.len()] {
                assert_eq!(
                    demux_reacting(automatic(), stream, chunk_length, |_, _| {}),
                    demux_reacting(DemuxerConfig::default(), stream, chunk_length, subscribing),
                    "{} bytes in chunks of {chunk_length}",
                    stream.len()
                );
            }
        }
    }

    #[test]
    fn a_stream_type_carried_in_sections_is_not_subscribed_to() {
        let stream = Stream::new(0x1000)
            .program_map(0, &[(0x02, 0x100), (0x05, 0x102)])
            .pes(0x100)
            .pes(0x102)
            .bytes;

        let seen = demux_reacting(automatic(), &stream, stream.len(), |_, _| {});

        assert_eq!(
            (of_pid(seen.clone(), 0x100).len(), of_pid(seen, 0x102)),
            (1, Vec::new())
        );
    }

    #[test]
    fn a_pid_the_next_program_map_leaves_out_stays_subscribed_to() {
        let stream = Stream::new(0x1000)
            .program_map(0, &[(0x02, 0x100)])
            .program_map(1, &[(0x02, 0x101)])
            .pes(0x100)
            .pes(0x101)
            .bytes;

        let seen = demux_reacting(automatic(), &stream, stream.len(), |_, _| {});

        assert_eq!(
            (of_pid(seen.clone(), 0x100).len(), of_pid(seen, 0x101).len()),
            (1, 1)
        );
    }

    #[test]
    fn a_pid_unsubscribed_from_on_the_program_map_event_gives_no_pes_packet() {
        let stream = Stream::new(0x1000)
            .program_map(0, &[(0x02, 0x100), (0x02, 0x101)])
            .pes(0x100)
            .pes(0x101)
            .bytes;

        let seen = demux_reacting(automatic(), &stream, stream.len(), |demuxer, _| {
            demuxer.unsubscribe_pes(pid(0x100));
        });

        assert_eq!(
            (of_pid(seen.clone(), 0x100), of_pid(seen, 0x101).len()),
            (Vec::new(), 1)
        );
    }

    #[test]
    fn an_entry_that_cannot_be_read_ends_the_elementary_pids_taken() {
        // PCR_PID 0x100, no program info, an entry of PID 0x100, then one cut
        // before its ES_info_length naming PID 0x101
        let body = [
            0xe1, 0x00, 0xf0, 0x00, 0x02, 0xe1, 0x00, 0xf0, 0x00, 0x02, 0xe1, 0x01,
        ];
        let stream = Stream::new(0x1000)
            .raw_program_map(0, &body)
            .pes(0x100)
            .pes(0x101)
            .bytes;

        let seen = demux_reacting(automatic(), &stream, stream.len(), |_, _| {});

        assert_eq!(
            (of_pid(seen.clone(), 0x100).len(), of_pid(seen, 0x101)),
            (1, Vec::new())
        );
    }

    #[test]
    fn a_section_that_fails_as_a_program_map_section_subscribes_to_nothing() {
        // A body shorter than PCR_PID and program_info_length
        let stream = Stream::new(0x1000)
            .raw_program_map(0, &[0xe1])
            .pes(0x100)
            .bytes;

        let seen = demux_reacting(automatic(), &stream, stream.len(), |_, _| {});

        assert_eq!(
            (
                seen.iter()
                    .any(|seen| matches!(seen, Seen::Error(Error::TsProgramMapSection { .. }))),
                pes_packets(seen)
            ),
            (true, Vec::new())
        );
    }

    #[test]
    fn a_pid_unsubscribed_from_is_subscribed_to_again_by_the_next_version_naming_it() {
        let stream = Stream::new(0x1000)
            .program_map(0, &[(0x02, 0x100)])
            .pes(0x100)
            .program_map(1, &[(0x02, 0x100)])
            .pes(0x100)
            .bytes;
        let unsubscribed_once = core::cell::Cell::new(false);

        let seen = demux_reacting(automatic(), &stream, stream.len(), |demuxer, _| {
            if !unsubscribed_once.replace(true) {
                demuxer.unsubscribe_pes(pid(0x100));
            }
        });

        assert_eq!(of_pid(seen, 0x100).len(), 1);
    }
}
