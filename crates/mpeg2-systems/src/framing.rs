//! Framing shared by the Transport Stream and Program Stream demuxers
//!
//! A [`Decoder`] of one stream cuts units off the front of a [`Buffer`], and a
//! [`Framer`] drives it over input handed over in chunks of any length. Both
//! demuxers copy the input into the buffer once and lend the events ranges of
//! it until the next poll.

use alloc::vec::Vec;
use core::ops::Range;

/// The input handed over and not yet dropped, which a [`Decoder`] cuts units
/// off
#[derive(Debug, Clone)]
pub(crate) struct Buffer {
    bytes: Vec<u8>,
    /// Bytes at the front that were cut; the ranges queued events lend still
    /// point at them
    consumed: usize,
    /// Input offset of the first byte `bytes` holds, counted from the first
    /// byte ever handed over
    position: u64,
}

impl Buffer {
    const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            consumed: 0,
            position: 0,
        }
    }

    /// Appends input; the ranges already lent keep pointing at the same
    /// bytes
    fn extend(&mut self, bytes: &[u8]) {
        // Why copy every chunk, not only the tail that crosses into the
        // next: an event borrows the demuxer rather than the caller's chunk,
        // so poll_event can lend an element after handle_input has returned.
        self.bytes.extend_from_slice(bytes);
    }

    /// Drops the bytes cut, if they are no fewer than the bytes left
    fn compact(&mut self) {
        // Why not a VecDeque, which drops the front without moving the rest:
        // an element straddling its wrap has no single slice to lend, so every
        // element would be copied out. Why not compact whenever bytes were
        // cut: that moves the unread rest of a large chunk on every element.
        // Compacting only once the cut bytes are no fewer than the rest moves
        // each byte a bounded number of times in all.
        if self.consumed < self.bytes.len().saturating_sub(self.consumed) {
            return;
        }
        self.position = self.offset_of(self.consumed);
        self.bytes.drain(..self.consumed);
        self.consumed = 0;
    }

    /// Returns the bytes not cut yet
    pub(crate) fn unconsumed(&self) -> &[u8] {
        self.bytes.get(self.consumed..).unwrap_or_default()
    }

    /// Cuts `length` bytes off the front and returns the range they lie in
    pub(crate) fn split_to(&mut self, length: usize) -> Range<usize> {
        let start = self.consumed;
        self.consumed = self.consumed.saturating_add(length).min(self.bytes.len());
        start..self.consumed
    }

    /// Returns the bytes of a range a cut unit lies in
    pub(crate) fn get(&self, range: Range<usize>) -> &[u8] {
        self.bytes.get(range).unwrap_or_default()
    }

    /// Returns the input offset of the byte at `index`
    pub(crate) fn offset_of(&self, index: usize) -> u64 {
        // Why saturating rather than wrapping: u64 spans 16 EiB of input, so
        // the ceiling is unreachable, and stopping at it beats wrapping an
        // offset into one that names the wrong byte.
        self.position
            .saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }
}

/// Cuts the units of one stream off the front of a [`Buffer`]
///
/// The contract of `tokio_util::codec::Decoder`, over a buffer whose cut
/// bytes stay until the events that lend them are polled. Decoding here is
/// cutting units out of a byte stream, not the decoding (process) of ISO/IEC
/// 13818-1 §2.1.26.
pub(crate) trait Decoder {
    type Item;

    /// Cuts the next unit, or returns `None` when more input is needed
    fn decode(&mut self, buffer: &mut Buffer) -> Option<Self::Item>;

    /// Cuts the next unit once the input has ended, and returns `None` only
    /// when every byte has been cut
    fn decode_eof(&mut self, buffer: &mut Buffer) -> Option<Self::Item> {
        self.decode(buffer)
    }
}

/// What [`Framer::next`] returns
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Next<T> {
    Item(T),
    /// The input holds too few bytes to cut the next unit
    NeedMoreData,
    /// Every byte before the end [`Framer::finish`] marked has been cut
    EndOfInput,
}

/// Drives a [`Decoder`] over the input
#[derive(Debug, Clone)]
pub(crate) struct Framer<D> {
    decoder: D,
    buffer: Buffer,
    /// The input was ended, and not every byte before the end is cut yet
    eof: bool,
}

impl<D: Decoder> Framer<D> {
    pub(crate) const fn new(decoder: D) -> Self {
        Self {
            decoder,
            buffer: Buffer::new(),
            eof: false,
        }
    }

    /// Appends input; the ranges outputs carry keep pointing at the same
    /// bytes
    ///
    /// # Panics
    /// Panics if called after [`Framer::finish`] before [`Framer::next`] has
    /// returned [`Next::EndOfInput`].
    pub(crate) fn extend(&mut self, bytes: &[u8]) {
        assert!(
            !self.eof,
            "input handed over after finish, before the events it left were polled"
        );
        self.buffer.extend(bytes);
    }

