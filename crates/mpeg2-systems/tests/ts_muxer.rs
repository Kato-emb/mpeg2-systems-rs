//! A Transport Stream written with the crate's muxer, read back by the
//! Transport Stream demuxer

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
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    use mpeg2_systems::psi::VersionNumber;
    use mpeg2_systems::psi::pat::{ProgramAssociationSection, ProgramLoop};
    use mpeg2_systems::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
    use mpeg2_systems::psi::section::Section;
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::{ClockReference, Timestamp};
    use mpeg2_systems::ts::Pid;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
    use mpeg2_systems::ts::mux::Muxer;
    use mpeg2_systems::ts::packet::TransportPacket;

    use crate::support::{STANDARD_188, Seen, pat, pmt, program, seen_event};

    const PMT_PID: u16 = 0x1000;
    const VIDEO_PID: u16 = 0x0100;
    const AUDIO_PID: u16 = 0x0101;

    /// A PES packet as the demuxer gives it back: its `stream_id`, PTS, DTS
    /// and payload
    type ReadPesPacket = (u8, Option<u64>, Option<u64>, Vec<u8>);

    /// What the demuxer reads from a stream, by kind
    #[derive(Debug, Default, PartialEq, Eq)]
    struct Read {
        tables: Vec<Seen>,
        program_clock_references: Vec<Seen>,
        /// The PES packets of each PID, in the order of the PID
        // Why not one list across PIDs: the demuxer gives a PES packet when
        // it ends, so the order across PIDs follows how the Transport
        // Packets of the PIDs interleave, which a muxer is free to choose.
        pes_packets: BTreeMap<u16, Vec<ReadPesPacket>>,
        /// Every other event but `SyncAcquired`: errors, discontinuities and
        /// PES packets that could not be read
        others: Vec<Seen>,
    }

    fn pid(value: u16) -> Pid {
        Pid::try_from(value).unwrap()
    }

    /// `length` bytes counting up, so bytes out of place differ
    fn counting(length: usize) -> Vec<u8> {
        (0..length)
            .map(|index| u8::try_from(index % 251).unwrap())
            .collect()
    }

    fn read(stream: &[u8]) -> Read {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.subscribe_pes(pid(VIDEO_PID));
        demuxer.subscribe_pes(pid(AUDIO_PID));
        demuxer.handle_input(stream);
        demuxer.finish();
        let mut read = Read::default();
        while let Some(event) = demuxer.poll_event() {
            if let Event::PesPacket { pid, packet, .. } = &event {
                let header = packet.optional_header.as_ref();
                read.pes_packets.entry(pid.get()).or_default().push((
                    packet.stream_id.get(),
                    header.and_then(|header| header.pts).map(Timestamp::get),
                    header.and_then(|header| header.dts).map(Timestamp::get),
                    packet.payload.to_vec(),
                ));
                continue;
            }
            let seen = seen_event(event);
            if matches!(seen, Seen::SyncAcquired(_)) {
                continue;
            }
            if matches!(
                seen,
                Seen::ProgramAssociationSection { .. } | Seen::TsProgramMapSection { .. }
            ) {
                read.tables.push(seen);
            } else if matches!(seen, Seen::ProgramClockReference { .. }) {
                read.program_clock_references.push(seen);
            } else {
                read.others.push(seen);
            }
        }
        read
    }

    /// The section `write` writes, as a [`Section`] over `bytes`
    fn section<E: core::fmt::Debug>(
        bytes: &mut Vec<u8>,
        write: impl FnOnce(&mut Vec<u8>) -> Result<(), E>,
    ) -> Section<'_> {
        write(bytes).unwrap();
        Section::read(bytes).unwrap()
    }

    fn pes_packet<'payload>(
        stream_id: u8,
        pts: u64,
        dts: Option<u64>,
        payload: &'payload [u8],
    ) -> PesPacket<'payload> {
        let mut header = OptionalHeader::new();
        header.pts = Some(Timestamp::try_from(pts).unwrap());
        header.dts = dts.map(|dts| Timestamp::try_from(dts).unwrap());
        let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
        packet.optional_header = Some(header);
        packet
    }

    #[test]
    fn a_written_program_reads_back_with_its_tables_pcrs_and_pes_packets() {
        let mut muxer = Muxer::new();
        let mut stream = Vec::new();
        let mut programs = Vec::new();
        ProgramLoop::write_from([program(1, PMT_PID)], &mut programs).unwrap();
        let mut pat_section =
            ProgramAssociationSection::new(1, VersionNumber::try_from(0).unwrap());
        pat_section.programs = ProgramLoop::from(&programs[..]);
        let mut streams = Vec::new();
        ElementaryStreamLoop::write_from(
            [
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(VIDEO_PID)),
                ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, pid(AUDIO_PID)),
            ],
            &mut streams,
        )
        .unwrap();
        let mut pmt_section = TsProgramMapSection::new(1, VersionNumber::try_from(0).unwrap());
        pmt_section.pcr_pid = Some(pid(VIDEO_PID));
        pmt_section.elementary_streams = ElementaryStreamLoop::from(&streams[..]);
        let mut bytes = Vec::new();
        muxer
            .write_section(
                Pid::PAT,
                &section(&mut bytes, |out| pat_section.write(out)),
                &mut stream,
            )
            .unwrap();
        let mut bytes = Vec::new();
        muxer
            .write_section(
                pid(PMT_PID),
                &section(&mut bytes, |out| pmt_section.write(out)),
                &mut stream,
            )
            .unwrap();

        // Video: one access unit longer than PES_packet_length can state
        let video = [counting(1_000), counting(70_000), counting(50)];
        let audio = [counting(500), counting(3_000)];
        let pcrs = [
            ClockReference::new(63_000, 0).unwrap(),
            ClockReference::new(66_600, 150).unwrap(),
            ClockReference::new(70_200, 299).unwrap(),
        ];
        for (index, (payload, pcr)) in video.iter().zip(pcrs).enumerate() {
            let pts = 90_000 + 3_600 * u64::try_from(index).unwrap();
            let mut packet = pes_packet(0xE0, pts, Some(pts - 3_600), payload);
            packet.length_unspecified = true;
            muxer
                .write_pes_packet(pid(VIDEO_PID), &packet, Some(pcr), &mut stream)
                .unwrap();
            if let Some(payload) = audio.get(index) {
                let packet = pes_packet(0xC0, pts + 100, None, payload);
                muxer
                    .write_pes_packet(pid(AUDIO_PID), &packet, None, &mut stream)
                    .unwrap();
            }
        }

        assert!(
            stream
                .chunks(188)
                .all(|packet| TransportPacket::read(packet).is_ok()),
            "a packet is not 188 bytes, or does not read"
        );
        let read = read(&stream);
        let video_packets = video.iter().enumerate().map(|(index, payload)| {
            let pts = 90_000 + 3_600 * u64::try_from(index).unwrap();
            (0xE0, Some(pts), Some(pts - 3_600), payload.clone())
        });
        let audio_packets = audio.iter().enumerate().map(|(index, payload)| {
            let pts = 90_000 + 3_600 * u64::try_from(index).unwrap() + 100;
            (0xC0, Some(pts), None, payload.clone())
        });
        assert_eq!(
            read,
            Read {
                tables: vec![pat(), pmt()],
                program_clock_references: pcrs
                    .iter()
                    .map(|&program_clock_reference| Seen::ProgramClockReference {
                        pid: VIDEO_PID,
                        program_clock_reference,
                    })
                    .collect(),
                pes_packets: BTreeMap::from([
                    (VIDEO_PID, video_packets.collect()),
                    (AUDIO_PID, audio_packets.collect()),
                ]),
                others: Vec::new(),
            }
        );
    }

    #[test]
    fn the_fixture_written_back_from_what_it_reads_reads_as_the_fixture() {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        demuxer.subscribe_pes(pid(VIDEO_PID));
        demuxer.subscribe_pes(pid(AUDIO_PID));
        demuxer.handle_input(STANDARD_188);
        demuxer.finish();
        let mut muxer = Muxer::new();
        let mut stream = Vec::new();
        // Why queue the PCRs rather than keep the last: the demuxer gives a
        // PES packet when it ends, after the PCRs of the packets it spans,
        // and each goes back on a video PES packet of its own.
        let mut pcrs = VecDeque::new();
        while let Some(event) = demuxer.poll_event() {
            let mut bytes = Vec::new();
            if let Event::ProgramAssociationSection(pat) = &event {
                let section = section(&mut bytes, |out| pat.write(out));
                muxer
                    .write_section(Pid::PAT, &section, &mut stream)
                    .unwrap();
            } else if let Event::TsProgramMapSection { pid, section: pmt } = &event {
                let section = section(&mut bytes, |out| pmt.write(out));
                muxer.write_section(*pid, &section, &mut stream).unwrap();
            } else if let Event::ProgramClockReference {
                program_clock_reference,
                ..
            } = &event
            {
                pcrs.push_back(*program_clock_reference);
            } else if let Event::PesPacket { pid, packet, .. } = &event {
                let pcr = (pid.get() == VIDEO_PID).then(|| pcrs.pop_front()).flatten();
                muxer
                    .write_pes_packet(*pid, packet, pcr, &mut stream)
                    .unwrap();
            }
        }

        assert_eq!(read(&stream), read(STANDARD_188));
    }
}
