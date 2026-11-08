//! Demuxing of a stream FFmpeg wrote, in each packet size, to the end of
//! its events, and of the 188-byte one with its PES packets reassembled
//!
//! Before each measurement, the routine runs once more under an allocation
//! count and must allocate nothing: once the buffers have grown, handing over
//! input copies it into the demuxer's buffer and nothing else allocates.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
use mpeg2_systems_bench::{
    CountingAllocator, INPUT_LENGTH, STREAMS, allocations_once_grown, demux_events,
    subscribed_demuxer,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new();

/// Measures the demuxer through `188.ts`, `192.ts` and `204.ts`
fn demux(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("demux");
    for (name, stream) in STREAMS {
        let input = stream.repeat(INPUT_LENGTH / stream.len());
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        assert_eq!(
            allocations_once_grown(&ALLOCATOR, &mut demuxer, &input, input.len()),
            0,
            "{name}"
        );
        group.throughput(Throughput::Bytes(
            u64::try_from(input.len()).expect("a length fits u64"),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &input,
            |bencher, input| {
                bencher.iter(|| demux_events(&mut demuxer, black_box(input), input.len()));
            },
        );
    }
    group.finish();
}

/// Measures the demuxer through `188.ts` with the PES packets of its video
/// and audio reassembled
fn demux_pes(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("demux_pes");
    let (name, stream) = STREAMS[0];
    let input = stream.repeat(INPUT_LENGTH / stream.len());
    let mut demuxer = subscribed_demuxer();
    assert_eq!(
        allocations_once_grown(&ALLOCATOR, &mut demuxer, &input, input.len()),
        0,
        "{name}"
    );
    group.throughput(Throughput::Bytes(
        u64::try_from(input.len()).expect("a length fits u64"),
    ));
    group.bench_with_input(
        BenchmarkId::from_parameter(name),
        &input,
        |bencher, input| {
            bencher.iter(|| demux_events(&mut demuxer, black_box(input), input.len()));
        },
    );
    group.finish();
}

criterion_group!(benches, demux, demux_pes);
criterion_main!(benches);
