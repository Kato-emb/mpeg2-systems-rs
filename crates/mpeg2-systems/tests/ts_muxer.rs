//! A Transport Stream the `Muxer` writes of a video and an audio stream,
//! read back by the Transport Stream demuxer

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
#[path = "support/mux.rs"]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "the time stamps and lengths are small constants"
)]
mod mux_support;

#[cfg(test)]
#[cfg(feature = "alloc")]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use mpeg2_systems::time::Timestamp;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
    use mpeg2_systems::ts::mux::MuxerConfig;

    use crate::mux_support::{AUDIO_PID, Input, PMT_PID, VIDEO_PID, inputs, multiplex, pid};
    use crate::support::{Seen, pat, seen_event, version};

    /// The `mux_delay` of the tests, 0.7 s
    const MUX_DELAY: Duration = Duration::from_millis(700);

    /// What the demuxer reads back: the tables, the PES packets of each PID,
    /// and every other event but `SyncAcquired` and the PCRs
    type Read = (Vec<Seen>, BTreeMap<u16, Vec<Input>>, Vec<Seen>);

    fn read(stream: &[u8]) -> Read {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.subscribe_pes(pid(VIDEO_PID));
        demuxer.subscribe_pes(pid(AUDIO_PID));
        demuxer.handle_input(stream);
        demuxer.finish();
        let (mut tables, mut pes_packets, mut others) = (Vec::new(), BTreeMap::new(), Vec::new());
        while let Some(event) = demuxer.poll_event() {
            if let Event::PesPacket { pid, packet, .. } = &event {
                let header = packet.optional_header.as_ref();
                pes_packets
                    .entry(pid.get())
                    .or_insert_with(Vec::new)
                    .push(Input {
                        pid: pid.get(),
                        stream_id: packet.stream_id.get(),
                        pts: header
                            .and_then(|header| header.pts)
                            .map_or(0, Timestamp::get),
                        dts: header.and_then(|header| header.dts).map(Timestamp::get),
                        payload: packet.payload.to_vec(),
                    });
                continue;
            }
            let seen = seen_event(event);
            if matches!(
                seen,
                Seen::ProgramAssociationSection { .. } | Seen::TsProgramMapSection { .. }
            ) {
                tables.push(seen);
            } else if !matches!(
                seen,
                Seen::SyncAcquired(_) | Seen::ProgramClockReference { .. }
            ) {
                others.push(seen);
            }
        }
        (tables, pes_packets, others)
    }

    #[test]
    fn the_program_registered_and_the_pes_packets_given_read_back_as_given() {
        let stream = multiplex(MuxerConfig::new(MUX_DELAY));

        let mut given = BTreeMap::new();
        for input in inputs() {
            given.entry(input.pid).or_insert_with(Vec::new).push(input);
        }
        let pmt = Seen::TsProgramMapSection {
            pid: PMT_PID,
            program_number: 1,
            version_number: version(0),
            current_next_indicator: true,
            section_number: 0,
            last_section_number: 0,
            pcr_pid: Some(VIDEO_PID),
            program_info: Vec::new(),
            elementary_streams: vec![(0x02, VIDEO_PID, Vec::new()), (0x04, AUDIO_PID, Vec::new())],
        };
        assert_eq!(read(&stream), (vec![pat(), pmt], given, Vec::new()));
    }

    #[test]
    fn the_same_input_gives_the_same_stream() {
        assert_eq!(
            multiplex(MuxerConfig::new(MUX_DELAY)),
            multiplex(MuxerConfig::new(MUX_DELAY))
        );
    }
}
