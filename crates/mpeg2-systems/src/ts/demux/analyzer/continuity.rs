//! Tracking of `continuity_counter` per PID

use alloc::collections::BTreeMap;

use crate::ts::packet::TransportPacket;
use crate::ts::{ContinuityCounter, Pid};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PidContinuity {
    counter: ContinuityCounter,
    duplicate_tolerated: bool,
}

/// How a packet's `continuity_counter` relates to the PID's history
enum Observation {
    NotCountable,
    FirstSeen,
    Excused,
    ToleratedDuplicate,
    StuckDuplicate { expected: ContinuityCounter },
    Continuous,
    Discontinuous { expected: ContinuityCounter },
}

/// What one packet's `continuity_counter` means for the packets of its PID
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// The counter continues the sequence, or the packet carries no counter
    /// to check
    InOrder,
    /// The one duplicate §2.4.3.3 allows: its payload repeats bytes already
    /// delivered
    Duplicate,
    /// `discontinuity_indicator` excused a jump: what was gathered on the PID
    /// before it belongs to another stream
    Excused,
    /// The counter is not the one expected
    Broken {
        /// The counter a continuous stream would carry
        expected: ContinuityCounter,
        /// The counter found
        found: ContinuityCounter,
    },
}

/// The last `continuity_counter` of each PID since the last reset
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct ContinuityTracker {
    last: BTreeMap<Pid, PidContinuity>,
}

impl ContinuityTracker {
    pub(super) const fn new() -> Self {
        Self {
            last: BTreeMap::new(),
        }
    }

    pub(super) fn reset(&mut self) {
        self.last.clear();
    }

    /// Records the packet's counter, if it carries one to check, and returns
    /// what it means for its PID
    pub(super) fn check(&mut self, packet: &TransportPacket<'_>) -> Verdict {
        let found = packet.continuity_counter;
        let last = self.last.get_mut(&packet.pid);
        let record = |duplicate_tolerated| {
            Some(PidContinuity {
                counter: found,
                duplicate_tolerated,
            })
        };
        let (next, verdict) = match observe(last.as_deref().copied(), packet) {
            Observation::NotCountable => (None, Verdict::InOrder),
            Observation::FirstSeen | Observation::Continuous => (record(false), Verdict::InOrder),
            Observation::Excused => (record(false), Verdict::Excused),
            Observation::ToleratedDuplicate => (record(true), Verdict::Duplicate),
            // Why keep duplicate_tolerated set: resetting it would tolerate
            // the next repeat again, so a stuck counter would alternate
            // between InOrder and Broken instead of breaking every time.
            Observation::StuckDuplicate { expected } => {
                (record(true), Verdict::Broken { expected, found })
            }
            Observation::Discontinuous { expected } => {
                (record(false), Verdict::Broken { expected, found })
            }
        };
        match (last, next) {
            (_, None) => {}
            (Some(last), Some(next)) => *last = next,
            (None, Some(next)) => {
                self.last.insert(packet.pid, next);
            }
        }
        verdict
    }
}

