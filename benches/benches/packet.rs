//! Reading of each Transport Packet of a stream FFmpeg wrote
//!
//! The packets keep the stream's mix of payload-only packets and packets
//! with an adaptation field.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::packet::TransportPacket;
use mpeg2_systems_bench::STREAMS;

/// Measures `TransportPacket::read` over every packet of `188.ts`
fn packet(criterion: &mut Criterion) {
    let (_, stream) = STREAMS[0];
    let mut group = criterion.benchmark_group("packet");
    group.throughput(Throughput::Bytes(
        u64::try_from(stream.len()).expect("a length fits u64"),
    ));
    group.bench_function("read", |bencher| {
        bencher.iter(|| {
            black_box(stream)
                .as_chunks::<188>()
                .0
                .iter()
                .filter(|packet| TransportPacket::read(*packet).is_ok())
                .count()
        });
    });
    group.finish();
}

criterion_group!(benches, packet);
criterion_main!(benches);
