//! PES packets of a stream FFmpeg wrote, read and written back

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use mpeg2_systems::pes::PesPacket;
    #[cfg(feature = "alloc")]
    use mpeg2_systems::ts::Pid;
    #[cfg(feature = "alloc")]
    use mpeg2_systems::ts::packet::TransportPacket;

    /// A Transport Stream of MPEG-2 video on PID 0x100 and MPEG-1 Layer II
    /// audio on PID 0x101, written by FFmpeg 6.1.1
    #[cfg(feature = "alloc")]
    const STREAM: &[u8] = include_bytes!("fixtures/188.ts");

    /// Returns the PES packets of `pid`, each the payloads from one packet
    /// with `payload_unit_start_indicator` set up to the next
    #[cfg(feature = "alloc")]
    fn pes_packets_of(pid: u16) -> Vec<Vec<u8>> {
        let pid = Pid::try_from(pid).unwrap();
        let mut packets: Vec<Vec<u8>> = Vec::new();
        for bytes in STREAM.as_chunks::<188>().0 {
            let packet = TransportPacket::read(bytes).unwrap();
            if packet.pid != pid {
                continue;
            }
            if packet.payload_unit_start_indicator {
                packets.push(Vec::new());
            }
            if let (Some(pes_packet), Some(payload)) = (packets.last_mut(), packet.payload) {
                pes_packet.extend_from_slice(payload);
            }
        }
        packets
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn the_time_stamps_of_each_pes_packet_are_those_ffmpeg_wrote() {
        let time_stamps_of = |pid| {
            pes_packets_of(pid)
                .iter()
                .map(|bytes| {
                    let packet = PesPacket::read(bytes).unwrap();
                    let header = packet.optional_header.unwrap();
                    (
                        packet.stream_id.get(),
                        header.pts.map(|pts| pts.get()),
                        header.dts.map(|dts| dts.get()),
                    )
                })
                .collect::<Vec<_>>()
        };

        assert_eq!(
            (time_stamps_of(0x100), time_stamps_of(0x101)),
            (
                vec![
                    (0xe0, Some(144_000), Some(126_000)),
                    (0xe0, Some(162_000), Some(144_000)),
                    (0xe0, Some(180_000), Some(162_000)),
                ],
                vec![(0xc0, Some(143_018), None), (0xc0, Some(175_933), None)],
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn every_pes_packet_of_a_stream_ffmpeg_wrote_is_written_back_byte_for_byte() {
        let packets: Vec<Vec<u8>> = [pes_packets_of(0x100), pes_packets_of(0x101)].concat();
        let written: Vec<Vec<u8>> = packets
            .iter()
            .map(|bytes| {
                let mut written = Vec::new();
                PesPacket::read(bytes).unwrap().write(&mut written).unwrap();
                written
            })
            .collect();

        assert_eq!(written, packets);
    }
}
