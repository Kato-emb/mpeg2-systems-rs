use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::ps::{MPEG_PROGRAM_END_CODE, PackHeader};

/// A pack header with SCR 0 and `pack_stuffing_length` covering `stuffing`
fn pack_header_bytes(stuffing: &[u8]) -> Vec<u8> {
    let stuffing_length = u8::try_from(stuffing.len() & 0x07).unwrap();
    let mut bytes = vec![
        0x00,
        0x00,
        0x01,
        0xBA,
        0x44,
        0x00,
        0x04,
        0x00,
        0x04,
        0x01,
        0x00,
        0x00,
        0x03,
        0xF8 | stuffing_length,
    ];
    bytes.extend_from_slice(stuffing);
    bytes
}

/// A pack header whose bits after the start code are `'0010'`: an ISO/IEC
/// 11172-1 pack
fn iso_11172_pack_bytes() -> Vec<u8> {
    let mut bytes = pack_header_bytes(&[]);
    bytes[4] = (bytes[4] & 0x3F) | 0x20;
    bytes
}

fn system_header_bytes() -> Vec<u8> {
    vec![
        0x00, 0x00, 0x01, 0xBB, 0x00, 0x06, 0x80, 0x00, 0x01, 0x00, 0x20, 0x00,
    ]
}

fn changed_system_header_bytes() -> Vec<u8> {
    let mut bytes = system_header_bytes();
    bytes[7] = 0x02;
    bytes
}

fn pes_packet_bytes(stream_id: u8, payload: &[u8]) -> Vec<u8> {
    let body_length = u16::try_from(payload.len() + 3).unwrap();
    let mut bytes = vec![0x00, 0x00, 0x01, stream_id];
    bytes.extend_from_slice(&body_length.to_be_bytes());
    bytes.extend_from_slice(&[0x80, 0x00, 0x00]);
    bytes.extend_from_slice(payload);
    bytes
}

