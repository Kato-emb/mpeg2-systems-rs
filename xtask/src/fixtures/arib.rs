//! `cargo xtask fixtures --arib`: the ARIB caption fixture, its tables made
//! with TSDuck and its caption PES packets built here from the syntax tables
//!
//! TSDuck cannot make PES packets, so the three caption PES packets are laid
//! out byte by byte from ARIB STD-B24, with this file's own `CRC_16`, never
//! with the writers of the workspace's libraries.

use std::env;
use std::fs;
use std::process::Command;

use super::{PACKET_LENGTH, fixtures_directory, read, require_tool, write};

/// Transport Packets of the stream TSDuck writes, null packets replaced by
/// the PAT and the PMT
const TSDUCK_PACKETS: &str = "40";

/// PID of the caption elementary stream, high and low bytes
const CAPTION_PID: [u8; 2] = [0x01, 0x30];

/// The PAT TSDuck writes: program 1, its PMT on PID 0x1000
const PAT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <PAT version="0" current="true" transport_stream_id="1">
    <service service_id="1" program_map_PID="0x1000"/>
  </PAT>
</tsduck>
"#;

/// The PMT TSDuck writes: the caption elementary stream, with the
/// descriptors ARIB TR-B14 Fascicle 2 Volume 3 Part 1 gives it
///
/// `component_tag` 0x30 is the default caption ES (§4.2.8.1);
/// `data_component_id` 0x0008 with `additional_arib_caption_info` 0x3D is DMF
/// `'0011'` and Timing `'01'` (§4.2.8.5, Table 4-9).
const PMT_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <PMT version="0" current="true" service_id="1" PCR_PID="0x1FFF">
    <component stream_type="0x06" elementary_PID="0x0130">
      <stream_identifier_descriptor component_tag="0x30"/>
      <data_component_descriptor data_component_id="0x0008">
        <additional_data_component_info>3D</additional_data_component_info>
      </data_component_descriptor>
    </component>
  </PMT>
</tsduck>
"#;

/// The `data_group_data_byte` of the caption management data: TMD `'00'`
/// and its Reserved bits, one language, no data unit
///
/// The language takes the values of TR-B14 Fascicle 2 Volume 3 Part 1
/// §4.2.5.1, Table 4-4: `language_tag` 0, reserved `'1'`, DMF `'1010'`
/// (0x1A), `jpn`, Format `'1000'`, TCS `'00'`, `rollup_mode` `'00'` (0x80).
const MANAGEMENT: [u8; 10] = [0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00, 0x00, 0x00];

/// The `data_group_data_byte` of a caption statement of one statement body
/// data unit holding `character`: TMD `'00'` and its Reserved bits,
/// `data_unit_loop_length` 7, `unit_separator` 0x1F, `data_unit_parameter`
/// 0x20, `data_unit_size` 2
const fn statement(character: [u8; 2]) -> [u8; 11] {
    let [first, second] = character;
    [
        0x3f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, first, second,
    ]
}

/// The three caption PES packets: the index, among TSDuck's packets, of the
/// packet each is put before, its PTS in 90 kHz units, the `data_group_id`,
/// the `data_group_version`, and the data
///
/// The characters are of the Kanji set of ARIB STD-B24 Volume 1 Part 2
/// §7.1.1.3, Table 7-4, invoked to GL as it is by default: あ (row 4 cell 2,
/// 0x24 0x22) and い (row 4 cell 4, 0x24 0x24).
const CAPTIONS: [(usize, u64, u8, u8, &[u8]); 3] = [
    (10, 90_000, 0x00, 0, &MANAGEMENT),
    (20, 180_000, 0x01, 0, &statement([0x24, 0x22])),
    (30, 270_000, 0x01, 1, &statement([0x24, 0x24])),
];

