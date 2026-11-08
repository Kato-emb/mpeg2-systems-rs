//! Reading of each Transport Packet of a stream FFmpeg wrote
//!
//! The packets keep the stream's mix of payload-only packets and packets
//! with an adaptation field.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::packet::TransportPacket;

/// Measures `TransportPacket::read` over every packet of `188.ts`
fn packet(criterion: &mut Criterion) {
    let stream: &[u8] = include_bytes!("../tests/fixtures/188.ts");
    let mut group = criterion.benchmark_group("packet");
    group.throughput(Throughput::Bytes(
        u64::try_from(stream.len()).unwrap_or(u64::MAX),
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