fn psm_bytes(version: u8, current_next_indicator: bool) -> Vec<u8> {
    let stream_map = [0x1B, 0xE0, 0x00, 0x00];
    let mut body = vec![
        (u8::from(current_next_indicator) << 7) | 0x60 | (version & 0x1F),
        0xFF,
        0x00,
        0x00,
    ];
    body.extend_from_slice(&u16::try_from(stream_map.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&stream_map);
    let program_stream_map_length = u16::try_from(body.len() + 4).unwrap();
    let mut bytes = vec![0x00, 0x00, 0x01, 0xBC];
    bytes.extend_from_slice(&program_stream_map_length.to_be_bytes());
    bytes.extend_from_slice(&body);
    bytes.extend_from_slice(&crate::crc::crc_32(&bytes).to_be_bytes());
    bytes
}

fn described(event: &Event<'_>) -> String {
    format!("{event:?}")
}

/// Every event the demuxer gives for `stream`, as `Debug` prints it, so the
/// events can outlive their borrow of the demuxer
fn events_of(demuxer: &mut Demuxer, stream: &[u8]) -> Vec<String> {
    demuxer.handle_input(stream);
    drained(demuxer)
}

fn drained(demuxer: &mut Demuxer) -> Vec<String> {
    let mut events = Vec::new();
    while let Some(event) = demuxer.poll_event() {
        events.push(described(&event));
    }
    events
}

fn demux(stream: &[u8]) -> Vec<String> {
    events_of(&mut Demuxer::new(), stream)
}

fn acquired() -> String {
    described(&Event::SyncAcquired)
}

fn pack(bytes: &[u8]) -> String {
    described(&Event::PackHeader(PackHeader::read(bytes).unwrap()))
}

fn system_header(bytes: &[u8]) -> String {
    described(&Event::SystemHeader(SystemHeader::read(bytes).unwrap()))
}

fn pes_packet(bytes: &[u8]) -> String {
    described(&Event::PesPacket(PesPacket::read(bytes).unwrap()))
}

fn program_stream_map(bytes: &[u8]) -> String {
    described(&Event::ProgramStreamMap(
        ProgramStreamMap::read(bytes).unwrap(),
    ))
}

fn program_end() -> String {
    described(&Event::ProgramEnd)
}

fn unsynchronized(bytes: &[u8]) -> String {
    described(&Event::Unsynchronized { bytes })
}

fn unframable(bytes: &[u8]) -> String {
    described(&Event::Unframable { bytes })
}

fn truncated(bytes: &[u8]) -> String {
    described(&Event::Truncated { bytes })
}

fn error(error: Error) -> String {
    described(&Event::Error(error))
}

fn concat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

#[test]
fn acquires_sync_on_the_first_pack_header() {
    let pack_bytes = pack_header_bytes(&[]);
    assert_eq!(demux(&pack_bytes), [acquired(), pack(&pack_bytes)]);
}

#[test]
fn reports_leading_garbage_before_the_pack_start_code() {
    let pack_bytes = pack_header_bytes(&[]);
    let garbage = [0xFF, 0x12, 0x00, 0x34];
    assert_eq!(
        demux(&concat(&[&garbage, &pack_bytes])),
        [unsynchronized(&garbage), acquired(), pack(&pack_bytes)]
    );
}

#[test]
fn reports_a_system_header_after_the_pack() {
    let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
    assert_eq!(
        demux(&concat(&[&pack_bytes, &system])),
        [acquired(), pack(&pack_bytes), system_header(&system)]
    );
}

#[test]
fn absorbs_a_system_header_repeated_unchanged() {
    let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
    assert_eq!(
        demux(&concat(&[&pack_bytes, &system, &system])),
        [acquired(), pack(&pack_bytes), system_header(&system)]
    );
}

#[test]
fn reports_a_system_header_again_when_its_contents_change() {
    let pack_bytes = pack_header_bytes(&[]);
    let (first, second) = (system_header_bytes(), changed_system_header_bytes());
    assert_eq!(
        demux(&concat(&[&pack_bytes, &first, &second])),
        [
            acquired(),
            pack(&pack_bytes),
            system_header(&first),
            system_header(&second),
        ]
    );
}

#[test]
fn reports_consecutive_packs_without_reacquiring() {
    let pack_bytes = pack_header_bytes(&[]);
    assert_eq!(
        demux(&concat(&[&pack_bytes, &pack_bytes])),
        [acquired(), pack(&pack_bytes), pack(&pack_bytes)]
    );
}

#[test]
fn does_not_acquire_on_a_malformed_pack_header() {
    let (malformed, good) = (iso_11172_pack_bytes(), pack_header_bytes(&[]));
    assert_eq!(
        demux(&concat(&[&malformed, &good])),
        [unsynchronized(&malformed), acquired(), pack(&good)]
    );
}

#[test]
fn acquires_across_a_split_feed() {
    let pack_bytes = pack_header_bytes(&[]);
    let (first, second) = pack_bytes.split_at(7);
    let mut demuxer = Demuxer::new();
    assert_eq!(
        (
            events_of(&mut demuxer, first),
            events_of(&mut demuxer, second)
        ),
        (vec![], vec![acquired(), pack(&pack_bytes)])
    );
}

#[test]
fn discards_an_unframed_start_code_and_reports_the_next_pack() {
    let pack_bytes = pack_header_bytes(&[]);
    let unframed = [0x00, 0x00, 0x01, 0x01, 0xAA, 0xBB];
    assert_eq!(
        demux(&concat(&[&pack_bytes, &unframed, &pack_bytes])),
        [
            acquired(),
            pack(&pack_bytes),
            unframable(&unframed),
            pack(&pack_bytes),
        ]
    );
}

#[test]
fn discards_bytes_recovering_from_a_malformed_pack_while_synced() {
    let (good, malformed) = (pack_header_bytes(&[]), iso_11172_pack_bytes());
    assert_eq!(
        demux(&concat(&[&good, &malformed, &good])),
        [
            acquired(),
            pack(&good),
            error(Error::PackHeader {
                error: crate::ps::pack_header::Error::Iso11172Pack,
            }),
            unframable(&malformed),
            pack(&good),
        ]
    );
}

#[test]
fn discards_each_of_consecutive_unframable_start_codes() {
    let pack_bytes = pack_header_bytes(&[]);
    let slice = [0x00, 0x00, 0x01, 0x01, 0xAA, 0xBB];
    let picture = [0x00, 0x00, 0x01, 0x00, 0xCC, 0xDD];
    assert_eq!(
        demux(&concat(&[&pack_bytes, &slice, &picture, &pack_bytes])),
        [
            acquired(),
            pack(&pack_bytes),
            unframable(&slice),
            unframable(&picture),
            pack(&pack_bytes),
        ]
    );
}

#[test]
fn extracts_a_pes_packet_between_two_packs() {
    let pack_bytes = pack_header_bytes(&[]);
    let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
    assert_eq!(
        demux(&concat(&[&pack_bytes, &pes, &pack_bytes])),
        [
            acquired(),
            pack(&pack_bytes),
            pes_packet(&pes),
            pack(&pack_bytes),
        ]
    );
}

#[test]
fn waits_for_the_rest_of_a_pes_packet() {
    let pack_bytes = pack_header_bytes(&[]);
    let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
    let (head, tail) = pes.split_at(pes.len() - 2);
    let mut demuxer = Demuxer::new();
    assert_eq!(
        (
            events_of(&mut demuxer, &concat(&[&pack_bytes, head])),
            events_of(&mut demuxer, tail),
        ),
        (vec![acquired(), pack(&pack_bytes)], vec![pes_packet(&pes)])
    );
}

#[test]
fn discards_a_length_unbounded_pes_packet() {
    let pack_bytes = pack_header_bytes(&[]);
    let mut unbounded = pes_packet_bytes(0xE0, &[0xAA, 0xBB]);
    unbounded[4] = 0x00;
    unbounded[5] = 0x00;
    assert_eq!(
        demux(&concat(&[&pack_bytes, &unbounded, &pack_bytes])),
        [
            acquired(),
            pack(&pack_bytes),
            unframable(&unbounded),
            pack(&pack_bytes),
        ]
    );
}

#[test]
fn reports_the_program_end_code() {
    let pack_bytes = pack_header_bytes(&[]);
    assert_eq!(
        demux(&concat(&[&pack_bytes, &MPEG_PROGRAM_END_CODE])),
        [acquired(), pack(&pack_bytes), program_end()]
    );
}

#[test]
fn reacquires_sync_after_a_program_end_code() {
    let pack_bytes = pack_header_bytes(&[]);
    assert_eq!(
        demux(&concat(&[&pack_bytes, &MPEG_PROGRAM_END_CODE, &pack_bytes])),
        [
            acquired(),
            pack(&pack_bytes),
            program_end(),
            acquired(),
            pack(&pack_bytes),
        ]
    );
}

#[test]
fn re_reports_a_system_header_in_a_stream_following_a_program_end() {
    let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
    assert_eq!(
        demux(&concat(&[
            &pack_bytes,
            &system,
            &MPEG_PROGRAM_END_CODE,
            &pack_bytes,
            &system,
        ])),
        [
            acquired(),
            pack(&pack_bytes),
            system_header(&system),
            program_end(),
            acquired(),
            pack(&pack_bytes),
            system_header(&system),
        ]
    );
}

#[test]
fn waits_for_the_rest_of_a_system_header() {
    let (pack_bytes, system) = (pack_header_bytes(&[]), system_header_bytes());
    assert_eq!(
        demux(&concat(&[&pack_bytes, &system[..system.len() - 1]])),
        [acquired(), pack(&pack_bytes)]
    );
}

#[test]
fn reports_a_program_stream_map_after_the_pack() {
    let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
    assert_eq!(
        demux(&concat(&[&pack_bytes, &psm])),
        [acquired(), pack(&pack_bytes), program_stream_map(&psm)]
    );
}

#[test]
fn absorbs_a_program_stream_map_resent_at_the_same_version() {
    let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
    assert_eq!(
        demux(&concat(&[&pack_bytes, &psm, &psm])),
        [acquired(), pack(&pack_bytes), program_stream_map(&psm)]
    );
}

#[test]
fn reports_a_program_stream_map_again_when_its_version_changes() {
    let pack_bytes = pack_header_bytes(&[]);
    let (first, second) = (psm_bytes(0, true), psm_bytes(1, true));
    assert_eq!(
        demux(&concat(&[&pack_bytes, &first, &second])),
        [
            acquired(),
            pack(&pack_bytes),
            program_stream_map(&first),
            program_stream_map(&second),
        ]
    );
}

#[test]
fn absorbs_a_next_version_announcement() {
    let pack_bytes = pack_header_bytes(&[]);
    assert_eq!(
        demux(&concat(&[&pack_bytes, &psm_bytes(5, false)])),
        [acquired(), pack(&pack_bytes)]
    );
}

#[test]
fn re_reports_a_program_stream_map_in_a_stream_following_a_program_end() {
    let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
    assert_eq!(
        demux(&concat(&[
            &pack_bytes,
            &psm,
            &MPEG_PROGRAM_END_CODE,
            &pack_bytes,
            &psm,
        ])),
        [
            acquired(),
            pack(&pack_bytes),
            program_stream_map(&psm),
            program_end(),
            acquired(),
            pack(&pack_bytes),
            program_stream_map(&psm),
        ]
    );
}

#[test]
fn waits_for_the_rest_of_a_program_stream_map() {
    let (pack_bytes, psm) = (pack_header_bytes(&[]), psm_bytes(0, true));
    let (head, tail) = psm.split_at(psm.len() - 1);
    let mut demuxer = Demuxer::new();
    assert_eq!(
        (
            events_of(&mut demuxer, &concat(&[&pack_bytes, head])),
            events_of(&mut demuxer, tail),
        ),
        (
            vec![acquired(), pack(&pack_bytes)],
            vec![program_stream_map(&psm)]
        )
    );
}

#[test]
fn reports_a_pes_packet_cut_short_by_the_stream_end() {
    let pack_bytes = pack_header_bytes(&[]);
    let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
    let cut = &pes[..pes.len() - 2];
    let mut demuxer = Demuxer::new();
    let mut events = events_of(&mut demuxer, &concat(&[&pack_bytes, cut]));
    demuxer.finish();
    events.extend(drained(&mut demuxer));
    assert_eq!(events, [acquired(), pack(&pack_bytes), truncated(cut)]);
}

#[test]
fn reports_nothing_when_the_stream_ends_on_a_unit_boundary() {
    let pack_bytes = pack_header_bytes(&[]);
    let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
    let mut demuxer = Demuxer::new();
    demuxer.handle_input(&concat(&[&pack_bytes, &pes]));
    demuxer.finish();
    assert_eq!(
        drained(&mut demuxer),
        [acquired(), pack(&pack_bytes), pes_packet(&pes)]
    );
}

#[test]
fn reports_the_bytes_held_when_the_stream_ends_before_sync_is_acquired() {
    let pack_bytes = pack_header_bytes(&[]);
    let partial = &pack_bytes[..pack_bytes.len() - 1];
    let mut demuxer = Demuxer::new();
    demuxer.handle_input(partial);
    demuxer.finish();
    assert_eq!(drained(&mut demuxer), [unsynchronized(partial)]);
}

#[test]
fn keeps_synchronization_across_a_stream_end() {
    let pack_bytes = pack_header_bytes(&[]);
    let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
    let mut demuxer = Demuxer::new();
    demuxer.handle_input(&pack_bytes);
    demuxer.finish();
    let before = drained(&mut demuxer);
    assert_eq!(
        (before, events_of(&mut demuxer, &pes)),
        (vec![acquired(), pack(&pack_bytes)], vec![pes_packet(&pes)])
    );
}

#[test]
fn drops_the_bytes_it_reported_truncated() {
    let pack_bytes = pack_header_bytes(&[]);
    let pes = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
    let (first, rest) = pes.split_at(pes.len() - 5);
    let mut demuxer = Demuxer::new();
    demuxer.handle_input(&concat(&[&pack_bytes, first]));
    demuxer.finish();
    let before = drained(&mut demuxer);
    assert_eq!(
        (before, events_of(&mut demuxer, rest)),
        (
            vec![acquired(), pack(&pack_bytes), truncated(first)],
            vec![unframable(rest)]
        )
    );
}

#[test]
fn discards_a_corrupted_program_stream_map() {
    let pack_bytes = pack_header_bytes(&[]);
    let mut corrupted = psm_bytes(0, true);
    *corrupted.last_mut().unwrap() ^= 0xFF;
    assert_eq!(
        demux(&concat(&[&pack_bytes, &corrupted, &pack_bytes])),
        [
            acquired(),
            pack(&pack_bytes),
            error(Error::ProgramStreamMap {
                error: ProgramStreamMap::read(&corrupted).unwrap_err(),
            }),
            unframable(&corrupted),
            pack(&pack_bytes),
        ]
    );
}

#[test]
fn reports_why_a_pes_packet_that_does_not_read_was_passed_over() {
    let pack_bytes = pack_header_bytes(&[]);
    let mut broken = pes_packet_bytes(0xE0, &[0xAA, 0xBB, 0xCC]);
    broken[6] = 0x40;
    assert_eq!(
        demux(&concat(&[&pack_bytes, &broken, &pack_bytes])),
        [
            acquired(),
            pack(&pack_bytes),
            error(Error::PesPacket {
                error: PesPacket::read(&broken).unwrap_err(),
            }),
            unframable(&broken),
            pack(&pack_bytes),
        ]
    );
}

#[test]
fn bytes_that_cannot_begin_a_start_code_are_unframable_without_waiting_for_four() {
    let pack_bytes = pack_header_bytes(&[]);
    for tail in [&[0xFF][..], &[0xFF, 0x12, 0x34]] {
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&concat(&[&pack_bytes, tail]));
        demuxer.finish();
        assert_eq!(
            drained(&mut demuxer),
            [acquired(), pack(&pack_bytes), unframable(tail)],
            "{tail:02x?}"
        );
    }
}

