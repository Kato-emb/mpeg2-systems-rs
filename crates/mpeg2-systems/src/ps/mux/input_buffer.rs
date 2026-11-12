//! The input buffer of each stream in the P-STD (ISO/IEC 13818-1 §2.5.2.3):
//! the payload bytes the packs written have put in it, and when they leave it

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::pes::StreamId;
use crate::pes::extension::PStdBuffer;
use crate::time::SystemTimeClock;

/// The input buffer `Bn` of each stream registered, in the order of their
/// registration, `None` for a stream whose `stream_id` carries no optional
/// header
///
/// The muxer does not read the elementary streams, so it does not know where
/// an access unit ends. The payload of a PES packet leaves at its time, the
/// DTS or the PTS without one; that of a PES packet without a time leaves at
/// the time of the next PES packet of the stream with one, and is held until
/// then.
// Why not each buffer in the definition of its stream: the definition is
// replaced whole whenever the registration changes, and the buffer is the
// state of the stream as it is multiplexed.
#[derive(Debug, Clone)]
pub(super) struct InputBuffers(Vec<Option<InputBuffer>>);

/// The input buffer `Bn` of a stream: `BSn`, and the payload bytes of the PES
/// packets written that are still in it, oldest first
#[derive(Debug, Clone)]
struct InputBuffer {
    size: usize,
    /// Each run of payload bytes and the time it leaves, `None` until a later
    /// PES packet gives it; the times never go back
    held: VecDeque<(Option<SystemTimeClock>, usize)>,
}

/// Returns `p_std_buffer` in bytes
fn bytes(p_std_buffer: PStdBuffer) -> usize {
    usize::try_from(p_std_buffer.size_in_bytes()).unwrap_or(usize::MAX)
}

impl InputBuffers {
    pub(super) const fn new() -> Self {
        Self(Vec::new())
    }

    /// Adds the buffer of the stream registered next, of `stream_id` and the
    /// P-STD buffer size `p_std_buffer`
    pub(super) fn register(&mut self, stream_id: StreamId, p_std_buffer: PStdBuffer) {
        // Why not a buffer for every stream: a stream whose PES packets carry
        // no optional header has no time stamps, so nothing says when its
        // bytes leave the buffer (§2.5.2.3).
        self.0
            .push(stream_id.carries_optional_header().then(|| InputBuffer {
                size: bytes(p_std_buffer),
                held: VecDeque::new(),
            }));
    }

    /// Returns the earliest time the first of the `payload` bytes of a PES
    /// packet of the stream at `index` may arrive, so that its buffer never
    /// holds more than `BSn`, `None` for any time; `ticks_before(k)` is the
    /// ticks byte k (from 0) arrives after the first at the earliest, and
    /// `given` the P-STD buffer size the PES packet carries, which is `BSn`
    /// from it on (§2.4.3.7)
    ///
    /// # Errors
    ///
    /// The bytes the buffer would hold at once, the payload and those held
    /// until its time, and `BSn`, where they are more than `BSn`.
    pub(super) fn earliest(
        &self,
        index: usize,
        given: Option<PStdBuffer>,
        payload: usize,
        ticks_before: impl Fn(usize) -> u64,
    ) -> Result<Option<SystemTimeClock>, (usize, usize)> {
        let Some(Some(buffer)) = self.0.get(index) else {
            return Ok(None);
        };
        let size = given.map_or(buffer.size, bytes);
        let (mut held, unknown) =
            buffer
                .held
                .iter()
                .fold((0_usize, 0_usize), |(held, unknown), (leaves, bytes)| {
                    (
                        held.saturating_add(*bytes),
                        if leaves.is_none() {
                            unknown.saturating_add(*bytes)
                        } else {
                            unknown
                        },
                    )
                });
        let length = unknown.saturating_add(payload);
        if length > size {
            return Err((length, size));
        }
        let mut earliest = None;
        // Why only at the first byte past the room left before each time
        // bytes leave: the room grows only at those times, and a later byte
        // arrives later.
        for (leaves, bytes) in &buffer.held {
            let first_past_room = size.saturating_sub(held);
            let Some(leaves) = leaves else {
                break;
            };
            if first_past_room >= payload {
                break;
            }
            earliest = earliest.max(Some(leaves.sub_ticks(ticks_before(first_past_room))));
            held = held.saturating_sub(*bytes);
        }
        Ok(earliest)
    }

