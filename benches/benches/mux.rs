//! Writing of PES packets as Transport Packets by the Transport Stream
//! packet writer, their multiplexing with the PAT, the PMT and the PCRs by
//! the Transport Stream muxer, and into packs by the Program Stream muxer
//!
//! Before each measurement, the routine runs once more under an allocation
//! count and must allocate nothing: once the output buffer has grown, writing
//! reuses it and the continuity counters of the PID, and a muxer reuses the
//! output it holds.

use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use mpeg2_systems::ts::mux::PacketWriter;
use mpeg2_systems_bench::{
    ACCESS_UNIT_LENGTH, CountingAllocator, PES_PACKETS, allocations_once_grown,
    multiplex_pes_packets, multiplex_program_stream_pes_packets, mux_pes_packets, registered_muxer,
    registered_program_stream_muxer,
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

/// Measures the muxer multiplexing video PES packets, with the PAT, the PMT
/// and the PCRs it sends
fn mux_ts_muxer(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("mux_ts_muxer");
    let access_unit = vec![0xAA; ACCESS_UNIT_LENGTH];
    let mut muxer = registered_muxer();
    let mut out = Vec::new();
    let mut first_dts = 90_000;
    assert_eq!(
        allocations_once_grown(&ALLOCATOR, || {
            multiplex_pes_packets(&mut muxer, &access_unit, first_dts, &mut out);
            first_dts += 3_600 * u64::try_from(PES_PACKETS).expect("a count fits u64");
        }),
        0
    );
    group.throughput(Throughput::Bytes(
        u64::try_from(PES_PACKETS * ACCESS_UNIT_LENGTH).expect("a length fits u64"),
    ));
    // Why a muxer for each iteration: the DTS of one run of PES packets must
    // follow on from the last, and enough iterations would carry them past
    // the 33 bits a DTS has.
    group.bench_with_input(
        BenchmarkId::from_parameter(format!("{PES_PACKETS} PES packets")),
        &access_unit,
        |bencher, access_unit| {
            bencher.iter_batched_ref(
                registered_muxer,
                |muxer| multiplex_pes_packets(muxer, black_box(access_unit), 90_000, &mut out),
                BatchSize::SmallInput,
            );
        },
    );
    group.finish();
}

/// Measures the Program Stream muxer multiplexing video PES packets into
/// packs, with their SCRs
fn mux_ps_muxer(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("mux_ps_muxer");
    let access_unit = vec![0xAA; ACCESS_UNIT_LENGTH];
    let mut muxer = registered_program_stream_muxer();
    let mut out = Vec::new();
    let mut first_dts = 90_000;
    assert_eq!(
        allocations_once_grown(&ALLOCATOR, || {
            multiplex_program_stream_pes_packets(&mut muxer, &access_unit, first_dts, &mut out);
            first_dts += 3_600 * u64::try_from(PES_PACKETS).expect("a count fits u64");
        }),
        0
    );
    group.throughput(Throughput::Bytes(
        u64::try_from(PES_PACKETS * ACCESS_UNIT_LENGTH).expect("a length fits u64"),
    ));
    // Why a muxer for each iteration: the DTS of one run of PES packets must
    // follow on from the last, and enough iterations would carry them past
    // the 33 bits a DTS has.
    group.bench_with_input(
        BenchmarkId::from_parameter(format!("{PES_PACKETS} PES packets")),
        &access_unit,
        |bencher, access_unit| {
            bencher.iter_batched_ref(
                registered_program_stream_muxer,
                |muxer| {
                    multiplex_program_stream_pes_packets(
                        muxer,
                        black_box(access_unit),
                        90_000,
                        &mut out,
                    )
                },
                BatchSize::SmallInput,
            );
        },
    );
    group.finish();
}

criterion_group!(benches, mux_ts, mux_ts_muxer, mux_ps_muxer);
criterion_main!(benches);
