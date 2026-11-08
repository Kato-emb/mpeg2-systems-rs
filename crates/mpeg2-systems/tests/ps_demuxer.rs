//! The Program Stream demuxer over a stream FFmpeg wrote, whole, behind
//! garbage, cut short, followed by another, and resumed after an end

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    reason = "streams are cut at constant offsets well inside their length"
)]
mod tests {
    use mpeg2_systems::ps::MPEG_PROGRAM_END_CODE;
    use mpeg2_systems::ps::demux::{Demuxer, Error, Event};

    /// A Program Stream FFmpeg wrote, carried over from the previous
    /// implementation
    const PROGRAM: &[u8] = include_bytes!("fixtures/program.ps");

    /// An [`Event`] by what identifies it: a pack header by its SCR base, a
    /// PES packet by its `stream_id` and payload length, lent bytes by their
    /// length
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Seen {
        SyncAcquired,
        PackHeader(u64),
        SystemHeader,
        ProgramStreamMap,
        PesPacket(u8, usize),
        ProgramEnd,
        Unsynchronized(usize),
        Unframable(usize),
        Truncated(usize),
        Error(Error),
        /// An event this test does not expect, so the comparison fails on it
        Unexpected(String),
    }

    fn seen_event(event: Event<'_>) -> Seen {
        match event {
            Event::SyncAcquired => Seen::SyncAcquired,
            Event::PackHeader(header) => Seen::PackHeader(header.system_clock_reference.base()),
            Event::SystemHeader(_) => Seen::SystemHeader,
            Event::ProgramStreamMap(_) => Seen::ProgramStreamMap,
            Event::PesPacket(packet) => {
                Seen::PesPacket(packet.stream_id.get(), packet.payload.len())
            }
            Event::ProgramEnd => Seen::ProgramEnd,
            Event::Unsynchronized { bytes } => Seen::Unsynchronized(bytes.len()),
            Event::Unframable { bytes } => Seen::Unframable(bytes.len()),
            Event::Truncated { bytes } => Seen::Truncated(bytes.len()),
            Event::Error(error) => Seen::Error(error),
            other => Seen::Unexpected(format!("{other:?}")),
        }
    }

    fn drain(demuxer: &mut Demuxer, seen: &mut Vec<Seen>) {
        while let Some(event) = demuxer.poll_event() {
            seen.push(seen_event(event));
        }
    }

    /// Hands `stream` to `demuxer` in chunks of `chunk_length`, then ends it
    fn demux_into(demuxer: &mut Demuxer, stream: &[u8], chunk_length: usize) -> Vec<Seen> {
        let mut seen = Vec::new();
        for chunk in stream.chunks(chunk_length) {
            demuxer.handle_input(chunk);
            drain(demuxer, &mut seen);
        }
        demuxer.finish();
        drain(demuxer, &mut seen);
        seen
    }

    fn demux(stream: &[u8], chunk_length: usize) -> Vec<Seen> {
        demux_into(&mut Demuxer::new(), stream, chunk_length)
    }

    /// The events of the fixture after `SyncAcquired`, as the previous
    /// implementation reported them
    fn fixture_events() -> Vec<Seen> {
        vec![
            Seen::PackHeader(0),
            Seen::SystemHeader,
            Seen::PesPacket(0xE0, 1993),
            Seen::PackHeader(3),
            Seen::PesPacket(0xC0, 2016),
            Seen::PackHeader(6),
            Seen::PesPacket(0xE0, 1576),
            Seen::PesPacket(0xBE, 432),
            Seen::PackHeader(45_001),
            Seen::PesPacket(0xC0, 2019),
            Seen::PackHeader(83_179),
            Seen::PesPacket(0xC0, 771),
            Seen::PesPacket(0xBE, 1242),
        ]
    }

    fn acquired_then(rest: Vec<Seen>) -> Vec<Seen> {
        let mut seen = vec![Seen::SyncAcquired];
        seen.extend(rest);
        seen
    }

    #[test]
    fn the_packs_system_header_and_pes_packets_are_reported_whatever_the_chunk_length() {
        for chunk_length in [1, 7, 1000, PROGRAM.len()] {
            assert_eq!(
                demux(PROGRAM, chunk_length),
                acquired_then(fixture_events()),
                "chunks of {chunk_length}"
            );
        }
    }

    #[test]
    fn garbage_before_the_stream_is_reported_before_sync() {
        let mut stream = vec![0xFF; 500];
        stream.extend_from_slice(PROGRAM);
        let mut expected = vec![Seen::Unsynchronized(500)];
        expected.extend(acquired_then(fixture_events()));
        assert_eq!(demux(&stream, 1000), expected);
    }

    #[test]
    fn a_stream_that_starts_inside_a_pack_synchronizes_on_the_next_pack_header() {
        let mut expected = vec![Seen::Unsynchronized(1096)];
        expected.extend(acquired_then(fixture_events().split_off(5)));
        assert_eq!(demux(&PROGRAM[3000..], 1000), expected);
    }

    #[test]
    fn a_stream_cut_inside_its_last_pes_packet_reports_the_bytes_left_as_truncated() {
        let mut expected = acquired_then(fixture_events());
        expected.pop();
        expected.push(Seen::Truncated(548));
        assert_eq!(demux(&PROGRAM[..PROGRAM.len() - 700], 1000), expected);
    }

    #[test]
    fn a_stream_after_a_program_end_synchronizes_again_and_reports_its_system_header() {
        let stream = [PROGRAM, &MPEG_PROGRAM_END_CODE, PROGRAM].concat();
        let mut expected = acquired_then(fixture_events());
        expected.push(Seen::ProgramEnd);
        expected.extend(acquired_then(fixture_events()));
        assert_eq!(demux(&stream, 1000), expected);
    }

    #[test]
    fn a_demuxer_resumed_after_an_end_stays_synchronized() {
        let second_pack = PROGRAM
            .windows(4)
            .skip(1)
            .position(|window| window == [0x00, 0x00, 0x01, 0xBA])
            .unwrap()
            + 1;
        let (before, after) = PROGRAM.split_at(second_pack);
        let mut demuxer = Demuxer::new();
        let mut seen = demux_into(&mut demuxer, before, 1000);
        seen.extend(demux_into(&mut demuxer, after, 1000));
        assert_eq!(seen, acquired_then(fixture_events()));
    }
}
