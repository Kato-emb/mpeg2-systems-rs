//! The program association and program map sections of a stream FFmpeg
//! wrote, converted from their sections and written back

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use core::num::NonZeroU16;

    #[cfg(feature = "alloc")]
    use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection};
    #[cfg(feature = "alloc")]
    use mpeg2_systems::psi::pmt::{ElementaryStream, TsProgramMapSection};
    #[cfg(feature = "alloc")]
    use mpeg2_systems::psi::section::Section;
    #[cfg(feature = "alloc")]
    use mpeg2_systems::stream_type::StreamType;
    #[cfg(feature = "alloc")]
    use mpeg2_systems::ts::Pid;
    #[cfg(feature = "alloc")]
    use mpeg2_systems::ts::packet::TransportPacket;

    /// A Transport Stream of one program, its PMT on PID 0x1000, written by
    /// FFmpeg 6.1.1
    #[cfg(feature = "alloc")]
    const STREAM: &[u8] = include_bytes!("fixtures/188.ts");

    #[cfg(feature = "alloc")]
    fn pid(value: u16) -> Pid {
        Pid::try_from(value).unwrap()
    }

    /// Returns the first section on `pid`: the payload of its first packet
    /// with `payload_unit_start_indicator` set, past the `pointer_field`, cut
    /// at the end `section_length` gives
    #[cfg(feature = "alloc")]
    fn first_section_on(pid: Pid) -> Vec<u8> {
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
    fn the_pat_ffmpeg_wrote_maps_its_program_to_the_pid_of_its_pmt() {
        let bytes = first_section_on(Pid::PAT);
        let section = Section::read(&bytes).unwrap();
        let pat = ProgramAssociationSection::try_from(&section).unwrap();
        let mut written = Vec::new();
        pat.write(&mut written).unwrap();

        assert_eq!(
            (
                pat.transport_stream_id,
                pat.programs.iter().collect::<Result<Vec<_>, _>>(),
                written.as_slice()
            ),
            (
                1,
                Ok(vec![Program::ProgramMap {
                    program_number: NonZeroU16::MIN,
                    program_map_pid: pid(0x1000)
                }]),
                bytes.as_slice()
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_pmt_ffmpeg_wrote_lists_its_video_and_audio_and_the_pid_of_its_pcr() {
        let bytes = first_section_on(pid(0x1000));
        let section = Section::read(&bytes).unwrap();
        let pmt = TsProgramMapSection::try_from(&section).unwrap();
        let mut written = Vec::new();
        pmt.write(&mut written).unwrap();

        assert_eq!(
            (
                pmt.program_number,
                pmt.pcr_pid,
                pmt.elementary_streams.iter().collect::<Result<Vec<_>, _>>(),
                written.as_slice()
            ),
            (
                1,
                Some(pid(0x0100)),
                Ok(vec![
                    ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(0x0100)),
                    ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, pid(0x0101)),
                ]),
                bytes.as_slice()
            )
        );
    }
}
