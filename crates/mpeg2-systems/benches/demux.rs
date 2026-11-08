//! Demuxing of a stream FFmpeg wrote, in each packet size, to the end of
//! its events
//!
//! Before each measurement, the routine runs once more under an allocation
//! count: once the buffers have grown, handing over input copies it into the
//! demuxer's buffer and nothing else allocates.

use std::alloc::System;
use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};

#[global_allocator]
static ALLOCATOR: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

/// Length of the inputs the demuxer is measured on
const INPUT_LENGTH: usize = 10 << 20;

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

/// Measures the demuxer through `188.ts`, `192.ts` and `204.ts`
fn demux(criterion: &mut Criterion) {
    let streams: [(&str, &[u8]); 3] = [
        ("188.ts", include_bytes!("../tests/fixtures/188.ts")),
        ("192.ts", include_bytes!("../tests/fixtures/192.ts")),
        ("204.ts", include_bytes!("../tests/fixtures/204.ts")),
    ];
    let inputs = streams.map(|(name, stream)| {
        (
            name,
            stream.repeat(INPUT_LENGTH.checked_div(stream.len()).unwrap_or_default()),
        )
    });
    let mut group = criterion.benchmark_group("demux");
    for (name, input) in &inputs {
        let mut demuxer = Demuxer::new(DemuxerConfig::default());
        // Why two passes before counting: bytes the framer has not yet cut
        // stay in the demuxer's buffer after the first, so the second input
        // can outgrow the capacity the first left once.
        demux_events(&mut demuxer, input, input.len());
        demux_events(&mut demuxer, input, input.len());
        let region = Region::new(ALLOCATOR);
        demux_events(&mut demuxer, input, input.len());
        let change = region.change();
        assert_eq!(
            (change.allocations, change.reallocations),
            (0, 0),
            "{name}: demuxing allocated once its buffers had grown"
        );
        group.throughput(Throughput::Bytes(
            u64::try_from(input.len()).unwrap_or(u64::MAX),
        ));
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            input,
            |bencher, input| {
                bencher.iter(|| demux_events(&mut demuxer, black_box(input), input.len()));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, demux);
criterion_main!(benches);
