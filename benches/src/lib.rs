//! Benchmarks and allocation tests of the mpeg2-systems per-packet paths
//!
//! The benchmarks are in `benches/` and the tests in `tests/`; this library
//! holds the inputs and the reader loop they share, and the global allocator
//! both declare to count allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use mpeg2_systems::ts::demux::Demuxer;

/// Length of the inputs the demuxer is measured on
pub const INPUT_LENGTH: usize = 10 << 20;

/// Transport Streams FFmpeg wrote, the same packets in each packet size, by
/// file name
pub const STREAMS: [(&str, &[u8]); 3] = [
    (
        "188.ts",
        include_bytes!("../../crates/mpeg2-systems/tests/fixtures/188.ts"),
    ),
    (
        "192.ts",
        include_bytes!("../../crates/mpeg2-systems/tests/fixtures/192.ts"),
    ),
    (
        "204.ts",
        include_bytes!("../../crates/mpeg2-systems/tests/fixtures/204.ts"),
    ),
];

/// Hands `input` over in chunks of `chunk_length` and polls each chunk to the
/// end, returning the number of events
pub fn demux_events(demuxer: &mut Demuxer, input: &[u8], chunk_length: usize) -> usize {
    let mut events = 0;
    for chunk in input.chunks(chunk_length) {
        demuxer.handle_input(chunk);
        while demuxer.poll_event().is_some() {
            events += 1;
        }
    }
    events
}

/// Returns the allocations `allocator` counts while `demuxer` takes `input`
/// once more, after two passes over it have grown its buffers
pub fn allocations_once_grown(
    allocator: &CountingAllocator,
    demuxer: &mut Demuxer,
    input: &[u8],
    chunk_length: usize,
) -> usize {
    // Why two passes before counting: bytes the framer has not yet cut stay
    // in the demuxer's buffer after the first, so the second input can
    // outgrow the capacity the first left once.
    demux_events(demuxer, input, chunk_length);
    demux_events(demuxer, input, chunk_length);
    allocator.reset();
    demux_events(demuxer, input, chunk_length);
    allocator.allocations()
}

/// A global allocator that hands every request to [`System`] and counts the
/// allocations and deallocations
///
/// Declare it with `#[global_allocator]`; it counts every thread's requests,
/// so read it while one thread allocates.
#[derive(Debug)]
pub struct CountingAllocator {
    allocations: AtomicUsize,
    deallocations: AtomicUsize,
}

impl CountingAllocator {
    /// Returns an allocator that has counted nothing
    #[must_use]
    pub const fn new() -> Self {
        Self {
            allocations: AtomicUsize::new(0),
            deallocations: AtomicUsize::new(0),
        }
    }

    /// Counts from zero again
    pub fn reset(&self) {
        self.allocations.store(0, Ordering::Relaxed);
        self.deallocations.store(0, Ordering::Relaxed);
    }

    /// Returns the allocations counted, a reallocation included
    #[must_use]
    pub fn allocations(&self) -> usize {
        self.allocations.load(Ordering::Relaxed)
    }

    /// Returns the deallocations counted
    #[must_use]
    pub fn deallocations(&self) -> usize {
        self.deallocations.load(Ordering::Relaxed)
    }
}

impl Default for CountingAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[expect(
    unsafe_code,
    reason = "GlobalAlloc is an unsafe trait; each method only counts and hands the request to System unchanged"
)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.allocations.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller's contract for `layout` is System's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.allocations.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller's contract for `layout` is System's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        self.allocations.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `pointer` came from this allocator, so from System.
        unsafe { System.realloc(pointer, layout, new_size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        self.deallocations.fetch_add(1, Ordering::Relaxed);
        // SAFETY: `pointer` came from this allocator, so from System.
        unsafe { System.dealloc(pointer, layout) }
    }
}
