use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::pes::StreamId;
use crate::ts::ContinuityCounter;

/// A PID that carries no PSI, so nothing but the subscription makes it
/// reassembled
const PID: Pid = Pid::TSDT;

/// Large enough that no PES packet here reaches it, but in the tests that
/// set a limit of their own
const BUFFER_LIMIT: usize = 4096;

/// Input offsets of the units of three Transport Packets on the 188-byte
/// grid, none of them 0, so an offset that is always 0 fails
const FIRST_PACKET: u64 = 188;
const SECOND_PACKET: u64 = 376;
const THIRD_PACKET: u64 = 564;

/// Where each payload starts in the framer's buffer: not 0, so a range of
/// the framer's buffer that misses it fails
const PAYLOAD_START: usize = 1000;

/// A PES packet of `private_stream_2`, which has no optional header, with
/// `PES_packet_length` as given and `body` after it
fn pes(pes_packet_length: u16, body: &[u8]) -> Vec<u8> {
    let [high, low] = pes_packet_length.to_be_bytes();
    let mut bytes = vec![
        0x00,
        0x00,
        0x01,
        StreamId::PRIVATE_STREAM_2.get(),
        high,
        low,
    ];
    bytes.extend_from_slice(body);
    bytes
}

/// A queued event, with what it lends copied out
#[derive(Debug, Clone, PartialEq, Eq)]
enum Seen {
    /// A PES packet that ended, its bytes lent from the framer's buffer or
    /// from the reassembly buffer
    Ended {
        bytes: Vec<u8>,
        from_packet: bool,
        start_offset: u64,
    },
    Truncated {
        bytes: Vec<u8>,
        start_offset: u64,
    },
    Overrun {
        discarded_bytes: usize,
    },
    /// An event this test does not expect, so the comparison fails on it
    Unexpected(String),
}

/// A packet of [`PID`] to push: whether it starts a unit, the verdict of
/// its counter, its payload, and its input offset
struct Arrival<'payload> {
    payload_unit_start_indicator: bool,
    verdict: Verdict,
    payload: &'payload [u8],
    offset: u64,
}

const fn start(payload: &[u8], offset: u64) -> Arrival<'_> {
    Arrival {
        payload_unit_start_indicator: true,
        verdict: Verdict::InOrder,
        payload,
        offset,
    }
}

const fn continuation(payload: &[u8], offset: u64) -> Arrival<'_> {
    Arrival {
        payload_unit_start_indicator: false,
        verdict: Verdict::InOrder,
        payload,
        offset,
    }
}

fn subscribed(buffer_limit: usize) -> PesReassembly {
    let mut reassembly = PesReassembly::new(buffer_limit);
    reassembly.subscribe(PID);
    reassembly
}

/// Resolves the queued events, while what they lend is still lent
fn seen(reassembly: &PesReassembly, payload: &[u8], pending: VecDeque<Pending>) -> Vec<Seen> {
    pending
        .into_iter()
        .map(|pending| match pending {
            Pending::PesPacket {
                pid,
                bytes: Lent::Packet(range),
                start_offset,
            } => {
                assert_eq!(pid, PID);
                let range = range.start.checked_sub(PAYLOAD_START).unwrap()
                    ..range.end.checked_sub(PAYLOAD_START).unwrap();
                Seen::Ended {
                    bytes: payload.get(range).unwrap().to_vec(),
                    from_packet: true,
                    start_offset,
                }
            }
            Pending::PesPacket {
                pid,
                bytes: Lent::Reassembled(range),
                start_offset,
            } => Seen::Ended {
                bytes: reassembly.bytes(pid, range).to_vec(),
                from_packet: false,
                start_offset,
            },
            Pending::TruncatedPesPacket {
                pid,
                bytes,
                start_offset,
            } => Seen::Truncated {
                bytes: reassembly.bytes(pid, bytes).to_vec(),
                start_offset,
            },
            Pending::Ready(Event::Error(Error::PesBufferOverrun {
                pid,
                discarded_bytes,
            })) => {
                assert_eq!(pid, PID);
                Seen::Overrun { discarded_bytes }
            }
            other @ (Pending::Ready(_)
            | Pending::Unsynchronized(_)
            | Pending::Section { .. }
            | Pending::ScrambledPesPacket { .. }) => Seen::Unexpected(format!("{other:?}")),
        })
        .collect()
}

/// Pushes a packet of [`PID`] and returns the events it queued
fn push(reassembly: &mut PesReassembly, arrival: &Arrival<'_>) -> Vec<Seen> {
    let packet = TransportPacket {
        payload_unit_start_indicator: arrival.payload_unit_start_indicator,
        payload: Some(arrival.payload),
        ..TransportPacket::new(PID, ContinuityCounter::try_from(0).unwrap())
    };
    let mut pending = VecDeque::new();
    reassembly.push(
        &packet,
        arrival.verdict.into(),
        PAYLOAD_START,
        arrival.offset,
        &mut pending,
    );
    seen(reassembly, arrival.payload, pending)
}