/// Makes `crates/mpeg2-systems-arib/tests/fixtures/caption.ts`
pub(crate) fn generate() -> Result<(), String> {
    require_tool(
        "tsp",
        "--version",
        "scripts/fetch-tools.sh puts TSDuck in <directory>/bin",
    )?;
    let work = env::temp_dir().join("mpeg2-systems-arib-fixture");
    fs::create_dir_all(&work).map_err(|error| format!("{}: {error}", work.display()))?;
    let pat = work.join("pat.xml");
    let pmt = work.join("pmt.xml");
    let tables = work.join("tables.ts");
    write(&pat, PAT_XML.as_bytes())?;
    write(&pmt, PMT_XML.as_bytes())?;
    let status = Command::new("tsp")
        .args(["-I", "null", TSDUCK_PACKETS])
        .args(["-P", "inject", "--pid", "0", "--inter-packet", "10"])
        .arg(&pat)
        .args(["-P", "inject", "--pid", "0x1000", "--inter-packet", "10"])
        .arg(&pmt)
        .args(["-O", "file"])
        .arg(&tables)
        .status()
        .map_err(|error| format!("tsp could not be run: {error}"))?;
    if !status.success() {
        return Err(format!("tsp failed writing {}: {status}", tables.display()));
    }

    let mut captions = Vec::with_capacity(CAPTIONS.len());
    for (continuity_counter, &(before, pts, data_group_id, version, data)) in
        (0_u8..).zip(CAPTIONS.iter())
    {
        let pes = pes_packet(pts, &data_group(data_group_id, version, data)?)?;
        captions.push((before, transport_packet(continuity_counter, &pes)?));
    }
    let stream = insert(&read(&tables)?, &captions)?;
    write(
        &fixtures_directory("mpeg2-systems-arib")?.join("caption.ts"),
        &stream,
    )
}

/// Returns the `data_group` of `data`, ARIB STD-B24 Volume 1 Part 3 §9.2,
/// Table 9-1, with its `CRC_16`
fn data_group(data_group_id: u8, version: u8, data: &[u8]) -> Result<Vec<u8>, String> {
    let size = u16::try_from(data.len()).map_err(|_| "a data group of over 65535 bytes")?;
    let [size_high, size_low] = size.to_be_bytes();
    let mut group = vec![
        data_group_id << 2 | version,
        0x00,
        0x00,
        size_high,
        size_low,
    ];
    group.extend_from_slice(data);
    let crc = crc_16(&group);
    group.extend_from_slice(&crc.to_be_bytes());

    Ok(group)
}

/// Returns the CRC of `bytes` by the division §9.2 gives: by
/// X^16 + X^12 + X^5 + 1, from 0, nothing inverted (CRC-16/XMODEM)
fn crc_16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0, |crc, &byte| {
        (0..8).fold(crc ^ u16::from(byte) << 8, |crc, _| {
            if crc & 0x8000 == 0 {
                crc << 1
            } else {
                crc << 1 ^ 0x1021
            }
        })
    })
}

/// Returns a PES packet of `stream_id` 0xBD with a PTS, carrying the
/// synchronized PES data of `data_group`, ARIB STD-B24 Volume 3 §5.1,
/// Table 5-1: `data_identifier` 0x80, `private_stream_id` 0xFF,
/// `PES_data_packet_header_length` 0
fn pes_packet(pts: u64, data_group: &[u8]) -> Result<Vec<u8>, String> {
    let mut body = vec![0x80, 0x80, 0x05];
    body.extend_from_slice(&pts_field(pts));
    body.extend_from_slice(&[0x80, 0xff, 0xf0]);
    body.extend_from_slice(data_group);
    let length = u16::try_from(body.len()).map_err(|_| "a PES packet of over 65535 bytes")?;
    let mut packet = vec![0x00, 0x00, 0x01, 0xbd];
    packet.extend_from_slice(&length.to_be_bytes());
    packet.extend_from_slice(&body);

    Ok(packet)
}

/// Returns the 5 bytes of `PTS_DTS_flags` `'10'`: `'0010'`, the 33 bits of
/// `pts` and their marker bits, ISO/IEC 13818-1 §2.4.3.7
fn pts_field(pts: u64) -> [u8; 5] {
    let byte = |value: u64| u8::try_from(value & 0xff).unwrap_or_default();
    [
        byte(0x21 | (pts >> 29) & 0x0e),
        byte(pts >> 22),
        byte((pts >> 14) & 0xfe | 0x01),
        byte(pts >> 7),
        byte(pts << 1 | 0x01),
    ]
}

