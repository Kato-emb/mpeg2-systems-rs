//! Sections of a stream FFmpeg wrote, read and written back

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use mpeg2_systems::psi::section::{Section, SectionSyntax};
    #[cfg(feature = "alloc")]
    use mpeg2_systems::psi::{TableId, VersionNumber};
    #[cfg(feature = "alloc")]
    use mpeg2_systems::ts::Pid;
    #[cfg(feature = "alloc")]
    use mpeg2_systems::ts::packet::TransportPacket;

    /// A Transport Stream of one program, its PMT on PID 0x1000, written by
    /// FFmpeg 6.1.1
    #[cfg(feature = "alloc")]
    const STREAM: &[u8] = include_bytes!("fixtures/188.ts");

    /// Returns the first section on `pid`: the payload of its first packet
    /// with `payload_unit_start_indicator` set, past the `pointer_field`, cut
    /// at the end `section_length` gives
    #[cfg(feature = "alloc")]
    fn first_section_on(pid: u16) -> Vec<u8> {
        let pid = Pid::try_from(pid).unwrap();
        let payload = STREAM
            .as_chunks::<188>()
            .0
            .iter()
            .map(|bytes| TransportPacket::read(bytes).unwrap())
            .find(|packet| packet.pid == pid && packet.payload_unit_start_indicator)
            .and_then(|packet| packet.payload)
            .unwrap();
        let (pointer_field, rest) = payload.split_first().unwrap();
        let section = rest.get(usize::from(*pointer_field)..).unwrap();
        let [_, high, low] = *section.first_chunk::<3>().unwrap();
        let section_length = usize::from(u16::from_be_bytes([high & 0x0f, low]));
        section.get(..section_length + 3).unwrap().to_vec()
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_pat_ffmpeg_wrote_names_its_transport_stream_and_version() {
        let bytes = first_section_on(0x0000);
        let section = Section::read(&bytes).unwrap();

        assert_eq!(
            (section.table_id, section.syntax),
            (
                TableId::PROGRAM_ASSOCIATION,
                Some(SectionSyntax::new(
                    1,
                    VersionNumber::try_from(0).unwrap(),
                    true,
                    0,
                    0
                ))
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_pat_and_pmt_ffmpeg_wrote_are_written_back_byte_for_byte() {
        let sections = [first_section_on(0x0000), first_section_on(0x1000)];
        let written = sections.clone().map(|bytes| {
            let mut written = Vec::new();
            Section::read(&bytes).unwrap().write(&mut written).unwrap();
            written
        });

        assert_eq!(written, sections);
    }
}