/// Pushes each packet in turn and returns the events each queued
fn push_all(reassembly: &mut PesReassembly, arrivals: &[Arrival<'_>]) -> Vec<Vec<Seen>> {
    arrivals
        .iter()
        .map(|arrival| push(reassembly, arrival))
        .collect()
}

fn finish(reassembly: &mut PesReassembly) -> Vec<Seen> {
    let mut pending = VecDeque::new();
    reassembly.finish(&mut pending);
    seen(reassembly, &[], pending)
}

fn ended(bytes: Vec<u8>, start_offset: u64) -> Seen {
    Seen::Ended {
        bytes,
        from_packet: false,
        start_offset,
    }
}

#[test]
fn a_pes_packet_inside_one_payload_is_lent_from_the_packet() {
    let packet = pes(3, &[0xAA, 0xBB, 0xCC]);
    assert_eq!(
        push(&mut subscribed(BUFFER_LIMIT), &start(&packet, FIRST_PACKET)),
        [Seen::Ended {
            bytes: packet.clone(),
            from_packet: true,
            start_offset: FIRST_PACKET,
        }]
    );
}

#[test]
fn a_pes_packet_of_a_declared_length_across_payloads_ends_on_the_last() {
    let packet = pes(20, &(0..20).collect::<Vec<u8>>());
    let (head, tail) = packet.split_at(8);
    assert_eq!(
        push_all(
            &mut subscribed(BUFFER_LIMIT),
            &[start(head, FIRST_PACKET), continuation(tail, SECOND_PACKET)]
        ),
        [vec![], vec![ended(packet.clone(), FIRST_PACKET)]]
    );
}

#[test]
fn the_bytes_after_a_pes_packet_that_ends_are_stuffing() {
    let packet = pes(2, &[0x77, 0x88]);
    let (head, tail) = packet.split_at(3);
    let mut stuffed = tail.to_vec();
    stuffed.extend_from_slice(&[0xFF; 5]);
    assert_eq!(
        push_all(
            &mut subscribed(BUFFER_LIMIT),
            &[
                start(head, FIRST_PACKET),
                continuation(&stuffed, SECOND_PACKET),
                continuation(&[0x01, 0x02], THIRD_PACKET),
            ]
        ),
        [vec![], vec![ended(packet.clone(), FIRST_PACKET)], vec![]]
    );
}

#[test]
fn a_pes_packet_of_an_unspecified_length_ends_at_the_next_unit_start() {
    let first = pes(0, &[0x11, 0x22, 0x33]);
    let second = pes(0, &[0x44]);
    assert_eq!(
        push_all(
            &mut subscribed(BUFFER_LIMIT),
            &[
                start(&first, FIRST_PACKET),
                continuation(&[0x55], SECOND_PACKET),
                start(&second, THIRD_PACKET),
            ]
        ),
        [
            vec![],
            vec![],
            vec![ended([first, vec![0x55]].concat(), FIRST_PACKET)]
        ]
    );
}

#[test]
fn a_unit_start_ends_a_pes_packet_short_of_its_declared_length_with_the_bytes_gathered() {
    let short = pes(10, &[0x01, 0x02, 0x03]);
    let next = pes(1, &[0xFF]);
    assert_eq!(
        push_all(
            &mut subscribed(BUFFER_LIMIT),
            &[start(&short, FIRST_PACKET), start(&next, SECOND_PACKET)]
        ),
        [
            vec![],
            vec![
                ended(short.clone(), FIRST_PACKET),
                Seen::Ended {
                    bytes: next.clone(),
                    from_packet: true,
                    start_offset: SECOND_PACKET,
                }
            ]
        ]
    );
}

#[test]
fn a_continuation_with_no_pes_packet_in_flight_is_passed_over() {
    let mut reassembly = subscribed(BUFFER_LIMIT);
    assert_eq!(
        push_all(
            &mut reassembly,
            &[
                continuation(&[0x00, 0x00, 0x01, 0xBF], FIRST_PACKET),
                continuation(&[0x00, 0x00], SECOND_PACKET),
            ]
        ),
        [vec![], vec![]]
    );
    assert_eq!(finish(&mut reassembly), []);
}

#[test]
fn a_packet_of_a_pid_not_subscribed_is_passed_over() {
    let mut reassembly = PesReassembly::new(BUFFER_LIMIT);
    assert_eq!(
        push(&mut reassembly, &start(&pes(1, &[0x00]), FIRST_PACKET)),
        []
    );
}

#[test]
fn a_gap_in_the_counter_ends_the_pes_packet_in_flight_as_truncated() {
    let head = pes(20, &[0x01, 0x02, 0x03]);
    let next = pes(1, &[0x0A]);
    for verdict in [
        Verdict::Broken {
            expected: ContinuityCounter::try_from(1).unwrap(),
            found: ContinuityCounter::try_from(2).unwrap(),
        },
        Verdict::Excused,
    ] {
        let mut reassembly = subscribed(BUFFER_LIMIT);
        assert_eq!(
            push_all(
                &mut reassembly,
                &[
                    start(&head, FIRST_PACKET),
                    Arrival {
                        verdict,
                        ..continuation(&[0x04, 0x05], SECOND_PACKET)
                    },
                    Arrival {
                        verdict,
                        ..start(&next, THIRD_PACKET)
                    },
                ]
            ),
            [
                vec![],
                vec![Seen::Truncated {
                    bytes: head.clone(),
                    start_offset: FIRST_PACKET,
                }],
                vec![Seen::Ended {
                    bytes: next.clone(),
                    from_packet: true,
                    start_offset: THIRD_PACKET,
                }]
            ],
            "{verdict:?}"
        );
    }
}

#[test]
fn a_duplicate_packet_adds_nothing_to_the_pes_packet_in_flight() {
    let packet = pes(4, &[0x01, 0x02, 0x03, 0x04]);
    let (head, tail) = packet.split_at(8);
    assert_eq!(
        push_all(
            &mut subscribed(BUFFER_LIMIT),
            &[
                start(head, FIRST_PACKET),
                Arrival {
                    verdict: Verdict::Duplicate,
                    ..start(head, SECOND_PACKET)
                },
                continuation(tail, THIRD_PACKET),
            ]
        ),
        [vec![], vec![], vec![ended(packet.clone(), FIRST_PACKET)]]
    );
}

#[test]
fn a_pes_packet_that_ended_stays_lent_until_the_next_packet_of_its_pid() {
    let first = pes(0, &[0x11; 30]);
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(&first, FIRST_PACKET));
    let packet = TransportPacket {
        payload_unit_start_indicator: true,
        payload: Some(&[0x00, 0x00, 0x01, 0xBF, 0x00, 0x00, 0x22]),
        ..TransportPacket::new(PID, ContinuityCounter::try_from(1).unwrap())
    };
    let mut pending = VecDeque::new();
    reassembly.push(
        &packet,
        Sequence::InOrder,
        PAYLOAD_START,
        SECOND_PACKET,
        &mut pending,
    );
    let range = pending
        .into_iter()
        .find_map(|pending| {
            if let Pending::PesPacket {
                bytes: Lent::Reassembled(range),
                ..
            } = pending
            {
                Some(range)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(reassembly.bytes(PID, range.clone()), first);
    push(&mut reassembly, &continuation(&[0x33; 40], THIRD_PACKET));
    assert_ne!(reassembly.bytes(PID, range), first);
}

#[test]
fn unsubscribing_drops_the_pes_packet_in_flight_and_what_follows_it() {
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(&pes(20, &[0x01; 8]), FIRST_PACKET));
    reassembly.unsubscribe(PID, &mut VecDeque::new());
    assert_eq!(
        (
            push(&mut reassembly, &continuation(&[0x02; 12], SECOND_PACKET)),
            push(&mut reassembly, &start(&pes(1, &[0x0A]), THIRD_PACKET)),
            finish(&mut reassembly),
        ),
        (vec![], vec![], vec![])
    );
}

#[test]
fn unsubscribing_a_pid_not_subscribed_changes_nothing() {
    let packet = pes(1, &[0x0A]);
    let mut reassembly = PesReassembly::new(BUFFER_LIMIT);
    reassembly.unsubscribe(PID, &mut VecDeque::new());
    reassembly.subscribe(PID);
    assert_eq!(
        push(&mut reassembly, &start(&packet, FIRST_PACKET)),
        [Seen::Ended {
            bytes: packet.clone(),
            from_packet: true,
            start_offset: FIRST_PACKET,
        }]
    );
}

#[test]
fn subscribing_again_keeps_the_pes_packet_in_flight() {
    let packet = pes(20, &(0..20).collect::<Vec<u8>>());
    let (head, tail) = packet.split_at(8);
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(head, FIRST_PACKET));
    reassembly.subscribe(PID);
    assert_eq!(
        push(&mut reassembly, &continuation(tail, SECOND_PACKET)),
        [ended(packet.clone(), FIRST_PACKET)]
    );
}

#[test]
fn subscribing_after_unsubscribing_reassembles_from_the_next_unit_start() {
    let packet = pes(20, &(0..20).collect::<Vec<u8>>());
    let (head, tail) = packet.split_at(8);
    let next = pes(2, &[0x0A, 0x0B]);
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(head, FIRST_PACKET));
    reassembly.unsubscribe(PID, &mut VecDeque::new());
    reassembly.subscribe(PID);
    assert_eq!(
        push_all(
            &mut reassembly,
            &[
                continuation(tail, SECOND_PACKET),
                start(&next, THIRD_PACKET)
            ]
        ),
        [
            vec![],
            vec![Seen::Ended {
                bytes: next.clone(),
                from_packet: true,
                start_offset: THIRD_PACKET,
            }]
        ]
    );
}

