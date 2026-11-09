//! What the tests of the Transport Stream muxer share: a program of a video
//! and an audio stream, its PES packets in the order of their DTS, and the
//! stream the muxer writes of them

use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::time::Timestamp;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::mux::{Muxer, MuxerConfig};

pub(crate) const PMT_PID: u16 = 0x1000;
pub(crate) const VIDEO_PID: u16 = 0x0100;
pub(crate) const AUDIO_PID: u16 = 0x0101;
pub(crate) const VIDEO_STREAM_ID: u8 = 0xe0;
pub(crate) const AUDIO_STREAM_ID: u8 = 0xc0;

/// A PES packet to give the muxer
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Input {
    pub(crate) pid: u16,
    pub(crate) stream_id: u8,
    pub(crate) pts: u64,
    pub(crate) dts: Option<u64>,
    pub(crate) payload: Vec<u8>,
}

impl Input {
    pub(crate) fn pes_packet(&self) -> PesPacket<'_> {
        let mut header = OptionalHeader::new();
        header.pts = Some(Timestamp::try_from(self.pts).unwrap());
        header.dts = self.dts.map(|dts| Timestamp::try_from(dts).unwrap());
        let mut packet = PesPacket::new(StreamId::from(self.stream_id), &self.payload);
        packet.optional_header = Some(header);
        packet
    }
}

pub(crate) fn pid(value: u16) -> Pid {
    Pid::try_from(value).unwrap()
}

/// Two seconds of video at 25 frames a second, a frame every 3600 ticks of
/// 90 kHz with its PTS a frame after its DTS, and of audio every 2160
/// ticks, merged in the order of their DTS (their PTS for the audio), the
/// first a second in
pub(crate) fn inputs() -> Vec<Input> {
    let video = (0_u64..50).map(|index| Input {
        pid: VIDEO_PID,
        stream_id: VIDEO_STREAM_ID,
        pts: 90_000 + (index + 1) * 3_600,
        dts: Some(90_000 + index * 3_600),
        payload: (0..if index % 25 == 0 { 20_000 } else { 3_000 })
            .map(|byte: usize| {
                u8::try_from((byte + usize::try_from(index).unwrap()) % 251).unwrap()
            })
            .collect(),
    });
    let audio = (0_u64..84).map(|index| Input {
        pid: AUDIO_PID,
        stream_id: AUDIO_STREAM_ID,
        pts: 90_000 + index * 2_160,
        dts: None,
        payload: vec![u8::try_from(index % 251).unwrap(); 400],
    });
    let mut inputs: Vec<Input> = video.chain(audio).collect();
    inputs.sort_by_key(|input| input.dts.unwrap_or(input.pts));
    inputs
}

/// A muxer of `config` with program 1, its video and its audio stream
pub(crate) fn muxer(config: MuxerConfig) -> Muxer {
    let mut muxer = Muxer::new(config).unwrap();
    muxer.add_program(1, pid(PMT_PID), None, &[]).unwrap();
    muxer
        .add_stream(1, pid(VIDEO_PID), StreamType::ISO_IEC_13818_2_VIDEO, &[])
        .unwrap();
    muxer
        .add_stream(1, pid(AUDIO_PID), StreamType::ISO_IEC_13818_3_AUDIO, &[])
        .unwrap();
    muxer
}

/// The stream [`muxer`] of `config` writes of [`inputs`]
pub(crate) fn multiplex(config: MuxerConfig) -> Vec<u8> {
    let mut muxer = muxer(config);
    let mut stream = Vec::new();
    for input in inputs() {
        muxer
            .handle_pes_packet(pid(input.pid), &input.pes_packet())
            .unwrap();
        muxer.poll_output(&mut stream);
    }
    muxer.finish();
    muxer.poll_output(&mut stream);
    stream
}
