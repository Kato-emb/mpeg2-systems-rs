//! Reassembly of the PES packets a subscribed PID carries across Transport
//! Packets

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::ops::Range;

use super::continuity::Verdict;
use crate::pes::{self, EncodedLen};
use crate::ts::Pid;
use crate::ts::demux::pending::{Lent, Pending};
use crate::ts::demux::{Error, Event};
use crate::ts::packet::{PACKET_LENGTH, TransportPacket, TransportScramblingControl};

/// Where a Transport Packet stands in its PID's sequence, as far as the PES
/// packet in flight cares
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Sequence {
    /// The packet follows the one before, or carries no counter to check
    InOrder,
    /// The one duplicate §2.4.3.3 allows: its payload repeats bytes already
    /// gathered
    Duplicate,
    /// Packets were lost, or `discontinuity_indicator` excused a jump
    Gap,
    /// `transport_error_indicator` is set: nothing in the packet is reliable
    Errored,
}

impl From<Verdict> for Sequence {
    fn from(verdict: Verdict) -> Self {
        match verdict {
            Verdict::InOrder => Self::InOrder,
            Verdict::Duplicate => Self::Duplicate,
            Verdict::Broken { .. } | Verdict::Excused => Self::Gap,
        }
    }
}

/// What a Transport Packet means for the PES packet in flight on its PID
#[derive(Debug, Clone, Copy)]
enum Observation {
    /// A duplicate, whose payload is already gathered
    Repeated,
    /// A gap, or a packet that cannot be trusted, with no
    /// `payload_unit_start_indicator` after it to start a PES packet from
    Interrupted,
    /// A continuation with no PES packet in flight: nothing in the payload
    /// says where a PES packet starts
    Stray,
    /// `payload_unit_start_indicator` is set on a packet in order: a new PES
    /// packet starts in the payload
    Starts(Growth),
    /// `payload_unit_start_indicator` is set on a packet after a gap
    StartsAfterGap(Growth),
    /// The payload continues the PES packet in flight
    Continues(Growth),
}

/// What a payload does to the PES packet it belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Growth {
    /// The PES packet ends this many bytes into the payload
    Ends { length: usize },
    /// The whole payload belongs to the PES packet, which goes on within
    /// the limit
    Gathers,
    /// The whole payload belongs to the PES packet, which it takes past the
    /// limit
    Overruns,
}

/// The PIDs subscribed to, and the PES packet in flight on each
#[derive(Debug, Clone)]
pub(super) struct PesReassembly {
    buffer_limit: usize,
    reassemblers: BTreeMap<Pid, PesReassembler>,
}

/// The PES packets of one subscribed PID
///
/// `buffer[..in_flight_start]` holds the PES packets the latest packet of
/// the PID ended, which stay lent until the next packet of the PID;
/// `buffer[in_flight_start..]` is the PES packet in flight.
#[derive(Debug, Clone)]
struct PesReassembler {
    buffer: Vec<u8>,
    in_flight_start: usize,
    /// Input offset of the unit of the Transport Packet the PES packet in
    /// flight started in
    start_offset: u64,
    /// `transport_scrambling_control` of the Transport Packet the PES packet
    /// in flight started in
    // Why not read it from the packets that follow: the packets of one PES
    // packet need not agree, and the one that starts it decides whether its
    // header can be read.
    transport_scrambling_control: TransportScramblingControl,
}

impl PesReassembly {
    pub(super) const fn new(buffer_limit: usize) -> Self {
        Self {
            buffer_limit,
            reassemblers: BTreeMap::new(),
        }
    }

    /// Reassembles the PES packets of `pid` from its next
    /// `payload_unit_start_indicator`, keeping the PES packet in flight of a
    /// PID already subscribed
    pub(super) fn subscribe(&mut self, pid: Pid) {
        self.reassemblers
            .entry(pid)
            .or_insert_with(|| PesReassembler {
                buffer: Vec::new(),
                in_flight_start: 0,
                start_offset: 0,
                transport_scrambling_control: TransportScramblingControl::NOT_SCRAMBLED,
            });
    }

    /// Drops the PID's subscription, every byte it gathered, and the queued
    /// events of its PES packets
    pub(super) fn unsubscribe(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        // Why no event for the PES packet in flight: the caller that left
        // the PID has no use for it, and reporting it would oblige the caller
        // to keep filtering events for a PID it has just left. Why drop the
        // queued ones too: they lend the buffer dropped here.
        self.reassemblers.remove(&pid);
        pending.retain(|pending| match pending {
            Pending::PesPacket { pid: queued, .. }
            | Pending::ScrambledPesPacket { pid: queued, .. }
            | Pending::TruncatedPesPacket { pid: queued, .. }
            | Pending::Ready(Event::Error(Error::PesBufferOverrun { pid: queued, .. })) => {
                *queued != pid
            }
            Pending::Ready(_) | Pending::Unsynchronized(_) | Pending::Section { .. } => true,
        });
    }

    /// Returns the bytes of a range the queue holds for `pid`
    pub(super) fn bytes(&self, pid: Pid, range: Range<usize>) -> &[u8] {
        self.reassemblers
            .get(&pid)
            .and_then(|reassembler| reassembler.buffer.get(range))
            .unwrap_or_default()
    }

