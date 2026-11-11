//! The input a demuxer holds, lent to the events it queues
//!
//! Both demuxers copy the input handed over into one buffer, cut elements off
//! its front, and lend the events ranges of it until the next poll. This is
//! that buffer, with the rule for when the cut bytes are dropped.

use alloc::vec::Vec;
use core::ops::Range;

/// Whether more input may follow the bytes handed over
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Input {
    Continues,
    Ends,
}

/// Whether the demuxer has reported every byte before the end of the input
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// The input has not ended, or bytes before its end are not reported yet
    NotReached,
    /// Every byte before the end is reported, and the buffer takes input
    /// again
    Reached,
}

/// The input handed to a demuxer and not yet dropped
#[derive(Debug, Clone)]
pub(crate) struct InputBuffer {
    bytes: Vec<u8>,
    /// Bytes at the front of `bytes` that were reported or cut; the ranges
    /// queued events lend still point at them
    consumed: usize,
    /// The input was ended, and the demuxer has not yet reported every byte
    /// before the end
    ended: bool,
}

impl InputBuffer {
    pub(crate) const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            consumed: 0,
            ended: false,
        }
    }

    /// Appends input; the ranges already lent keep pointing at the same
    /// bytes
    ///
    /// # Panics
    /// Panics if called after [`InputBuffer::finish`] before
    /// [`InputBuffer::reach_end`] reports the end reached.
    pub(crate) fn extend(&mut self, bytes: &[u8]) {
        assert!(
            !self.ended,
            "input handed over after finish, before the events it left were polled"
        );
        // Why copy every chunk, not only the tail that crosses into the
        // next: an event borrows the demuxer rather than the caller's chunk,
        // so poll_event can lend an element after handle_input has returned.
        self.bytes.extend_from_slice(bytes);
    }

    /// Ends the input at the bytes handed over so far
    pub(crate) fn finish(&mut self) {
        self.ended = true;
    }

    /// Returns whether more input may follow the bytes handed over
    pub(crate) const fn input(&self) -> Input {
        if self.ended {
            Input::Ends
        } else {
            Input::Continues
        }
    }

    /// Returns whether the end of the input is reached, and takes input
    /// again once it is
    pub(crate) fn reach_end(&mut self) -> End {
        if self.ended && self.consumed == self.bytes.len() {
            self.ended = false;
            End::Reached
        } else {
            End::NotReached
        }
    }

    /// Drops the bytes reported or cut, once no queued event lends them, if
    /// they are no fewer than the bytes left, and returns how many it dropped
    pub(crate) fn compact(&mut self) -> usize {
        // Why not a VecDeque, which drops the front without moving the rest:
        // an element straddling its wrap has no single slice to lend, so every
        // element would be copied out. Why not compact whenever bytes were
        // consumed: that moves the unread rest of a large chunk on every
        // element. Compacting only once the consumed bytes are no fewer than
        // the rest moves each byte a bounded number of times in all.
        if self.consumed < self.bytes.len().saturating_sub(self.consumed) {
            return 0;
        }
        let dropped = self.consumed;
        self.bytes.drain(..dropped);
        self.consumed = 0;
        dropped
    }

    /// Returns the bytes of a range a queued event lends
    pub(crate) fn get(&self, range: Range<usize>) -> &[u8] {
        self.bytes.get(range).unwrap_or_default()
    }

    /// Returns the bytes from `index` to the end
    pub(crate) fn from(&self, index: usize) -> &[u8] {
        self.bytes.get(index..).unwrap_or_default()
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    pub(crate) const fn consumed(&self) -> usize {
        self.consumed
    }

    /// Marks `length` more bytes at the front as reported or cut
    pub(crate) fn consume(&mut self, length: usize) {
        self.consumed = self.consumed.saturating_add(length);
    }
}
