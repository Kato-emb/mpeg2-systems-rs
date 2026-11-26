//! A Program Stream the `Muxer` writes of a video and an audio stream, read
//! back by the Program Stream demuxer and cut into packs by hand, its SCRs
//! checked against ISO/IEC 13818-1 §2.5.2.2 equation 2-21 and §2.7.1

#[cfg(test)]
#[cfg(feature = "alloc")]
#[expect(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "the stream is cut at offsets its own lengths give, and the times are small"
)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use mpeg2_systems::pes::extension::PStdBuffer;
    use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    use mpeg2_systems::ps::PackHeader;
    use mpeg2_systems::ps::demux::{Demuxer, Event};
    use mpeg2_systems::ps::mux::{Muxer, MuxerConfig};
    use mpeg2_systems::ps::psm::ElementaryStream;
    use mpeg2_systems::stream_type::StreamType;
    use mpeg2_systems::time::{ClockReference, Timestamp};

    /// The `mux_delay` of the tests, 0.7 s
    const MUX_DELAY: Duration = Duration::from_millis(700);

    const VIDEO: u8 = 0xe0;
    const AUDIO: u8 = 0xc0;
    const VIDEO_BUFFER: PStdBuffer = PStdBuffer::new(true, 224);
    /// 16 KiB, room for the audio `MUX_DELAY` sends ahead, so
    /// that the SCRs follow the time stamps and the bytes alone
    const AUDIO_BUFFER: PStdBuffer = PStdBuffer::new(false, 128);

    /// 1 260 000 bytes a second
    const RATE: u32 = 25_200;

    /// The 27 MHz ticks of `MUX_DELAY`
    const MUX_DELAY_TICKS: u64 = 18_900_000;

    /// The 27 MHz ticks of 0.7 s, the longest ISO/IEC 13818-1 §2.7.1 allows
    /// from one SCR to the next
    const LONGEST_SCR_INTERVAL: u64 = 18_900_000;

    /// A PES packet to give: its `stream_id`, PTS, DTS and payload
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Input {
        stream_id: u8,
        pts: u64,
        dts: Option<u64>,
        payload: Vec<u8>,
    }

    impl Input {
        fn pes_packet(&self) -> PesPacket<'_> {
            let mut header = OptionalHeader::new();
            header.pts = Some(Timestamp::try_from(self.pts).unwrap());
            header.dts = self.dts.map(|dts| Timestamp::try_from(dts).unwrap());
            let mut packet = PesPacket::new(StreamId::from(self.stream_id), &self.payload);
            packet.optional_header = Some(header);
            packet
        }

        /// The time stamp the muxer follows: the DTS, or the PTS
        fn timestamp(&self) -> u64 {
            self.dts.unwrap_or(self.pts)
        }
    }

    /// Two seconds of video at 25 frames a second, a frame every 3600 ticks
    /// of 90 kHz with its PTS a frame after its DTS, and of audio every 2160
    /// ticks, merged in the order of their DTS (their PTS for the audio), the
    /// first a second in
    fn inputs() -> Vec<Input> {
        let video = (0_u64..50).map(|index| Input {
            stream_id: VIDEO,
            pts: 90_000 + (index + 1) * 3_600,
            dts: Some(90_000 + index * 3_600),
            payload: (0..if index % 25 == 0 { 20_000 } else { 3_000 })
                .map(|byte: u64| u8::try_from((byte + index) % 251).unwrap())
                .collect(),
        });
        let audio = (0_u64..84).map(|index| Input {
            stream_id: AUDIO,
            pts: 90_000 + index * 2_160,
            dts: None,
            payload: vec![u8::try_from(index % 251).unwrap(); 400],
        });
        let mut inputs: Vec<Input> = video.chain(audio).collect();
        inputs.sort_by_key(Input::timestamp);
        inputs
    }

    /// Video and audio whose DTS and PTS are the same, every 20 ms
    /// `first_dts` on, round the 33-bit circle, with a leap of `leap` 90 kHz
    /// ticks after the 50th
    fn stepped(first_dts: u64, leap: u64) -> Vec<Input> {
        (0_u64..100)
            .map(|index| {
                let leaped = if index >= 50 { leap } else { 0 };
                let dts = (first_dts + index * 1_800 + leaped) % Timestamp::MODULUS;
                Input {
                    stream_id: if index % 2 == 0 { VIDEO } else { AUDIO },
                    pts: dts,
                    dts: Some(dts),
                    payload: vec![u8::try_from(index % 251).unwrap(); 2_500],
                }
            })
            .collect()
    }

    /// What the muxer of `config`, with the video and the audio stream
    /// registered, writes of `inputs`
    fn multiplex(config: MuxerConfig, inputs: &[Input]) -> Vec<u8> {
        let mut muxer = Muxer::new(config).unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                VIDEO_BUFFER,
            )
            .unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
                AUDIO_BUFFER,
            )
            .unwrap();
        let mut stream = Vec::new();
        for input in inputs {
            muxer.handle_pes_packet(&input.pes_packet()).unwrap();
            muxer.poll_output(&mut stream);
        }
        muxer.finish();
        muxer.poll_output(&mut stream);
        stream
    }

    /// A pack cut by hand: its SCR in 27 MHz ticks, its bytes, the start
    /// codes of what follows its pack header, and the DTS (or the PTS) of
    /// its PES packet
    #[derive(Debug)]
    struct Pack {
        scr: u64,
        length: usize,
        start_codes: Vec<u8>,
        timestamp: Option<u64>,
    }

    /// The packs of `stream`, cut by the start code and the length of each
    /// element, and whether `MPEG_program_end_code` ends it
    fn packs(stream: &[u8]) -> (Vec<Pack>, bool) {
        let mut packs: Vec<Pack> = Vec::new();
        let mut rest = stream;
        loop {
            match rest {
                [0x00, 0x00, 0x01, 0xba, ..] => {
                    let length = 14 + usize::from(rest[13] & 0x07);
                    let header = PackHeader::read(&rest[..length]).unwrap();
                    packs.push(Pack {
                        scr: header.system_clock_reference.ticks(),
                        length,
                        start_codes: Vec::new(),
                        timestamp: None,
                    });
                    rest = &rest[length..];
                }
                [0x00, 0x00, 0x01, 0xb9] => return (packs, true),
                [0x00, 0x00, 0x01, code, high, low, ..] => {
                    let length = 6 + usize::from(u16::from_be_bytes([*high, *low]));
                    let pack = packs.last_mut().unwrap();
                    pack.length += length;
                    pack.start_codes.push(*code);
                    if *code >= 0xbd {
                        let header = PesPacket::read(&rest[..length]).unwrap().optional_header;
                        pack.timestamp = header
                            .and_then(|header| header.dts.or(header.pts))
                            .map(Timestamp::get);
                    }
                    rest = &rest[length..];
                }
                _ => {
                    assert!(rest.is_empty(), "bytes after the packs: {rest:02x?}");
                    return (packs, false);
                }
            }
        }
    }

    /// The 27 MHz ticks `length` bytes take to arrive at `program_mux_rate`
    /// (§2.5.2.2 equation 2-21), rounded up
    fn arrival(length: usize, program_mux_rate: u32) -> u64 {
        (u64::try_from(length).unwrap() * 27_000_000).div_ceil(u64::from(program_mux_rate) * 50)
    }

    /// The ticks from `earlier` to `later` round the circle of the SCR
    fn since(later: u64, earlier: u64) -> u64 {
        (later + ClockReference::MODULUS - earlier) % ClockReference::MODULUS
    }

    /// How `packs` keep to §2.7.1 and equation 2-21: whether every SCR is
    /// after the one before, no earlier than the bytes of the pack before
    /// have arrived, and at most 0.7 s on, and the longest step
    fn scr_steps(packs: &[Pack], program_mux_rate: u32) -> (bool, u64) {
        let steps: Vec<(u64, u64)> = packs
            .windows(2)
            .map(|pair| {
                (
                    since(pair[1].scr, pair[0].scr),
                    arrival(pair[0].length, program_mux_rate),
                )
            })
            .collect();
        let kept = steps
            .iter()
            .all(|&(step, arrival)| step > 0 && step >= arrival && step <= LONGEST_SCR_INTERVAL);
        (kept, steps.iter().map(|&(step, _)| step).max().unwrap_or(0))
    }

    #[test]
    fn the_pes_packets_given_read_back_as_given_with_the_p_std_buffer_on_the_first_of_each_stream()
    {
        let inputs = inputs();
        let stream = multiplex(MuxerConfig::new(RATE, MUX_DELAY), &inputs);

        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&stream);
        demuxer.finish();
        let mut read: Vec<Input> = Vec::new();
        let mut first_buffers = BTreeMap::new();
        let mut last_of_stream: BTreeMap<u8, usize> = BTreeMap::new();
        while let Some(event) = demuxer.poll_event() {
            let Event::PesPacket(packet) = event else {
                continue;
            };
            let stream_id = packet.stream_id.get();
            let header = packet.optional_header.as_ref().unwrap();
            first_buffers.entry(stream_id).or_insert_with(|| {
                header
                    .pes_extension
                    .as_ref()
                    .and_then(|extension| extension.p_std_buffer)
            });
            // Why not one input for each PES packet: a PES packet too long
            // for a pack goes on in the next ones, without its times.
            match header.pts {
                Some(pts) => {
                    last_of_stream.insert(stream_id, read.len());
                    read.push(Input {
                        stream_id,
                        pts: pts.get(),
                        dts: header.dts.map(Timestamp::get),
                        payload: packet.payload.to_vec(),
                    });
                }
                None => read[last_of_stream[&stream_id]]
                    .payload
                    .extend_from_slice(packet.payload),
            }
        }
        assert_eq!(
            (read, first_buffers),
            (
                inputs,
                BTreeMap::from([(AUDIO, Some(AUDIO_BUFFER)), (VIDEO, Some(VIDEO_BUFFER))])
            )
        );
    }

    #[test]
    fn every_pack_fits_pack_size_and_only_the_first_has_the_system_header_and_the_map() {
        let (packs, ended) = packs(&multiplex(MuxerConfig::new(RATE, MUX_DELAY), &inputs()));

        let headers: Vec<(usize, Vec<u8>)> = packs
            .iter()
            .enumerate()
            .filter(|(_, pack)| pack.start_codes.iter().any(|&code| code < 0xbd))
            .map(|(index, pack)| (index, pack.start_codes.clone()))
            .collect();
        assert_eq!(
            (packs.iter().all(|pack| pack.length <= 2048), headers, ended),
            (true, vec![(0, vec![0xbb, 0xbc, VIDEO])], true)
        );
    }

    #[test]
    fn each_scr_is_the_dts_less_the_mux_delay_or_the_one_before_on_by_the_bytes_of_its_pack() {
        let (packs, _) = packs(&multiplex(MuxerConfig::new(RATE, MUX_DELAY), &inputs()));

        let mut expected: Vec<u64> = Vec::new();
        for (index, pack) in packs.iter().enumerate() {
            let due = pack
                .timestamp
                .map(|timestamp| timestamp * 300 - MUX_DELAY_TICKS);
            let scr = match index.checked_sub(1).map(|before| &packs[before]) {
                None => due.unwrap(),
                Some(before) => {
                    let arrived = expected[index - 1] + arrival(before.length, RATE);
                    due.map_or(arrived, |due| arrived.max(due))
                }
            };
            expected.push(scr);
        }
        let scrs: Vec<u64> = packs.iter().map(|pack| pack.scr).collect();
        assert_eq!(scrs, expected);
    }

    #[test]
    fn time_stamps_from_below_the_mux_delay_keep_the_scrs_within_0_7_s() {
        let (packs, _) = packs(&multiplex(
            MuxerConfig::new(RATE, MUX_DELAY),
            &stepped(0, 0),
        ));
        assert!(scr_steps(&packs, RATE).0);
    }

    #[test]
    fn time_stamps_across_the_wrap_of_33_bits_keep_the_scrs_within_0_7_s() {
        let (packs, _) = packs(&multiplex(
            MuxerConfig::new(RATE, MUX_DELAY),
            &stepped(Timestamp::MODULUS - 50 * 1_800, 0),
        ));
        assert!(scr_steps(&packs, RATE).0);
    }

    #[test]
    fn a_leap_of_the_time_stamps_keeps_the_scrs_within_0_7_s() {
        let (packs, _) = packs(&multiplex(
            MuxerConfig::new(RATE, MUX_DELAY),
            &stepped(90_000, 180_000),
        ));
        let (kept, _) = scr_steps(&packs, RATE);
        assert_eq!(
            (kept, packs.iter().any(|pack| pack.start_codes.is_empty())),
            (true, true)
        );
    }

    #[test]
    fn a_leap_at_the_longest_pack_size_for_the_rate_keeps_the_scrs_within_0_7_s() {
        // 5 000 bytes a second, and packs of 3 500 bytes, which take 0.7 s
        // and arrive within the 1 s before their DTS
        let mut config = MuxerConfig::new(100, Duration::from_secs(1));
        config.pack_size = 3_500;
        let mut inputs = stepped(90_000, 900_000);
        for input in &mut inputs {
            input.payload.truncate(10);
        }
        // The last before the leap in a full pack: 14 + 19 of header and
        // 3 467 bytes
        inputs[49].payload = vec![0xbb; 3_467];
        let (packs, _) = packs(&multiplex(config, &inputs));
        let (kept, longest) = scr_steps(&packs, 100);
        let lengths: Vec<usize> = packs.iter().map(|pack| pack.length).collect();
        let full = lengths.iter().rposition(|&length| length == 3_500).unwrap();
        assert_eq!(
            (kept, longest, &lengths[full..=full + 1]),
            (true, LONGEST_SCR_INTERVAL, &[3_500, 14][..])
        );
    }

    #[test]
    fn at_a_low_rate_every_pack_arrives_within_0_7_s_whatever_pack_size() {
        // 500 bytes a second: 350 bytes arrive in 0.7 s, less than the
        // default pack_size of 2048. 300 bytes of video, more than the first
        // pack holds, take some 0.8 s with their headers, within the 1 s
        // before their DTS.
        let config = MuxerConfig::new(10, Duration::from_secs(1));
        let inputs = [
            Input {
                stream_id: VIDEO,
                pts: 360_000,
                dts: Some(360_000),
                payload: vec![0xaa; 300],
            },
            Input {
                stream_id: AUDIO,
                pts: 450_000,
                dts: None,
                payload: vec![0xbb; 10],
            },
        ];
        let (packs, _) = packs(&multiplex(config, &inputs));
        let (kept, _) = scr_steps(&packs, 10);
        assert_eq!(
            (packs.iter().all(|pack| pack.length <= 350), kept),
            (true, true)
        );
    }

    /// Video and audio of 10 bytes each, a second apart, from a second in,
    /// leaping `leaps` 90 kHz ticks after the 2nd and the 4th
    fn sparse(leaps: [u64; 2]) -> Vec<Input> {
        let mut dts = 90_000;
        (0_u64..6)
            .map(|index| {
                dts += match index {
                    0 => 0,
                    2 => leaps[0],
                    4 => leaps[1],
                    _ => 90_000,
                };
                Input {
                    stream_id: if index % 2 == 0 { VIDEO } else { AUDIO },
                    pts: dts % Timestamp::MODULUS,
                    dts: Some(dts % Timestamp::MODULUS),
                    payload: vec![0xaa; 10],
                }
            })
            .collect()
    }

    #[test]
    fn long_leaps_at_the_lowest_rate_keep_the_scrs_within_0_7_s() {
        // 150 bytes a second: the lowest rate whose 0.7 s carries the first
        // pack of the two streams and a PES packet with its PTS and DTS
        let (packs, _) = packs(&multiplex(
            MuxerConfig::new(3, MUX_DELAY),
            &sparse([900_000, 90_000 * 3_500]),
        ));
        let (kept, longest) = scr_steps(&packs, 3);
        assert_eq!(
            (
                kept,
                longest,
                packs
                    .iter()
                    .filter(|pack| pack.start_codes.is_empty())
                    .count()
            ),
            // ⌈10 / 0.7⌉ − 1 and ⌈3 500 / 0.7⌉ − 1 packs of a pack header
            // alone, and ⌈1 / 0.7⌉ − 1 in each of the three steps of 1 s
            (true, LONGEST_SCR_INTERVAL, 14 + 4_999 + 3)
        );
    }

    #[test]
    fn a_leap_across_the_wrap_of_33_bits_keeps_the_scrs_within_0_7_s() {
        let (packs, _) = packs(&multiplex(
            MuxerConfig::new(RATE, MUX_DELAY),
            &stepped(Timestamp::MODULUS - 50 * 1_800, 180_000),
        ));
        assert!(scr_steps(&packs, RATE).0);
    }

    #[test]
    fn the_same_input_gives_the_same_stream() {
        let inputs = stepped(Timestamp::MODULUS - 50 * 1_800, 180_000);
        assert_eq!(
            multiplex(MuxerConfig::new(RATE, MUX_DELAY), &inputs),
            multiplex(MuxerConfig::new(RATE, MUX_DELAY), &inputs)
        );
    }
}
