//! Synchronization of the demuxer on a stream and on bytes that never
//! synchronize
//!
//! The framer is reached only through the demuxer, so the stream also runs
//! section reassembly; its cost apart from the framer shows against the
//! demuxer benchmark. Each input is handed over whole, and in chunks of 7
//! packets, the payload of a typical UDP datagram. Before each measurement,
//! the routine runs once more under an allocation count, as in the demuxer
//! benchmark.

use std::alloc::System;
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Length of the inputs the demuxer is measured on
const INPUT_LENGTH: usize = 10 << 20;

/// Lengths the input is handed over in: whole, and 7 packets
const CHUNK_LENGTHS: [usize; 2] = [INPUT_LENGTH, 7 * 188];

/// Hands `input` over in chunks of `chunk_length` and polls each chunk to the
/// end, returning the number of events
fn demux_events(demuxer: &mut Demuxer, input: &[u8], chunk_length: usize) -> usize {
    let mut events = 0_usize;
    for chunk in input.chunks(chunk_length) {
        demuxer.handle_input(chunk);
        while demuxer.poll_event().is_some() {
            events = events.wrapping_add(1);
        }
    }
    events
}

/// Measures the demuxer through a stream of `188.ts` and through 0xFF bytes
fn framer(criterion: &mut Criterion) {
    let stream: &[u8] = include_bytes!("../tests/fixtures/188.ts");
    let inputs = [
        (
            "188.ts",
            stream.repeat(INPUT_LENGTH.checked_div(stream.len()).unwrap_or_default()),
        ),
        ("0xff", vec![0xFF; INPUT_LENGTH]),
    ];
    let mut group = criterion.benchmark_group("framer");
    for (name, input) in &inputs {
        group.throughput(Throughput::Bytes(
            u64::try_from(input.len()).unwrap_or(u64::MAX),
        ));
        for chunk_length in CHUNK_LENGTHS {
            let mut demuxer = Demuxer::new(DemuxerConfig::default());
            // Why two passes before counting: bytes the search has not yet
            // passed over stay in the demuxer's buffer after the first, so
            // the second input can outgrow the capacity the first left once.
            demux_events(&mut demuxer, input, chunk_length);
            demux_events(&mut demuxer, input, chunk_length);
            let region = Region::new(ALLOCATOR);
            demux_events(&mut demuxer, input, chunk_length);
            let change = region.change();
            assert_eq!(
                (change.allocations, change.reallocations),
                (0, 0),
                "{name} in chunks of {chunk_length}: demuxing allocated once its buffers had grown"
            );
            group.bench_with_input(
                BenchmarkId::new(*name, chunk_length),
                input,
                |bencher, input| {
                    bencher.iter(|| demux_events(&mut demuxer, black_box(input), chunk_length));
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, framer);
criterion_main!(benches);