/// Classifies a packet's `continuity_counter` against the PID's last one
fn observe(previous: Option<PidContinuity>, packet: &TransportPacket<'_>) -> Observation {
    let discontinuity_indicator = packet
        .adaptation_field
        .as_ref()
        .is_some_and(|field| field.discontinuity_indicator);
    // Why not check a null packet, or one without payload unless it indicates
    // a discontinuity: §2.4.3.3 leaves the counter unchanged on them, so
    // there is no value to expect, but §2.4.3.5 lets any packet indicate a
    // discontinuity, which sets the counter the next packet continues.
    if packet.pid == Pid::NULL || (packet.payload.is_none() && !discontinuity_indicator) {
        return Observation::NotCountable;
    }
    let Some(previous) = previous else {
        return Observation::FirstSeen;
    };
    if discontinuity_indicator {
        return Observation::Excused;
    }
    let expected = previous.counter.next();
    match packet.continuity_counter {
        found if found == previous.counter && previous.duplicate_tolerated => {
            Observation::StuckDuplicate { expected }
        }
        found if found == previous.counter => Observation::ToleratedDuplicate,
        found if found == expected => Observation::Continuous,
        _ => Observation::Discontinuous { expected },
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::ts::adaptation_field::AdaptationField;

    fn counter(value: u8) -> ContinuityCounter {
        ContinuityCounter::try_from(value).unwrap()
    }

    fn payload_packet(pid: u16, counter_value: u8) -> TransportPacket<'static> {
        let mut packet = TransportPacket::new(Pid::try_from(pid).unwrap(), counter(counter_value));
        packet.payload = Some(&[]);
        packet
    }

    fn excused_packet(pid: u16, counter_value: u8) -> TransportPacket<'static> {
        let mut field = AdaptationField::new();
        field.discontinuity_indicator = true;
        let mut packet = payload_packet(pid, counter_value);
        packet.adaptation_field = Some(field);
        packet
    }

    fn adaptation_only_packet(pid: u16, counter_value: u8) -> TransportPacket<'static> {
        let mut packet = payload_packet(pid, counter_value);
        packet.payload = None;
        packet.adaptation_field = Some(AdaptationField::new());
        packet
    }

    fn verdicts(packets: &[TransportPacket<'_>]) -> Vec<Verdict> {
        let mut tracker = ContinuityTracker::new();
        packets.iter().map(|packet| tracker.check(packet)).collect()
    }

    #[test]
    fn advancing_counters_stay_in_order() {
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 1),
                payload_packet(0x0100, 2),
            ]),
            [Verdict::InOrder; 3]
        );
    }

    #[test]
    fn a_counter_gap_breaks_continuity() {
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 1),
                payload_packet(0x0100, 3),
            ]),
            [
                Verdict::InOrder,
                Verdict::InOrder,
                Verdict::Broken {
                    expected: counter(2),
                    found: counter(3),
                },
            ]
        );
    }

    #[test]
    fn one_duplicate_is_tolerated_but_a_second_breaks() {
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 0),
            ]),
            [
                Verdict::InOrder,
                Verdict::Duplicate,
                Verdict::Broken {
                    expected: counter(1),
                    found: counter(0),
                },
            ]
        );
    }

    #[test]
    fn a_stuck_counter_breaks_on_every_duplicate_past_the_first() {
        let broken = Verdict::Broken {
            expected: counter(1),
            found: counter(0),
        };
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 0),
            ]),
            [Verdict::InOrder, Verdict::Duplicate, broken, broken]
        );
    }

    #[test]
    fn a_fresh_counter_after_a_stuck_run_tolerates_one_duplicate_again() {
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 0),
                payload_packet(0x0100, 1),
                payload_packet(0x0100, 1),
            ]),
            [
                Verdict::InOrder,
                Verdict::Duplicate,
                Verdict::Broken {
                    expected: counter(1),
                    found: counter(0),
                },
                Verdict::InOrder,
                Verdict::Duplicate,
            ]
        );
    }

    #[test]
    fn a_discontinuity_indicator_excuses_a_jump() {
        assert_eq!(
            verdicts(&[payload_packet(0x0100, 0), excused_packet(0x0100, 9)]),
            [Verdict::InOrder, Verdict::Excused]
        );
    }

    #[test]
    fn a_discontinuity_indicator_without_payload_sets_the_counter_that_follows() {
        let mut excused = excused_packet(0x0100, 5);
        excused.payload = None;
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                excused,
                payload_packet(0x0100, 6)
            ]),
            [Verdict::InOrder, Verdict::Excused, Verdict::InOrder]
        );
    }

    #[test]
    fn counters_are_tracked_per_pid() {
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                payload_packet(0x0200, 0),
                payload_packet(0x0100, 1),
                payload_packet(0x0200, 1),
            ]),
            [Verdict::InOrder; 4]
        );
    }

    #[test]
    fn a_packet_without_payload_leaves_the_expectation_unchanged() {
        assert_eq!(
            verdicts(&[
                payload_packet(0x0100, 0),
                adaptation_only_packet(0x0100, 9),
                payload_packet(0x0100, 1),
            ]),
            [Verdict::InOrder; 3]
        );
    }

    #[test]
    fn a_reset_forgets_every_counter() {
        let mut tracker = ContinuityTracker::new();
        tracker.check(&payload_packet(0x0100, 4));
        tracker.reset();
        assert_eq!(tracker.check(&payload_packet(0x0100, 9)), Verdict::InOrder);
    }
}
