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
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Error, Event};
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
}
