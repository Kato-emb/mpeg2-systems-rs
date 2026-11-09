//! `cargo xtask fixtures`: the stream fixtures of the tests, made with FFmpeg
//!
//! The fixtures are committed as they were first made; this records how, and
//! makes them again on a host that has FFmpeg. CI does not run it: another
//! FFmpeg version writes other bytes.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::Command;

pub(crate) mod arib;

/// What `cargo xtask fixtures --help` prints
pub(crate) const USAGE: &str = "\
Usage: cargo xtask fixtures [--arib]

Generate the stream fixtures in crates/mpeg2-systems/tests/fixtures/ with
FFmpeg, which must be on PATH (the committed fixtures were made with FFmpeg
6.1.1). The sources are FFmpeg's lavfi testsrc (64x64, 5 fps) and a 440 Hz
sine, 0.6 s each, encoded as MPEG-2 video at 200 kbit/s and MPEG-1 Layer II
audio at 64 kbit/s.

  188.ts      Transport Stream: PAT, PMT, PCR, and PES of video on PID 0x100
              and audio on PID 0x101
  192.ts      188.ts with 4 bytes of 0x00 before each packet, as M2TS frames it
  204.ts      188.ts with 16 bytes of 0xFF after each packet, where
              Reed-Solomon parity would be
  program.ps  Program Stream of MPEG-2 packs: system header, padding, and PES
              of video (stream_id 0xE0) and audio (0xC0)

With --arib, generate crates/mpeg2-systems-arib/tests/fixtures/caption.ts
instead, with TSDuck's tsp, which must be on PATH (the committed fixture was
made with TSDuck 3.45-4798, the version scripts/fetch-tools.sh fetches):

  caption.ts  Transport Stream of 43 packets. TSDuck writes the PAT (program
              1, PMT on PID 0x1000) and the PMT (PCR_PID 0x1FFF; a caption
              elementary stream of stream_type 0x06 on PID 0x0130, with
              component_tag 0x30, data_component_id 0x0008 and
              additional_arib_caption_info 0x3D, the values of ARIB TR-B14
              Fascicle 2 Volume 3 Part 1 4.2.8.1 and Table 4-9). Three
              synchronized PES packets of ARIB STD-B24, built here from the
              syntax tables with their own CRC_16, one Transport Packet each:
              caption management data (DGI 0x00, one language: jpn, DMF 1010,
              Format 1000, as Table 4-4) at PTS 1 s, and the caption
              statements of the characters 0x24 0x22 and 0x24 0x24 of the
              Kanji set (DGI 0x01, versions 0 and 1) at 2 s and 3 s
";

/// Bytes in a Transport Packet
const PACKET_LENGTH: usize = 188;

/// The arguments that give FFmpeg its two sources and their encoders
const SOURCES: [&str; 12] = [
    "-hide_banner",
    "-loglevel",
    "error",
    "-y",
    "-f",
    "lavfi",
    "-i",
    "testsrc=size=64x64:rate=5:duration=0.6",
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:duration=0.6",
];

/// The arguments that encode the sources as MPEG-2 video and MPEG-1 Layer II audio
const ENCODERS: [&str; 8] = [
    "-c:v",
    "mpeg2video",
    "-b:v",
    "200k",
    "-c:a",
    "mp2",
    "-b:a",
    "64k",
];

/// Generates every fixture into `crates/mpeg2-systems/tests/fixtures/`
pub(crate) fn generate() -> Result<(), String> {
    require_tool(
        "ffmpeg",
        "-version",
        "it is a host prerequisite, not installed by the workspace",
    )?;
    let directory = fixtures_directory("mpeg2-systems")?;
    let transport_stream = directory.join("188.ts");
    ffmpeg("mpegts", &transport_stream)?;
    let packets = read(&transport_stream)?;
    write(
        &directory.join("192.ts"),
        &reframe(&packets, &[0x00; 4], &[])?,
    )?;
    write(
        &directory.join("204.ts"),
        &reframe(&packets, &[], &[0xff; 16])?,
    )?;

    // Why not `-f mpeg`: it can write MPEG-1 packs, which the library does not
    // read; `vob` writes MPEG-2 ones.
    let program_stream = directory.join("program.ps");
    ffmpeg("vob", &program_stream)?;
    first_pack_is_mpeg_2(&read(&program_stream)?)
}

