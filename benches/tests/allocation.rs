//! The demuxers and the Transport Stream packet writer allocate nothing once
//! their buffers have grown
//!
//! After passes over an input, one more pass over the same input allocates
//! nothing: handing over input copies it into the buffer the passes before
//! grew, and the packets, the PAT and PMT the stream resends, the sections in
//! flight, the PES packets reassembled on a subscribed PID, and the elements
//! of a Program Stream reuse what was allocated before. Writing the same PES
//! packets, or PCRs on their own, again reuses the output buffer and the
//! continuity counters of the PIDs written before, and multiplexing PES
//! packets with either muxer reuses its output held and its scratch buffer,
//! the Program Stream one also when it sends the system header and the
//! program stream map with every PES packet.
//! A stream whose descriptors could not fit the program stream map, or a
//! program or a stream whose descriptors could not fit the PMT, is refused
//! before they are copied.

use core::time::Duration;

use mpeg2_systems::descriptor::DescriptorLoop;
use mpeg2_systems::pes::StreamId;
use mpeg2_systems::pes::extension::PStdBuffer;
use mpeg2_systems::ps;
use mpeg2_systems::psi::pmt;
use mpeg2_systems::stream_type::StreamType;
use mpeg2_systems::time::ClockReference;
use mpeg2_systems::ts::Pid;
use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
use mpeg2_systems::ts::mux::{Muxer, MuxerConfig, PacketWriter};
use mpeg2_systems_bench::{
    ACCESS_UNIT_LENGTH, CountingAllocator, PES_PACKETS, PROGRAM, STREAMS, allocations_once_grown,
    demux_events, multiplex_pes_packets, multiplex_program_stream_pes_packets, mux_pes_packets,
    program_stream_events, registered_muxer, registered_program_stream_muxer,
    registered_program_stream_muxer_of, subscribed_demuxer,
};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new();