    /// Puts the `payload` bytes of a PES packet of the time `time` in the
    /// buffer of the stream at `index`, which is also the time the bytes held
    /// without one leave, and takes `given`, the P-STD buffer size the packet
    /// carries, for `BSn` from it on
    pub(super) fn put(
        &mut self,
        index: usize,
        given: Option<PStdBuffer>,
        time: Option<SystemTimeClock>,
        payload: usize,
    ) {
        let Some(Some(buffer)) = self.0.get_mut(index) else {
            return;
        };
        buffer.size = given.map_or(buffer.size, bytes);
        if let Some(time) = time {
            for (leaves, _) in buffer.held.iter_mut().rev() {
                if leaves.is_some() {
                    break;
                }
                *leaves = Some(time);
            }
        }
        if payload > 0 {
            buffer.held.push_back((time, payload));
        }
    }

    /// Lets go of the bytes that leave at `now` or before, the time from
    /// which bytes arrive next
    pub(super) fn forget(&mut self, now: SystemTimeClock) {
        for buffer in self.0.iter_mut().flatten() {
            while let Some((Some(leaves), _)) = buffer.held.front()
                && *leaves <= now
            {
                buffer.held.pop_front();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;

    fn at(ticks: u64) -> SystemTimeClock {
        SystemTimeClock::first(Timestamp::try_from(0).unwrap()).add_ticks(ticks)
    }

    /// The buffers of a video stream of 128 bytes
    fn buffers() -> InputBuffers {
        let mut buffers = InputBuffers::new();
        buffers.register(StreamId::from(0xe0), PStdBuffer::new(false, 1));
        buffers
    }

    #[test]
    fn a_payload_starts_once_the_bytes_held_leave_enough_room_for_each_byte() {
        let mut buffers = buffers();
        buffers.put(0, None, Some(at(1_000)), 60);
        buffers.put(0, None, Some(at(2_000)), 30);
        // 38 bytes fit now, the 39th after 1 000, the 99th after 2 000; a
        // byte a tick
        let earliest =
            |payload| buffers.earliest(0, None, payload, |byte| u64::try_from(byte).unwrap());
        assert_eq!(
            [
                earliest(38),
                earliest(39),
                earliest(98),
                earliest(99),
                earliest(128)
            ],
            [
                Ok(None),
                Ok(Some(at(1_000 - 38))),
                Ok(Some(at(1_000 - 38))),
                Ok(Some(at(2_000 - 98))),
                Ok(Some(at(2_000 - 98))),
            ]
        );
    }

    #[test]
    fn bytes_without_a_time_are_held_until_the_next_time_and_a_payload_past_the_rest_never_fits() {
        let mut buffers = buffers();
        buffers.put(0, None, None, 40);
        let held_until_known = buffers.earliest(0, None, 89, |_| 0);
        buffers.put(0, None, Some(at(500)), 20);
        buffers.forget(at(500));
        assert_eq!(
            (held_until_known, buffers.earliest(0, None, 128, |_| 0)),
            (Err((129, 128)), Ok(None))
        );
    }

    #[test]
    fn a_p_std_buffer_size_a_pes_packet_carries_is_bsn_from_it_on() {
        let mut buffers = buffers();
        let smaller = Some(PStdBuffer::new(false, 0));
        let at_once = buffers.earliest(0, smaller, 1, |_| 0);
        buffers.put(0, smaller, Some(at(100)), 0);
        assert_eq!(
            (at_once, buffers.earliest(0, None, 1, |_| 0)),
            (Err((1, 0)), Err((1, 0)))
        );
    }

    #[test]
    fn a_stream_without_an_optional_header_has_no_buffer() {
        let mut buffers = InputBuffers::new();
        buffers.register(StreamId::PRIVATE_STREAM_2, PStdBuffer::new(false, 1));
        buffers.put(0, None, None, 1_000);
        assert_eq!(buffers.earliest(0, None, 1_000, |_| 0), Ok(None));
    }
}