#[test]
fn the_start_of_a_start_code_left_at_the_end_is_truncated() {
    let pack_bytes = pack_header_bytes(&[]);
    for tail in [&[0x00][..], &[0x00, 0x00], &[0x00, 0x00, 0x01]] {
        let mut demuxer = Demuxer::new();
        demuxer.handle_input(&concat(&[&pack_bytes, tail]));
        demuxer.finish();
        assert_eq!(
            drained(&mut demuxer),
            [acquired(), pack(&pack_bytes), truncated(tail)],
            "{tail:02x?}"
        );
    }
}

#[test]
fn an_iso_11172_pack_is_reported_without_waiting_for_the_length_of_a_13818_pack() {
    let pack_bytes = pack_header_bytes(&[]);
    let iso_11172 = [
        0x00, 0x00, 0x01, 0xBA, 0x21, 0x00, 0x01, 0x00, 0x01, 0x80, 0x00, 0x01,
    ];
    let mut demuxer = Demuxer::new();
    demuxer.handle_input(&concat(&[&pack_bytes, &iso_11172]));
    demuxer.finish();
    assert_eq!(
        drained(&mut demuxer),
        [
            acquired(),
            pack(&pack_bytes),
            error(Error::PackHeader {
                error: crate::ps::pack_header::Error::Iso11172Pack,
            }),
            unframable(&iso_11172),
        ]
    );
}
