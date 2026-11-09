//! Writing of PES packets as Transport Packets by the Transport Stream
//! packet writer
//!
//! Before each measurement, the routine runs once more under an allocation
//! count and must allocate nothing: once the output buffer has grown, writing
//! reuses it and the continuity counters of the PID.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::mux::PacketWriter;
use mpeg2_systems_bench::{
    ACCESS_UNIT_LENGTH, CountingAllocator, PES_PACKETS, allocations_once_grown, mux_pes_packets,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new();

/// Measures the packet writer writing video PES packets, each with a PCR
fn mux_ts(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("mux_ts");
    let access_unit = vec![0xAA; ACCESS_UNIT_LENGTH];
    let mut writer = PacketWriter::new();
    let mut out = Vec::new();
    assert_eq!(
        allocations_once_grown(&ALLOCATOR, || {
            mux_pes_packets(&mut writer, &access_unit, &mut out);
        }),
        0
    );
    group.throughput(Throughput::Bytes(
        u64::try_from(PES_PACKETS * ACCESS_UNIT_LENGTH).expect("a length fits u64"),
    ));
    group.bench_with_input(
        BenchmarkId::from_parameter(format!("{PES_PACKETS} PES packets")),
        &access_unit,
        |bencher, access_unit| {
            bencher.iter(|| mux_pes_packets(&mut writer, black_box(access_unit), &mut out));
        },
    );
    group.finish();
}

criterion_group!(benches, mux_ts);
criterion_main!(benches);