    /// Ends the input at the bytes handed over so far
    pub(crate) fn finish(&mut self) {
        self.eof = true;
    }

    /// Drops the bytes cut, once no output's range points at them, if they
    /// are no fewer than the bytes left
    pub(crate) fn compact(&mut self) {
        self.buffer.compact();
    }

    /// Returns the bytes of a range an output carries
    pub(crate) fn bytes(&self, range: Range<usize>) -> &[u8] {
        self.buffer.get(range)
    }

    #[cfg(test)]
    pub(crate) fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Cuts the next unit, or says why none comes
    pub(crate) fn next(&mut self) -> Next<D::Item> {
        if !self.eof {
            return self
                .decoder
                .decode(&mut self.buffer)
                .map_or(Next::NeedMoreData, Next::Item);
        }
        if let Some(item) = self.decoder.decode_eof(&mut self.buffer) {
            return Next::Item(item);
        }
        debug_assert!(
            self.buffer.unconsumed().is_empty(),
            "a decoder left bytes uncut at the end of the input"
        );
        // Why take input again: a caller may go on after the end, on the
        // same footing.
        self.eof = false;
        Next::EndOfInput
    }
}

/// Bytes a search walked past and has not reported yet
///
/// Reported in pieces no longer than [`Skipped::LIMIT`], which bounds what a
/// stream that never synchronizes holds while still handing every byte back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Skipped {
    count: usize,
}

impl Skipped {
    pub(crate) const LIMIT: usize = 64 * 1024;

    pub(crate) const fn new() -> Self {
        Self { count: 0 }
    }

    /// Returns the bytes walked past
    pub(crate) const fn count(self) -> usize {
        self.count
    }

    /// Returns how many more bytes may be walked past before they are
    /// reported
    pub(crate) const fn room(self) -> usize {
        Self::LIMIT.saturating_sub(self.count)
    }

    pub(crate) const fn is_full(self) -> bool {
        self.count >= Self::LIMIT
    }

    /// Walks past `length` more bytes, no further than the room left
    pub(crate) fn skip(&mut self, length: usize) {
        self.count = self.count.saturating_add(length.min(self.room()));
    }

    /// Ends the run walked past, and returns how many bytes to report
    pub(crate) fn report(&mut self) -> usize {
        core::mem::replace(&mut self.count, 0)
    }

    /// Ends the run at the end of the input, and returns how many of the
    /// `left` bytes held to report at once
    pub(crate) fn report_rest(&mut self, left: usize) -> usize {
        self.count = 0;
        left.min(Self::LIMIT)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use alloc::vec::Vec;
    use core::fmt::Debug;
    use core::ops::Range;

    use super::{Decoder, Framer, Next, Skipped};

    /// What the shared tests need to know of an output of a [`Decoder`]
    pub(crate) trait Reported {
        /// Returns the range of the bytes the output cut, if it cut any
        fn range(&self) -> Option<Range<usize>>;
        /// Returns whether the output reports bytes passed over
        fn is_passed_over(&self) -> bool;
    }

    /// Hands `stream` over in chunks of `chunk` bytes, ends the input, and
    /// returns every output
    fn run<D: Decoder>(decoder: D, stream: &[u8], chunk: usize) -> Vec<D::Item> {
        let mut framer = Framer::new(decoder);
        let mut outputs = Vec::new();
        for piece in stream.chunks(chunk) {
            framer.extend(piece);
            while let Next::Item(item) = framer.next() {
                outputs.push(item);
            }
        }
        framer.finish();
        while let Next::Item(item) = framer.next() {
            outputs.push(item);
        }
        outputs
    }

    /// Checks the rules every framing follows on `stream`: every byte is cut
    /// into one output, in order; no report of bytes passed over exceeds
    /// [`Skipped::LIMIT`]; and where the input is cut does not change the
    /// outputs
    pub(crate) fn check<D>(decoder: &D, stream: &[u8])
    where
        D: Decoder + Clone,
        D::Item: Reported + Debug + PartialEq,
    {
        let whole = run(decoder.clone(), stream, stream.len().max(1));
        let mut next = 0;
        for output in &whole {
            if let Some(range) = output.range() {
                assert_eq!(range.start, next, "a gap or an overlap before {output:?}");
                next = range.end;
            }
            if output.is_passed_over() {
                assert!(
                    output
                        .range()
                        .is_some_and(|range| range.len() <= Skipped::LIMIT),
                    "{output:?} is longer than the report limit"
                );
            }
        }
        assert_eq!(next, stream.len(), "bytes left uncut");
        for chunk in [1, 2, 7, 13, 4096] {
            assert_eq!(
                run(decoder.clone(), stream, chunk),
                whole,
                "in chunks of {chunk}"
            );
        }
    }
}
