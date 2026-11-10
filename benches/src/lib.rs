//! Benchmarks and allocation tests of the mpeg2-systems per-packet paths
//!
//! The benchmarks are in `benches/` and the tests in `tests/`; this library
//! holds the inputs and the reader loop they share, and the global allocator
//! both declare to count allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use mpeg2_systems::pes::extension::PStdBuffer;
use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
use mpeg2_systems::ps;
use mpeg2_systems::psi::pmt;
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::time::{ClockReference, Timestamp};
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
use mpeg2_systems::ts::mux::{Muxer, MuxerConfig, PacketWriter};

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

/// A Program Stream FFmpeg wrote
pub const PROGRAM: &[u8] = include_bytes!("../../crates/mpeg2-systems/tests/fixtures/program.ps");

/// Returns a demuxer that reassembles the PES packets of the video (PID
/// 0x100) and the audio (PID 0x101) of [`STREAMS`]
#[must_use]
pub fn subscribed_demuxer() -> Demuxer {
    let mut demuxer = Demuxer::new(DemuxerConfig::default());
    for pid in [0x0100, 0x0101].map(Pid::try_from).into_iter().flatten() {
        demuxer.subscribe_pes(pid);
    }
    demuxer
}

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

/// Hands `input` to the Program Stream demuxer in chunks of `chunk_length`
/// and polls each chunk to the end, returning the number of events
pub fn program_stream_events(
    demuxer: &mut ps::demux::Demuxer,
    input: &[u8],
    chunk_length: usize,
) -> usize {
    let mut events = 0;
    for chunk in input.chunks(chunk_length) {
        demuxer.handle_input(chunk);
        while demuxer.poll_event().is_some() {
            events += 1;
        }
    }
    events
}

/// PES packets the Transport Stream packet writer writes in one pass
pub const PES_PACKETS: usize = 1_000;

/// Length of the payload of each PES packet the writer writes, which leaves
/// the last Transport Packet of each short, so its stuffing is written too
pub const ACCESS_UNIT_LENGTH: usize = 4_000;

/// Clears `out` and writes into it [`PES_PACKETS`] video PES packets of
/// `access_unit`, each with a PTS and a PCR, returning the bytes written
///
/// # Panics
///
/// Panics if the writer refuses a packet, which a benchmark must not measure.
pub fn mux_pes_packets(writer: &mut PacketWriter, access_unit: &[u8], out: &mut Vec<u8>) -> usize {
    out.clear();
    let pid = Pid::try_from(0x0100).expect("0x0100 is a PID");
    for index in 0..PES_PACKETS {
        let ticks = u64::try_from(index).expect("an index fits u64") * 3_600;
        let mut header = OptionalHeader::new();
        header.pts = Some(Timestamp::try_from(ticks + 3_600).expect("a PTS fits 33 bits"));
        let mut packet = PesPacket::new(StreamId::from(0xE0), access_unit);
        packet.optional_header = Some(header);
        let pcr = ClockReference::new(ticks, 0).expect("a PCR fits its fields");
        writer
            .write_pes_packet(pid, &packet, Some(pcr), out)
            .expect("the writer writes the packet");
    }
    out.len()
}

/// Returns a muxer of the default settings with program 1 and its video
/// stream on PID 0x0100 registered
///
/// # Panics
///
/// Panics if the muxer refuses the registration, which a benchmark must not
/// measure.
#[must_use]
pub fn registered_muxer() -> Muxer {
    let mut muxer = Muxer::new(MuxerConfig::new()).expect("the default settings are taken");
    let pid = |value| Pid::try_from(value).expect("a PID");
    muxer
        .add_program(1, pid(0x1000))
        .expect("the muxer registers the program");
    muxer
        .add_stream(
            1,
            pmt::ElementaryStream::new(StreamType::AVC_VIDEO, pid(0x0100)),
        )
        .expect("the muxer registers the stream");
    muxer
}

