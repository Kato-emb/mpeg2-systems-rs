//! Reads a Transport Stream and multiplexes the PES packets of the program
//! its first PMT describes into a Program Stream
//!
//! ```text
//! cargo run --example ts_to_ps -- <in.ts> <out.ps>
//! ```
//!
//! The muxer takes its streams when the first PMT is read. Each video stream
//! of the PMT (ISO/IEC 11172-2, ISO/IEC 13818-2, AVC or HEVC) takes a
//! `stream_id` from 0xE0, and each audio stream (ISO/IEC 11172-3,
//! ISO/IEC 13818-3 or ISO/IEC 13818-7) one from 0xC0, in the order the PMT
//! names them; their PES packets are written with that `stream_id`. The
//! P-STD buffer is 230 units of 1024 bytes for video and 32 units of 128
//! bytes for audio, and `program_mux_rate` is 10.08 Mbit/s. A stream of any
//! other `stream_type` is not registered; its PES packets, and those of any
//! other program, are left out, and how many is printed at the end.
//!
//! The PES packets go to the muxer in the order the demuxer reports them;
//! they are not sorted into the order of their DTS across the streams.

use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};

use mpeg2_systems::pes::StreamId;
use mpeg2_systems::pes::extension::PStdBuffer;
use mpeg2_systems::ps::mux::{Muxer, MuxerConfig};
use mpeg2_systems::ps::psm::ElementaryStream;
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};

/// Bytes handed over per read, the Transport Packets of one UDP datagram
const CHUNK_LENGTH: usize = 7 * 188;
/// `program_mux_rate`, 10.08 Mbit/s in units of 50 bytes per second
const PROGRAM_MUX_RATE: u32 = 25_200;
/// P-STD buffer of a video stream
const VIDEO_BUFFER: PStdBuffer = PStdBuffer::new(true, 230);
/// P-STD buffer of an audio stream
const AUDIO_BUFFER: PStdBuffer = PStdBuffer::new(false, 32);

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args().skip(1);
    let (Some(input), Some(output)) = (arguments.next(), arguments.next()) else {
        return Err("usage: ts_to_ps <in.ts> <out.ps>".into());
    };
    let reader = BufReader::with_capacity(CHUNK_LENGTH, File::open(input)?);
    let mut writer = BufWriter::new(File::create(output)?);
    let left_out = convert(reader, &mut writer)?;
    writer.flush()?;
    eprintln!("PES packets left out: {left_out}");
    Ok(())
}