#[test]
fn the_end_of_input_ends_a_pes_packet_of_an_unspecified_length() {
    let packet = pes(0, &[0x0A, 0x0B, 0x0C]);
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(&packet, FIRST_PACKET));
    assert_eq!(finish(&mut reassembly), [ended(packet, FIRST_PACKET)]);
}

#[test]
fn the_end_of_input_truncates_a_pes_packet_short_of_its_declared_length() {
    let head = pes(20, &[0x01, 0x02, 0x03]);
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(&head, FIRST_PACKET));
    assert_eq!(
        finish(&mut reassembly),
        [Seen::Truncated {
            bytes: head,
            start_offset: FIRST_PACKET,
        }]
    );
}

#[test]
fn the_end_of_input_truncates_a_pes_packet_too_short_to_tell_its_length() {
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(&[0x00, 0x00, 0x01], FIRST_PACKET));
    assert_eq!(
        finish(&mut reassembly),
        [Seen::Truncated {
            bytes: vec![0x00, 0x00, 0x01],
            start_offset: FIRST_PACKET,
        }]
    );
}

#[test]
fn the_end_of_input_keeps_the_subscription() {
    let packet = pes(1, &[0x09]);
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(&pes(0, &[0x01]), FIRST_PACKET));
    finish(&mut reassembly);
    assert_eq!(
        push(&mut reassembly, &start(&packet, SECOND_PACKET)),
        [Seen::Ended {
            bytes: packet.clone(),
            from_packet: true,
            start_offset: SECOND_PACKET,
        }]
    );
}

