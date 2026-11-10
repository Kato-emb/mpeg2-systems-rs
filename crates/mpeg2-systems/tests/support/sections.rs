//! What the tests of the sections a Transport Stream demuxer reassembles
//! share: Transport Packets and sections laid out by hand

use mpeg2_systems::ts::demux::Demuxer;

use crate::support::{Seen, drain, pcr};

/// A Transport Packet laid out by hand: the header, an adaptation field
/// of `adaptation_field` bytes (its length byte not included) if any, then
/// `payload` if any, padded with 0xFF
pub(crate) fn packet(
    pid: u16,
    payload_unit_start_indicator: bool,
    continuity_counter: u8,
    adaptation_field: Option<&[u8]>,
    payload: Option<&[u8]>,
) -> Vec<u8> {
    let [high, low] = pid.to_be_bytes();
    let adaptation_field_control = match (adaptation_field, payload) {
        (None, _) => 0x10,
        (Some(_), None) => 0x20,
        (Some(_), Some(_)) => 0x30,
    };
    let mut bytes = vec![
        0x47,
        high | if payload_unit_start_indicator {
            0x40
        } else {
            0
        },
        low,
        adaptation_field_control | continuity_counter,
    ];
    if let Some(field) = adaptation_field {
        let length = if payload.is_some() { field.len() } else { 183 };
        bytes.push(u8::try_from(length).unwrap());
        bytes.extend_from_slice(field);
        bytes.resize(5 + length, 0xFF);
    }
    bytes.extend_from_slice(payload.unwrap_or_default());
    bytes.resize(188, 0xFF);
    bytes
}

/// A short-form section of `table_id` with `body`, which carries no
/// `CRC_32`
pub(crate) fn short_section(table_id: u8, body: &[u8]) -> Vec<u8> {
    let [high, low] = u16::try_from(body.len()).unwrap().to_be_bytes();
    let mut bytes = vec![table_id, 0x30 | high, low];
    bytes.extend_from_slice(body);
    bytes
}

/// Null packets enough to acquire the grid, then `packets`
pub(crate) fn behind_null_packets(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = Vec::new();
    for _ in 0..5 {
        stream.extend(packet(0x1FFF, false, 0, None, Some(&[])));
    }
    for bytes in packets {
        stream.extend_from_slice(bytes);
    }
    stream
}

/// The events of each stream in turn through `demuxer`, the input ended
/// after each
pub(crate) fn demux_in_turn(mut demuxer: Demuxer, streams: &[&[u8]]) -> Vec<Vec<Seen>> {
    let mut turns = Vec::new();
    for stream in streams {
        let mut seen = Vec::new();
        demuxer.handle_input(stream);
        drain(&mut demuxer, &mut seen);
        demuxer.finish();
        drain(&mut demuxer, &mut seen);
        turns.push(seen);
    }
    turns
}

/// A long-form section of `table_id`, with `table_id_extension`, at
/// version `version_number`, in force, its `CRC_32` computed
pub(crate) fn table_section(
    table_id: u8,
    table_id_extension: u16,
    version_number: u8,
    body: &[u8],
) -> Vec<u8> {
    let [high, low] = u16::try_from(body.len() + 9).unwrap().to_be_bytes();
    let mut bytes = vec![table_id, 0xB0 | high, low];
    bytes.extend_from_slice(&table_id_extension.to_be_bytes());
    bytes.extend_from_slice(&[0xC1 | (version_number << 1), 0x00, 0x00]);
    bytes.extend_from_slice(body);
    bytes.extend_from_slice(&mpeg2_systems::crc::crc_32(&bytes).to_be_bytes());
    bytes
}

/// A PAT section of `transport_stream_id` 1 at `version_number`, naming
/// `programs` (`program_number`, PID)
pub(crate) fn pat_section(version_number: u8, programs: &[(u16, u16)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (program_number, pid) in programs {
        body.extend_from_slice(&program_number.to_be_bytes());
        body.extend_from_slice(&(0xE000 | pid).to_be_bytes());
    }
    table_section(0x00, 0x0001, version_number, &body)
}

pub(crate) fn pcrs() -> Vec<Seen> {
    vec![pcr(63_000), pcr(81_000), pcr(99_000)]
}