    /// Drops the PES packet in flight on every PID, keeping the
    /// subscriptions
    pub(super) fn discard_in_flight(&mut self) {
        // Why not drop the subscriptions: a gap in the input ends the bytes
        // in flight, not the caller's interest in the PID. Why not report the
        // bytes as truncated, as a continuity break does: Error::SyncLost
        // already says every PID lost what was in flight.
        for reassembler in self.reassemblers.values_mut() {
            reassembler.buffer.truncate(reassembler.in_flight_start);
        }
    }

    /// Ends the PES packet in flight on every PID at the end of input, and
    /// queues it
    pub(super) fn finish(&mut self, pending: &mut VecDeque<Pending>) {
        for (&pid, reassembler) in &mut self.reassemblers {
            match length_of(reassembler.in_flight(), reassembler.is_scrambled()) {
                EncodedLen::Unbounded => reassembler.close_ended(pid, pending),
                // Why not read a packet of a declared length: it ends the
                // moment its bytes are in, so one still in flight is short of
                // its length.
                EncodedLen::Bounded(_) | EncodedLen::NeedMoreData => {
                    reassembler.close_truncated(pid, pending);
                }
            }
        }
    }

    /// Takes a packet, which `sequence` places in its PID's sequence, whose
    /// payload starts at `payload_start` in the framer's buffer and whose
    /// unit starts at input `offset`, and queues the PES packets it ends, if
    /// its PID is subscribed
    pub(super) fn push(
        &mut self,
        packet: &TransportPacket<'_>,
        sequence: Sequence,
        payload_start: usize,
        offset: u64,
        pending: &mut VecDeque<Pending>,
    ) {
        let pid = packet.pid;
        let Some(reassembler) = self.reassemblers.get_mut(&pid) else {
            return;
        };
        let payload = packet.payload.unwrap_or_default();
        match observe(reassembler, sequence, packet, self.buffer_limit) {
            // Why not reassemble a duplicate, nor end what it would lend:
            // its payload repeats bytes already gathered.
            Observation::Repeated => {}
            // Why cut the PES packet in flight on a packet that cannot be
            // read: its payload may belong to it or not, so the bytes after
            // it would be stitched across a gap.
            Observation::Interrupted => {
                reassembler.lend_no_more();
                reassembler.close_truncated(pid, pending);
            }
            Observation::Stray => reassembler.lend_no_more(),
            Observation::Starts(growth) => {
                reassembler.lend_no_more();
                reassembler.close_ended(pid, pending);
                reassembler.start(pid, packet, growth, (payload_start, offset), pending);
            }
            Observation::StartsAfterGap(growth) => {
                reassembler.lend_no_more();
                reassembler.close_truncated(pid, pending);
                reassembler.start(pid, packet, growth, (payload_start, offset), pending);
            }
            Observation::Continues(growth) => {
                reassembler.lend_no_more();
                match growth {
                    // Why drop the bytes after the end: a PES packet starts
                    // only at a payload_unit_start_indicator, so the bytes
                    // after one that ends inside a Transport Packet are
                    // stuffing.
                    Growth::Ends { length } => {
                        reassembler
                            .buffer
                            .extend_from_slice(payload.get(..length).unwrap_or_default());
                        reassembler.close_ended(pid, pending);
                    }
                    Growth::Gathers => reassembler.buffer.extend_from_slice(payload),
                    Growth::Overruns => reassembler.overrun(pid, payload, pending),
                }
            }
        }
        debug_assert!(
            reassembler.buffer.len() <= self.buffer_limit.saturating_add(PACKET_LENGTH),
            "the PES reassembly buffer of a PID outgrew the limit by more than a packet"
        );
    }
}

impl PesReassembler {
    fn in_flight(&self) -> &[u8] {
        self.buffer.get(self.in_flight_start..).unwrap_or_default()
    }

    /// Drops the PES packets the previous packet of the PID ended, which no
    /// queued event lends any more
    fn lend_no_more(&mut self) {
        self.buffer.drain(..self.in_flight_start);
        self.in_flight_start = 0;
    }

    /// Ends the PES packet in flight, and returns the range of the buffer it
    /// is lent from, or `None` if none is in flight
    fn close(&mut self) -> Option<Range<usize>> {
        let start = self.in_flight_start;
        self.in_flight_start = self.buffer.len();
        (start < self.in_flight_start).then_some(start..self.in_flight_start)
    }

    /// Starts a PES packet in the payload of `packet`, which starts at
    /// `payload_start` in the framer's buffer and whose unit starts at input
    /// `offset`
    fn start(
        &mut self,
        pid: Pid,
        packet: &TransportPacket<'_>,
        growth: Growth,
        (payload_start, offset): (usize, u64),
        pending: &mut VecDeque<Pending>,
    ) {
        let payload = packet.payload.unwrap_or_default();
        self.start_offset = offset;
        self.transport_scrambling_control = packet.transport_scrambling_control;
        match growth {
            // Why not copy a PES packet that lies inside one packet: it can
            // be lent from the packet, and is copied only to cross packets.
            Growth::Ends { length } => pending.push_back(Pending::PesPacket {
                pid,
                bytes: Lent::Packet(payload_start..payload_start.saturating_add(length)),
                start_offset: offset,
            }),
            Growth::Gathers => self.buffer.extend_from_slice(payload),
            Growth::Overruns => self.overrun(pid, payload, pending),
        }
    }