// Why one test for every input: the allocator counts every thread, so
// tests run in parallel would count each other's allocations.
#[test]
fn a_pass_over_an_input_allocates_nothing_once_the_buffers_have_grown() {
    let mut inputs: Vec<(&str, Vec<u8>)> = STREAMS
        .iter()
        .map(|&(name, stream)| (name, stream.repeat(100)))
        .collect();
    inputs.push(("0xff", vec![0xFF; 1 << 20]));
    let mut allocations = Vec::new();
    let mut none = Vec::new();
    for (name, input) in &inputs {
        for chunk_length in [input.len(), 7 * 188] {
            for (demuxer_name, mut demuxer) in [
                ("ts", Demuxer::new(DemuxerConfig::default())),
                ("ts subscribed", subscribed_demuxer()),
            ] {
                let counted = allocations_once_grown(&ALLOCATOR, || {
                    demux_events(&mut demuxer, input, chunk_length);
                });
                allocations.push((*name, chunk_length, demuxer_name, counted));
                none.push((*name, chunk_length, demuxer_name, 0));
            }
        }
    }
    let program = PROGRAM.repeat(100);
    for (name, input) in [("program.ps", &program), ("0xff", &inputs[3].1)] {
        for chunk_length in [input.len(), 2048] {
            let mut demuxer = ps::demux::Demuxer::new();
            let counted = allocations_once_grown(&ALLOCATOR, || {
                program_stream_events(&mut demuxer, input, chunk_length);
            });
            allocations.push((name, chunk_length, "ps", counted));
            none.push((name, chunk_length, "ps", 0));
        }
    }
    let access_unit = vec![0xAA; ACCESS_UNIT_LENGTH];
    let mut writer = PacketWriter::new();
    let mut out = Vec::new();
    let counted = allocations_once_grown(&ALLOCATOR, || {
        mux_pes_packets(&mut writer, &access_unit, &mut out);
    });
    allocations.push(("pes packets", ACCESS_UNIT_LENGTH, "ts mux", counted));
    none.push(("pes packets", ACCESS_UNIT_LENGTH, "ts mux", 0));
    let pid = Pid::try_from(0x0100).unwrap();
    let counted = allocations_once_grown(&ALLOCATOR, || {
        out.clear();
        for base in 0..1_000 {
            let pcr = ClockReference::new(base, 0).unwrap();
            writer
                .write_program_clock_reference(pid, pcr, &mut out)
                .unwrap();
        }
    });
    allocations.push(("pcrs on their own", 1_000, "ts mux", counted));
    none.push(("pcrs on their own", 1_000, "ts mux", 0));
    let mut muxer = registered_muxer();
    let mut first_dts = 90_000;
    let counted = allocations_once_grown(&ALLOCATOR, || {
        multiplex_pes_packets(&mut muxer, &access_unit, first_dts, &mut out);
        first_dts += 3_600 * u64::try_from(PES_PACKETS).unwrap();
    });
    allocations.push(("pes packets", ACCESS_UNIT_LENGTH, "ts muxer", counted));
    none.push(("pes packets", ACCESS_UNIT_LENGTH, "ts muxer", 0));
    let mut muxer = registered_program_stream_muxer();
    let mut first_dts = 90_000;
    let counted = allocations_once_grown(&ALLOCATOR, || {
        multiplex_program_stream_pes_packets(&mut muxer, &access_unit, first_dts, &mut out);
        first_dts += 3_600 * u64::try_from(PES_PACKETS).unwrap();
    });
    allocations.push(("pes packets", ACCESS_UNIT_LENGTH, "ps muxer", counted));
    none.push(("pes packets", ACCESS_UNIT_LENGTH, "ps muxer", 0));
    let mut muxer = registered_program_stream_muxer_of(
        ps::mux::MuxerConfig::new(25_200).with_header_interval(Duration::ZERO),
    );
    let mut first_dts = 90_000;
    let counted = allocations_once_grown(&ALLOCATOR, || {
        multiplex_program_stream_pes_packets(&mut muxer, &access_unit, first_dts, &mut out);
        first_dts += 3_600 * u64::try_from(PES_PACKETS).unwrap();
    });
    allocations.push((
        "pes packets",
        ACCESS_UNIT_LENGTH,
        "ps muxer, headers each",
        counted,
    ));
    none.push((
        "pes packets",
        ACCESS_UNIT_LENGTH,
        "ps muxer, headers each",
        0,
    ));
    let es_info = vec![0x80; 8 << 20];
    let mut muxer = registered_program_stream_muxer();
    let counted = allocations_once_grown(&ALLOCATOR, || {
        muxer
            .add_stream(
                ps::psm::ElementaryStream::new(
                    StreamType::ISO_IEC_11172_3_AUDIO,
                    StreamId::from(0xC0),
                )
                .with_elementary_stream_info(DescriptorLoop::from(es_info.as_slice())),
                PStdBuffer::new(false, 32),
            )
            .unwrap_err();
    });
    allocations.push(("es_info past the map", es_info.len(), "ps muxer", counted));
    none.push(("es_info past the map", es_info.len(), "ps muxer", 0));
    let mut muxer = registered_muxer();
    let counted = allocations_once_grown(&ALLOCATOR, || {
        muxer
            .add_stream(
                1,
                pmt::ElementaryStream::new(
                    StreamType::ISO_IEC_13818_7_AUDIO,
                    Pid::try_from(0x0101).unwrap(),
                )
                .with_es_info(DescriptorLoop::from(es_info.as_slice())),
            )
            .unwrap_err();
    });
    allocations.push(("es_info past the pmt", es_info.len(), "ts muxer", counted));
    none.push(("es_info past the pmt", es_info.len(), "ts muxer", 0));
    let mut muxer = Muxer::new(MuxerConfig::new()).unwrap();
    muxer
        .add_program(1, Pid::try_from(0x1000).unwrap())
        .unwrap();
    let counted = allocations_once_grown(&ALLOCATOR, || {
        muxer
            .set_program_info(1, DescriptorLoop::from(es_info.as_slice()))
            .unwrap_err();
    });
    allocations.push((
        "program_info past the pmt",
        es_info.len(),
        "ts muxer",
        counted,
    ));
    none.push(("program_info past the pmt", es_info.len(), "ts muxer", 0));
    assert_eq!(allocations, none);
}
