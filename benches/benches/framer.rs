//! Synchronization of the demuxer on a stream and on bytes that never
//! synchronize
//!
//! The framer is reached only through the demuxer, so the stream also runs
//! section reassembly; its cost apart from the framer shows against the
//! demuxer benchmark. Each input is handed over whole, and in chunks of 7
//! packets, the payload of a typical UDP datagram. Before each measurement,
//! the routine runs once more under an allocation count and must allocate
//! nothing.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
use mpeg2_systems_bench::{
    CountingAllocator, INPUT_LENGTH, STREAMS, allocations_once_grown, demux_events,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new();

/// Lengths the input is handed over in: whole, and 7 packets
const CHUNK_LENGTHS: [usize; 2] = [INPUT_LENGTH, 7 * 188];

/// Measures the demuxer through a stream of `188.ts` and through 0xFF bytes
fn framer(criterion: &mut Criterion) {
    let (_, stream) = STREAMS[0];
    let inputs = [
        ("188.ts", stream.repeat(INPUT_LENGTH / stream.len())),
        ("0xff", vec![0xFF; INPUT_LENGTH]),
    ];
    let mut group = criterion.benchmark_group("framer");
    for (name, input) in &inputs {
        group.throughput(Throughput::Bytes(
            u64::try_from(input.len()).expect("a length fits u64"),
        ));
        for chunk_length in CHUNK_LENGTHS {
            let mut demuxer = Demuxer::new(DemuxerConfig::default());
            assert_eq!(
                allocations_once_grown(&ALLOCATOR, &mut demuxer, input, chunk_length),
                0,
                "{name} in chunks of {chunk_length}"
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