    fn is_scrambled(&self) -> bool {
        self.transport_scrambling_control != TransportScramblingControl::NOT_SCRAMBLED
    }

    /// Ends the PES packet in flight, if any, and queues it to be read, or
    /// as scrambled
    fn close_ended(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        let Some(bytes) = self.close() else {
            return;
        };
        pending.push_back(if self.is_scrambled() {
            Pending::ScrambledPesPacket {
                pid,
                bytes,
                transport_scrambling_control: self.transport_scrambling_control,
                start_offset: self.start_offset,
            }
        } else {
            Pending::PesPacket {
                pid,
                bytes: Lent::Reassembled(bytes),
                start_offset: self.start_offset,
            }
        });
    }

    /// Drops the PES packet in flight, which `payload` would take past the
    /// limit, and queues the overrun
    fn overrun(&mut self, pid: Pid, payload: &[u8], pending: &mut VecDeque<Pending>) {
        let discarded_bytes = self.in_flight().len().saturating_add(payload.len());
        self.buffer.truncate(self.in_flight_start);
        pending.push_back(Pending::Ready(Event::Error(Error::PesBufferOverrun {
            pid,
            discarded_bytes,
        })));
    }

    /// Ends the PES packet in flight, if any, and queues it as cut short
    fn close_truncated(&mut self, pid: Pid, pending: &mut VecDeque<Pending>) {
        if let Some(bytes) = self.close() {
            pending.push_back(Pending::TruncatedPesPacket {
                pid,
                bytes,
                start_offset: self.start_offset,
            });
        }
    }
}

/// Classifies a packet of a subscribed PID against the PES packet in flight
fn observe(
    reassembler: &PesReassembler,
    sequence: Sequence,
    packet: &TransportPacket<'_>,
    buffer_limit: usize,
) -> Observation {
    let payload = packet.payload.unwrap_or_default();
    let start = || {
        growth(
            &[],
            packet.transport_scrambling_control != TransportScramblingControl::NOT_SCRAMBLED,
            payload,
            buffer_limit,
        )
    };
    match (sequence, packet.payload_unit_start_indicator) {
        (Sequence::Duplicate, _) => Observation::Repeated,
        (Sequence::Errored, _) | (Sequence::Gap, false) => Observation::Interrupted,
        (Sequence::InOrder, true) => Observation::Starts(start()),
        (Sequence::Gap, true) => Observation::StartsAfterGap(start()),
        (Sequence::InOrder, false) if reassembler.in_flight().is_empty() => Observation::Stray,
        (Sequence::InOrder, false) => Observation::Continues(growth(
            reassembler.in_flight(),
            reassembler.is_scrambled(),
            payload,
            buffer_limit,
        )),
    }
}

/// Classifies `payload` against `in_flight`, the PES packet it belongs to,
/// whose length is read unless it is `scrambled`
fn growth(in_flight: &[u8], scrambled: bool, payload: &[u8], buffer_limit: usize) -> Growth {
    // Why copy the prefix only while the packet in flight is shorter than
    // it: once its first bytes are gathered, they alone give the length.
    let mut joined = [0; pes::PREFIX_LENGTH];
    let prefix = if in_flight.len() >= pes::PREFIX_LENGTH {
        in_flight
    } else {
        let copied = joined
            .iter_mut()
            .zip(in_flight.iter().chain(payload))
            .map(|(slot, byte)| *slot = *byte)
            .count();
        joined.get(..copied).unwrap_or_default()
    };
    let gathered = in_flight.len().saturating_add(payload.len());
    match length_of(prefix, scrambled) {
        // Why end before checking the limit: a PES packet whose declared
        // length is all in hand is whole, and dropping it as an overrun would
        // lose it for nothing.
        EncodedLen::Bounded(length) if length <= gathered => Growth::Ends {
            length: length.saturating_sub(in_flight.len()),
        },
        EncodedLen::Bounded(_) | EncodedLen::Unbounded | EncodedLen::NeedMoreData
            if gathered > buffer_limit =>
        {
            Growth::Overruns
        }
        EncodedLen::Bounded(_) | EncodedLen::Unbounded | EncodedLen::NeedMoreData => {
            Growth::Gathers
        }
    }
}

/// Returns how long the PES packet that `prefix` starts is, unread when
/// its Transport Packets are `scrambled`
fn length_of(prefix: &[u8], scrambled: bool) -> EncodedLen {
    // Why not read the length of a packet in scrambled Transport Packets:
    // PES_packet_length is scrambled with the rest, so the packet runs to
    // the next payload_unit_start_indicator, bounded by the limit.
    if scrambled {
        EncodedLen::Unbounded
    } else {
        pes::encoded_len(prefix)
    }
}

#[cfg(test)]
mod tests {
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
}
