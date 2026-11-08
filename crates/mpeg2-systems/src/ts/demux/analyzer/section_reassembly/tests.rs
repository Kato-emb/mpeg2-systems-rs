use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// A section of `table_id` whose `section_length` covers `body` exactly
///
/// The reassembler reads only `table_id` and `section_length`, so the body
/// stands for whatever the syntax and `CRC_32` would hold.
fn section(table_id: u8, body: &[u8]) -> Vec<u8> {
    framed_section(table_id, 0xB0, body)
}

/// A section whose byte after `table_id` is `flags` above `section_length`
fn framed_section(table_id: u8, flags: u8, body: &[u8]) -> Vec<u8> {
    let [high, low] = u16::try_from(body.len()).unwrap().to_be_bytes();
    let mut bytes = vec![table_id, flags | high, low];
    bytes.extend_from_slice(body);
    bytes
}

/// A payload starting a unit: `pointer_field`, then `tail`, then `rest`
fn unit_start(tail: &[u8], rest: &[u8]) -> Vec<u8> {
    let mut payload = vec![u8::try_from(tail.len()).unwrap()];
    payload.extend_from_slice(tail);
    payload.extend_from_slice(rest);
    payload
}

/// Pushes each payload in turn and returns the sections each completed
fn push_all(payloads: &[(bool, Vec<u8>)]) -> Vec<Vec<Vec<u8>>> {
    let mut reassembler = SectionReassembler::new();
    payloads
        .iter()
        .map(|(payload_unit_start_indicator, payload)| {
            let mut sections = Vec::new();
            reassembler.push(*payload_unit_start_indicator, payload, &mut |_, section| {
                sections.push(section.to_vec());
            });
            sections
        })
        .collect()
}

#[test]
fn two_sections_in_one_payload_are_both_completed() {
    let first = section(0x00, &[0xAA; 9]);
    let second = section(0x02, &[0xBB; 12]);
    let mut rest = first.clone();
    rest.extend_from_slice(&second);
    assert_eq!(
        push_all(&[(true, unit_start(&[], &rest))]),
        [vec![first, second]]
    );
}

#[test]
fn a_section_across_three_packets_is_completed_by_the_last() {
    let bytes = section(0x02, &(0..=255).collect::<Vec<u8>>());
    let (head, rest) = bytes.split_at(100);
    let (middle, end) = rest.split_at(100);
    assert_eq!(
        push_all(&[
            (true, unit_start(&[], head)),
            (false, middle.to_vec()),
            (false, end.to_vec()),
        ]),
        [vec![], vec![], vec![bytes.clone()]]
    );
}

#[test]
fn a_pointer_field_that_matches_the_section_in_flight_ends_it() {
    let first = section(0x02, &[0xAA; 30]);
    let second = section(0x02, &[0xBB; 5]);
    let (head, tail) = first.split_at(10);
    assert_eq!(
        push_all(&[
            (true, unit_start(&[], head)),
            (true, unit_start(tail, &second))
        ]),
        [vec![], vec![first, second]]
    );
}

#[test]
fn a_pointer_field_that_disagrees_with_the_section_in_flight_drops_it() {
    let first = section(0x02, &[0xAA; 30]);
    let second = section(0x02, &[0xBB; 5]);
    let (head, tail) = first.split_at(10);
    let too_long = [tail, &[0xCC, 0xCC]].concat();
    let too_short = tail.get(..5).unwrap();
    assert_eq!(
        push_all(&[
            (true, unit_start(&[], head)),
            (true, unit_start(&too_long, &second)),
        ]),
        [vec![], vec![first.clone(), second.clone()]]
    );
    assert_eq!(
        push_all(&[
            (true, unit_start(&[], head)),
            (true, unit_start(too_short, &second)),
        ]),
        [vec![], vec![second]]
    );
}

#[test]
fn bytes_after_a_stuffing_byte_are_not_read_as_sections() {
    let bytes = section(0x00, &[0xAA; 9]);
    let mut rest = bytes.clone();
    rest.extend_from_slice(&[0xFF; 20]);
    rest.extend_from_slice(&section(0x00, &[0xBB; 9]));
    assert_eq!(push_all(&[(true, unit_start(&[], &rest))]), [vec![bytes]]);
}

#[test]
fn a_section_longer_than_its_table_allows_is_dropped_until_the_next_start() {
    let oversized = [0x02, 0xB3, 0xFE, 0xAA, 0xAA];
    let next = section(0x02, &[0xBB; 5]);
    assert_eq!(
        push_all(&[
            (true, unit_start(&[], &oversized)),
            (false, vec![0xAA; 100]),
            (true, unit_start(&[], &next)),
        ]),
        [vec![], vec![], vec![next.clone()]]
    );
}

#[test]
fn a_short_form_section_is_completed_by_its_section_length() {
    let bytes = framed_section(0x80, 0x30, &[0xAA; 7]);
    assert_eq!(push_all(&[(true, unit_start(&[], &bytes))]), [vec![bytes]]);
}

#[test]
fn a_section_with_no_body_is_completed_by_its_prefix() {
    let empty = section(0x80, &[]);
    let next = section(0x80, &[0xAA; 3]);
    assert_eq!(
        push_all(&[(
            true,
            unit_start(&[], &[empty.clone(), next.clone()].concat())
        )]),
        [vec![empty, next]]
    );
}

#[test]
fn a_continuation_with_no_section_in_flight_is_ignored() {
    assert_eq!(
        push_all(&[(false, section(0x00, &[0xAA; 9]))]),
        [Vec::<Vec<u8>>::new()]
    );
}

#[test]
fn a_prefix_split_across_packets_is_joined() {
    let bytes = section(0x02, &[0xAA; 20]);
    let (head, rest) = bytes.split_at(2);
    assert_eq!(
        push_all(&[(true, unit_start(&[], head)), (false, rest.to_vec())]),
        [vec![], vec![bytes.clone()]]
    );
}

#[test]
fn a_section_inside_one_payload_is_not_copied() {
    let first = section(0x00, &[0xAA; 9]);
    let second = section(0x02, &[0xBB; 12]);
    let mut reassembler = SectionReassembler::new();
    let payload = unit_start(&[], &[first.clone(), second.clone()].concat());
    let mut lent = Vec::new();
    reassembler.push(true, &payload, &mut |completed, _| lent.push(completed));
    assert_eq!(
        (lent, reassembler.buffer.capacity()),
        (
            vec![
                Completed::InPayload(1),
                Completed::InPayload(1 + first.len()),
            ],
            0
        )
    );
}

#[test]
fn a_section_across_packets_is_lent_from_the_buffer_until_the_next_push() {
    let bytes = section(0x02, &[0xAA; 30]);
    let after = section(0x02, &[0xBB; 5]);
    let (head, tail) = bytes.split_at(10);
    let mut reassembler = SectionReassembler::new();
    reassembler.push(true, &unit_start(&[], head), &mut |_, _| {});
    let mut lent = Vec::new();
    reassembler.push(
        true,
        &unit_start(tail, &after),
        &mut |completed, section| {
            lent.push((completed, section.to_vec()));
        },
    );
    assert_eq!(
        lent,
        [
            (Completed::Reassembled(0), bytes.clone()),
            (Completed::InPayload(1 + tail.len()), after.clone()),
        ]
    );
    reassembler.push(false, &[], &mut |_, _| {});
    assert_eq!(reassembler, SectionReassembler::new());
}