/// Returns the Transport Packet on the caption PID that starts and holds
/// `pes`, stuffed to 188 bytes by its adaptation field
fn transport_packet(continuity_counter: u8, pes: &[u8]) -> Result<[u8; PACKET_LENGTH], String> {
    // Why not carry a PES packet over several Transport Packets: each one here
    // fits in one, the adaptation field stuffing what it leaves. The stuffing
    // counts adaptation_field_length and the flags byte, so it is 2 or more.
    let stuffing = (PACKET_LENGTH - 4)
        .checked_sub(pes.len())
        .filter(|&stuffing| stuffing >= 2)
        .ok_or("a PES packet too long for one Transport Packet")?;
    let [pid_high, pid_low] = CAPTION_PID;
    let mut packet = vec![
        0x47,
        0x40 | pid_high,
        pid_low,
        0x30 | continuity_counter & 0x0f,
        u8::try_from(stuffing.saturating_sub(1)).map_err(|_| "an adaptation field too long")?,
        0x00,
    ];
    packet.resize(
        packet.len().saturating_add(stuffing).saturating_sub(2),
        0xff,
    );
    packet.extend_from_slice(pes);

    packet
        .try_into()
        .map_err(|packet: Vec<u8>| format!("a Transport Packet of {} bytes", packet.len()))
}

/// Returns TSDuck's `stream` with each of `captions` put before the packet
/// of the index it comes with
fn insert(stream: &[u8], captions: &[(usize, [u8; PACKET_LENGTH])]) -> Result<Vec<u8>, String> {
    let (packets, rest) = stream.as_chunks::<PACKET_LENGTH>();
    let last = captions
        .iter()
        .map(|&(before, _)| before)
        .max()
        .unwrap_or_default();
    if !rest.is_empty() || packets.len() <= last {
        return Err(format!(
            "TSDuck wrote {} bytes, not {TSDUCK_PACKETS} Transport Packets",
            stream.len()
        ));
    }
    let mut output = Vec::with_capacity(stream.len());
    for (index, packet) in packets.iter().enumerate() {
        for (_, caption) in captions.iter().filter(|&&(before, _)| before == index) {
            output.extend_from_slice(caption);
        }
        output.extend_from_slice(packet);
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{crc_16, data_group, pes_packet, pts_field, transport_packet};

    /// A PES packet of caption management data at 1 s, built by hand
    /// independently of this file, which FFmpeg's libaribcaption read as an
    /// ARIB caption packet
    const MANAGEMENT_PES: [u8; 34] = [
        0x00, 0x00, 0x01, 0xbd, 0x00, 0x1c, 0x80, 0x80, 0x05, 0x21, 0x00, 0x05, 0xbf, 0x21, 0x80,
        0xff, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00,
        0x00, 0x00, 0xb0, 0x78,
    ];

    #[test]
    fn crc_16_is_the_crc_16_xmodem_of_the_crc_catalog() {
        assert_eq!(crc_16(b"123456789"), 0x31c3);
    }

    #[test]
    fn a_pts_takes_the_marker_bits_of_the_pes_header() {
        assert_eq!(pts_field(90_000), [0x21, 0x00, 0x05, 0xbf, 0x21]);
    }

    #[test]
    fn a_caption_management_pes_packet_is_laid_out_as_one_ffmpeg_decoded() {
        let group = data_group(0x00, 0, &super::MANAGEMENT).unwrap();

        assert_eq!(pes_packet(90_000, &group), Ok(MANAGEMENT_PES.to_vec()));
    }

    #[test]
    fn a_pes_packet_fills_one_transport_packet_after_its_adaptation_field() {
        let packet = transport_packet(5, &[0x00, 0x00, 0x01, 0xbd, 0x00, 0x00]).unwrap();

        assert_eq!(
            packet.first_chunk::<6>(),
            Some(&[0x47, 0x41, 0x30, 0x35, 0xb1, 0x00])
        );
        assert_eq!(
            packet.last_chunk::<7>(),
            Some(&[0xff, 0x00, 0x00, 0x01, 0xbd, 0x00, 0x00])
        );
    }
}