/// Writes the Program Stream of the Transport Stream `reader` holds into
/// `writer`, and returns how many PES packets it left out
fn convert(mut reader: impl BufRead, mut writer: impl Write) -> Result<usize, Box<dyn Error>> {
    let mut demuxer = Demuxer::new(DemuxerConfig::default().with_subscribe_elementary_pids(true));
    let mut muxer = Muxer::new(MuxerConfig::new(PROGRAM_MUX_RATE))?;
    let mut stream_ids: Option<Vec<(Pid, StreamId)>> = None;
    let mut left_out: usize = 0;
    let mut out = Vec::new();

    loop {
        let chunk = reader.fill_buf()?;
        let length = chunk.len();
        if length == 0 {
            demuxer.finish();
        } else {
            demuxer.handle_input(chunk);
        }
        reader.consume(length);
        while let Some(event) = demuxer.poll_event() {
            match event {
                Event::TsProgramMapSection { section, .. } if stream_ids.is_none() => {
                    let (mut video, mut audio) = (0xe0..=0xef, 0xc0..=0xdf);
                    let mut registered = Vec::new();
                    for stream in section.elementary_streams.iter() {
                        let stream = stream?;
                        let (stream_id, buffer) = match stream.stream_type {
                            StreamType::ISO_IEC_11172_2_VIDEO
                            | StreamType::ISO_IEC_13818_2_VIDEO
                            | StreamType::AVC_VIDEO
                            | StreamType::HEVC_VIDEO => (video.next(), VIDEO_BUFFER),
                            StreamType::ISO_IEC_11172_3_AUDIO
                            | StreamType::ISO_IEC_13818_3_AUDIO
                            | StreamType::ISO_IEC_13818_7_AUDIO => (audio.next(), AUDIO_BUFFER),
                            _ => continue,
                        };
                        let stream_id = StreamId::from(stream_id.ok_or("too many streams")?);
                        muxer.add_stream(
                            ElementaryStream::new(stream.stream_type, stream_id),
                            buffer,
                        )?;
                        registered.push((stream.elementary_pid, stream_id));
                    }
                    stream_ids = Some(registered);
                }
                Event::PesPacket {
                    pid, mut packet, ..
                } => match stream_ids.iter().flatten().find(|(on, _)| *on == pid) {
                    Some(&(_, stream_id)) => {
                        packet.stream_id = stream_id;
                        packet.length_unspecified = false;
                        muxer.handle_pes_packet(&packet)?;
                        muxer.poll_output(&mut out);
                    }
                    None => left_out += 1,
                },
                _ => {}
            }
        }
        if length == 0 {
            break;
        }
        writer.write_all(&out)?;
        out.clear();
    }

    muxer.finish();
    muxer.poll_output(&mut out);
    writer.write_all(&out)?;
    Ok(left_out)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::process::Command;

    use mpeg2_systems::pes::PesPacket;
    use mpeg2_systems::ps::demux as ps_demux;
    use mpeg2_systems::time::Timestamp;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
    use serde_json::Value;

    use super::convert;

    /// The PTS and the DTS of each PES packet that carries a PTS, and the
    /// payloads of all the PES packets joined
    type Read = (Vec<(u64, Option<u64>)>, Vec<u8>);

    /// The PTS, the DTS and the size of a frame ffprobe reads
    type Frame = (Option<i64>, Option<i64>, String);

    /// The Transport Stream FFmpeg wrote, among the fixtures of the crate
    const TRANSPORT_STREAM: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../crates/mpeg2-systems/tests/fixtures/188.ts"
    );

    fn transport_stream() -> Vec<u8> {
        std::fs::read(TRANSPORT_STREAM).unwrap()
    }

    /// The Program Stream of the Transport Stream, and how many PES packets
    /// it left out
    fn converted() -> (Vec<u8>, usize) {
        let mut program_stream = Vec::new();
        let left_out = convert(&transport_stream()[..], &mut program_stream).unwrap();
        (program_stream, left_out)
    }

    fn read(packet: &PesPacket<'_>, streams: &mut BTreeMap<u8, Read>) {
        let (times, payload) = streams.entry(packet.stream_id.get()).or_default();
        let header = packet.optional_header.as_ref();
        if let Some(pts) = header.and_then(|header| header.pts) {
            times.push((
                pts.get(),
                header.and_then(|header| header.dts).map(Timestamp::get),
            ));
        }
        payload.extend_from_slice(packet.payload);
    }

    #[test]
    fn the_program_stream_carries_the_pes_packets_of_the_transport_stream() {
        let mut from_transport_stream = BTreeMap::new();
        let mut demuxer =
            Demuxer::new(DemuxerConfig::default().with_subscribe_elementary_pids(true));
        demuxer.handle_input(&transport_stream());
        demuxer.finish();
        while let Some(event) = demuxer.poll_event() {
            if let Event::PesPacket { packet, .. } = event {
                read(&packet, &mut from_transport_stream);
            }
        }

        let (program_stream, left_out) = converted();
        let mut from_program_stream = BTreeMap::new();
        let mut demuxer = ps_demux::Demuxer::new();
        demuxer.handle_input(&program_stream);
        demuxer.finish();
        while let Some(event) = demuxer.poll_event() {
            if let ps_demux::Event::PesPacket(packet) = event {
                read(&packet, &mut from_program_stream);
            }
        }

        assert_eq!((from_program_stream, left_out), (from_transport_stream, 0));
    }

    /// The `codec_type` and `codec_name` of each stream ffprobe reads in
    /// the file at `path`, and the PTS, the DTS and the size of each of its
    /// frames
    fn ffprobe(path: &str) -> Vec<(String, String, Vec<Frame>)> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-fflags",
                "+nofillin",
                "-of",
                "json",
                "-show_streams",
                "-show_packets",
                path,
            ])
            .output()
            .map_err(|error| format!("ffprobe did not run ({error}); put it on PATH"))
            .unwrap();
        assert!(
            output.status.success(),
            "ffprobe {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let probed: Value = serde_json::from_slice(&output.stdout).unwrap();
        let array = |key: &str| probed.get(key).and_then(Value::as_array).unwrap();
        let mut streams: Vec<_> = array("streams")
            .iter()
            .map(|stream| {
                let text = |key: &str| stream.get(key).and_then(Value::as_str).unwrap().to_owned();
                (text("codec_type"), text("codec_name"), Vec::new())
            })
            .collect();
        for packet in array("packets") {
            let index = packet.get("stream_index").and_then(Value::as_u64).unwrap();
            let index = usize::try_from(index).unwrap();
            streams.get_mut(index).unwrap().2.push((
                packet.get("pts").and_then(Value::as_i64),
                packet.get("dts").and_then(Value::as_i64),
                packet
                    .get("size")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_owned(),
            ));
        }
        streams
    }

    #[test]
    #[ignore = "runs ffprobe from FFmpeg"]
    fn ffprobe_reads_the_program_stream_as_it_reads_the_transport_stream() {
        let path = std::env::temp_dir().join("ts_to_ps.ps");
        std::fs::write(&path, converted().0).unwrap();

        assert_eq!(ffprobe(path.to_str().unwrap()), ffprobe(TRANSPORT_STREAM));
    }
}