#[test]
fn losing_sync_drops_the_pes_packet_in_flight_and_keeps_the_subscription() {
    let packet = pes(1, &[0x09]);
    let mut reassembly = subscribed(BUFFER_LIMIT);
    push(&mut reassembly, &start(&pes(20, &[0x01]), FIRST_PACKET));
    reassembly.discard_in_flight();
    assert_eq!(
        (
            push(&mut reassembly, &continuation(&[0x02; 13], SECOND_PACKET)),
            push(&mut reassembly, &start(&packet, THIRD_PACKET)),
        ),
        (
            vec![],
            vec![Seen::Ended {
                bytes: packet.clone(),
                from_packet: true,
                start_offset: THIRD_PACKET,
            }]
        )
    );
}

#[test]
fn a_pes_packet_of_an_unspecified_length_grown_past_the_limit_is_dropped() {
    let mut reassembly = subscribed(8);
    assert_eq!(
        push_all(
            &mut reassembly,
            &[
                start(&pes(0, &[0x01]), FIRST_PACKET),
                continuation(&[0x02, 0x03], SECOND_PACKET),
            ]
        ),
        [vec![], vec![Seen::Overrun { discarded_bytes: 9 }]]
    );
}

#[test]
fn a_declared_length_past_the_limit_is_dropped_before_it_is_in() {
    assert_eq!(
        push(
            &mut subscribed(8),
            &start(
                &pes(100, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
                FIRST_PACKET
            )
        ),
        [Seen::Overrun {
            discarded_bytes: 12
        }]
    );
}

#[test]
fn a_pes_packet_whose_declared_length_is_in_ends_though_past_the_limit() {
    let packet = pes(6, &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    let (head, tail) = packet.split_at(7);
    assert_eq!(
        push_all(
            &mut subscribed(8),
            &[start(head, FIRST_PACKET), continuation(tail, SECOND_PACKET)]
        ),
        [vec![], vec![ended(packet.clone(), FIRST_PACKET)]]
    );
}

#[test]
fn reassembly_starts_again_at_the_next_unit_start_after_an_overrun() {
    let packet = pes(2, &[0x0A, 0x0B]);
    assert_eq!(
        push_all(
            &mut subscribed(8),
            &[
                start(&pes(0, &[0x01]), FIRST_PACKET),
                continuation(&[0x02, 0x03], SECOND_PACKET),
                continuation(&[0x04], SECOND_PACKET),
                start(&packet, THIRD_PACKET),
            ]
        ),
        [
            vec![],
            vec![Seen::Overrun { discarded_bytes: 9 }],
            vec![],
            vec![Seen::Ended {
                bytes: packet.clone(),
                from_packet: true,
                start_offset: THIRD_PACKET,
            }]
        ]
    );
}
