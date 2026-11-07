//! The program association and program map sections of a stream FFmpeg
//! wrote, converted from their sections and written back, and the typed
//! descriptors of a hand-written program map section

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use core::num::NonZeroU16;

    use mpeg2_systems::descriptor::maximum_bitrate::MaximumBitrateDescriptor;
    use mpeg2_systems::descriptor::smoothing_buffer::SmoothingBufferDescriptor;
    #[cfg(feature = "alloc")]
    use mpeg2_systems::psi::pat::{Program, ProgramAssociationSection};
    #[cfg(feature = "alloc")]
    use mpeg2_systems::psi::pmt::ElementaryStream;
    use mpeg2_systems::psi::pmt::TsProgramMapSection;
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

    /// A PMT whose video stream carries a `maximum_bitrate_descriptor` and a
    /// `smoothing_buffer_descriptor`, its `CRC_32` computed outside this
    /// crate
    const PMT_WITH_ES_INFO: [u8; 34] = [
        0x02, 0xb0, 0x1f, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xe1, 0x00, 0xf0, 0x00, 0x02, 0xe1, 0x00,
        0xf0, 0x0d, 0x0e, 0x03, 0xc0, 0x9c, 0x40, 0x10, 0x06, 0xc0, 0x92, 0x7c, 0xc0, 0x05, 0xe0,
        0xd7, 0x8d, 0x80, 0x52,
    ];

    #[test]
    fn a_descriptor_of_the_es_info_of_a_pmt_converts_to_its_type() {
        let section = Section::read(&PMT_WITH_ES_INFO).unwrap();
        let pmt = TsProgramMapSection::try_from(&section).unwrap();
        let stream = pmt.elementary_streams.iter().next().unwrap().unwrap();
        let mut es_info = stream.es_info.iter();

        assert_eq!(
            (
                es_info
                    .next()
                    .map(|raw| raw.and_then(|raw| MaximumBitrateDescriptor::try_from(&raw))),
                es_info
                    .next()
                    .map(|raw| raw.and_then(|raw| SmoothingBufferDescriptor::try_from(&raw))),
                es_info.next()
            ),
            (
                Some(Ok(MaximumBitrateDescriptor::new(40000))),
                Some(Ok(SmoothingBufferDescriptor::new(37500, 1504))),
                None
            )
        );
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
    fn the_pmt_ffmpeg_wrote_lists_its_video_and_audio_without_descriptors() {
        let bytes = first_section_on(pid(0x1000));
        let section = Section::read(&bytes).unwrap();
        let pmt = TsProgramMapSection::try_from(&section).unwrap();
        let mut written = Vec::new();
        pmt.write(&mut written).unwrap();

        assert_eq!(
            (
                pmt.program_number,
                pmt.pcr_pid,
                pmt.program_info.is_empty(),
                pmt.elementary_streams.iter().collect::<Result<Vec<_>, _>>(),
                written.as_slice()
            ),
            (
                1,
                Some(pid(0x0100)),
                true,
                Ok(vec![
                    ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, pid(0x0100)),
                    ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, pid(0x0101)),
                ]),
                bytes.as_slice()
            )
        );
    }
}