/// Clears `out` and gives `muxer` [`PES_PACKETS`] video PES packets of
/// `access_unit`, a frame apart from the DTS `first_dts`, taking its output
/// into `out` after each, and returns the bytes taken
///
/// # Panics
///
/// Panics if the muxer refuses a packet, which a benchmark must not measure.
pub fn multiplex_pes_packets(
    muxer: &mut Muxer,
    access_unit: &[u8],
    first_dts: u64,
    out: &mut Vec<u8>,
) -> usize {
    out.clear();
    let pid = Pid::try_from(0x0100).expect("0x0100 is a PID");
    for index in 0..PES_PACKETS {
        let dts = first_dts + u64::try_from(index).expect("an index fits u64") * 3_600;
        let mut header = OptionalHeader::new();
        header.pts = Some(Timestamp::try_from(dts + 3_600).expect("a PTS fits 33 bits"));
        header.dts = Some(Timestamp::try_from(dts).expect("a DTS fits 33 bits"));
        let mut packet = PesPacket::new(StreamId::from(0xE0), access_unit);
        packet.optional_header = Some(header);
        muxer
            .handle_pes_packet(pid, &packet)
            .expect("the muxer takes the packet");
        muxer.poll_output(out);
    }
    out.len()
}

/// Returns a Program Stream muxer at 1 260 000 bytes a second, the other
/// settings by default, with its video stream of `stream_id` 0xE0 registered
///
/// # Panics
///
/// Panics if the muxer refuses the registration, which a benchmark must not
/// measure.
#[must_use]
pub fn registered_program_stream_muxer() -> ps::mux::Muxer {
    registered_program_stream_muxer_of(ps::mux::MuxerConfig::new(25_200))
}

/// Returns a Program Stream muxer of `config` with its video stream of
/// `stream_id` 0xE0 registered
///
/// # Panics
///
/// Panics if the muxer refuses `config` or the registration, which a
/// benchmark must not measure.
#[must_use]
pub fn registered_program_stream_muxer_of(config: ps::mux::MuxerConfig) -> ps::mux::Muxer {
    let mut muxer = ps::mux::Muxer::new(config).expect("the settings are taken");
    muxer
        .add_stream(
            ps::psm::ElementaryStream::new(StreamType::AVC_VIDEO, StreamId::from(0xE0)),
            PStdBuffer::new(true, 224),
        )
        .expect("the muxer registers the stream");
    muxer
}

/// Clears `out` and gives the Program Stream `muxer` [`PES_PACKETS`] video
/// PES packets of `access_unit`, a frame apart from the DTS `first_dts`,
/// taking its output into `out` after each, and returns the bytes taken
///
/// # Panics
///
/// Panics if the muxer refuses a packet, which a benchmark must not measure.
pub fn multiplex_program_stream_pes_packets(
    muxer: &mut ps::mux::Muxer,
    access_unit: &[u8],
    first_dts: u64,
    out: &mut Vec<u8>,
) -> usize {
    out.clear();
    for index in 0..PES_PACKETS {
        let dts = first_dts + u64::try_from(index).expect("an index fits u64") * 3_600;
        let mut header = OptionalHeader::new();
        header.pts = Some(Timestamp::try_from(dts + 3_600).expect("a PTS fits 33 bits"));
        header.dts = Some(Timestamp::try_from(dts).expect("a DTS fits 33 bits"));
        let mut packet = PesPacket::new(StreamId::from(0xE0), access_unit);
        packet.optional_header = Some(header);
        muxer
            .handle_pes_packet(&packet)
            .expect("the muxer takes the packet");
        muxer.poll_output(out);
    }
    out.len()
}

/// Returns the allocations `allocator` counts during one more `pass` over
/// an input, after two passes have grown the buffers the pass fills
pub fn allocations_once_grown(allocator: &CountingAllocator, mut pass: impl FnMut()) -> usize {
    // Why two passes before counting: bytes the framer has not yet cut stay
    // in the demuxer's buffer after the first, so the second input can
    // outgrow the capacity the first left once.
    pass();
    pass();
    allocator.reset();
    pass();
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