/// Refuses to go on when `tool` cannot be run with `argument`, `hint` telling
/// where it comes from when it is not on PATH
fn require_tool(tool: &str, argument: &str, hint: &str) -> Result<(), String> {
    match Command::new(tool).arg(argument).output() {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            Err(format!("{tool} is not on PATH; {hint}"))
        }
        Err(error) => Err(format!("{tool} could not be run: {error}")),
    }
}

/// Returns `crates/<library>/tests/fixtures`, made if it is missing
fn fixtures_directory(library: &str) -> Result<std::path::PathBuf, String> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask has no parent directory")?
        .join("crates")
        .join(library)
        .join("tests/fixtures");
    fs::create_dir_all(&directory).map_err(|error| format!("{}: {error}", directory.display()))?;

    Ok(directory)
}

/// Runs FFmpeg on the two sources, writing `format` to `output`
fn ffmpeg(format: &str, output: &Path) -> Result<(), String> {
    let status = Command::new("ffmpeg")
        .args(SOURCES)
        .args(ENCODERS)
        .args(["-f", format])
        .arg(output)
        .status()
        .map_err(|error| format!("ffmpeg could not be run: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "ffmpeg failed writing {}: {status}",
            output.display()
        ))
    }
}

/// Reads a file, naming it in the error
fn read(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Writes a file, naming it in the error
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// Returns `packets` with `prefix` before and `suffix` after each 188-byte packet
fn reframe(packets: &[u8], prefix: &[u8], suffix: &[u8]) -> Result<Vec<u8>, String> {
    let (packets, rest) = packets.as_chunks::<PACKET_LENGTH>();
    if !rest.is_empty() {
        return Err(format!(
            "the transport stream ends {} bytes into a packet",
            rest.len()
        ));
    }

    Ok(packets
        .iter()
        .flat_map(|packet| [prefix, packet.as_slice(), suffix])
        .flatten()
        .copied()
        .collect())
}

/// Refuses a Program Stream whose first pack is not MPEG-2
///
/// An MPEG-2 `pack_header` has `'01'` after `pack_start_code`; ISO/IEC 11172-1
/// has `'0010'`, which the library does not read.
fn first_pack_is_mpeg_2(stream: &[u8]) -> Result<(), String> {
    let marker = stream
        .windows(4)
        .position(|window| window == [0x00, 0x00, 0x01, 0xba])
        .and_then(|start| stream.get(start.checked_add(4)?))
        .ok_or("the program stream has no pack_header")?;
    if marker & 0xc0 == 0x40 {
        Ok(())
    } else {
        Err(format!(
            "the first pack is not MPEG-2: its bits after pack_start_code are {:08b}",
            marker
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{first_pack_is_mpeg_2, reframe};

    #[test]
    fn each_packet_is_framed_by_the_prefix_and_the_suffix() {
        let packets: Vec<u8> = [[0x47; 188], [0x48; 188]].concat();
        let expected: Vec<u8> = [
            &[0x00; 2][..],
            &[0x47; 188],
            &[0xff; 3],
            &[0x00; 2],
            &[0x48; 188],
            &[0xff; 3],
        ]
        .concat();

        assert_eq!(reframe(&packets, &[0x00; 2], &[0xff; 3]), Ok(expected));
    }

    #[test]
    fn a_stream_that_ends_inside_a_packet_is_refused() {
        assert_eq!(
            reframe(&[0x47; 189], &[], &[]),
            Err("the transport stream ends 1 bytes into a packet".to_owned())
        );
    }

    #[test]
    fn only_a_program_stream_that_starts_with_an_mpeg_2_pack_is_accepted() {
        assert_eq!(
            [
                first_pack_is_mpeg_2(&[0x00, 0x00, 0x00, 0x01, 0xba, 0x44]),
                first_pack_is_mpeg_2(&[0x00, 0x00, 0x01, 0xba, 0x21]),
                first_pack_is_mpeg_2(&[0x00, 0x00, 0x01, 0xbb]),
            ],
            [
                Ok(()),
                Err(
                    "the first pack is not MPEG-2: its bits after pack_start_code are 00100001"
                        .to_owned()
                ),
                Err("the program stream has no pack_header".to_owned()),
            ]
        );
    }
}
