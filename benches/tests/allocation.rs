//! The demuxers and the Transport Stream muxer allocate nothing once their
//! buffers have grown
//!
//! After passes over an input, one more pass over the same input allocates
//! nothing: handing over input copies it into the buffer the passes before
//! grew, and the packets, the PAT and PMT the stream resends, the sections in
//! flight, the PES packets reassembled on a subscribed PID, and the elements
//! of a Program Stream reuse what was allocated before. Writing the same PES
//! packets, or PCRs on their own, again reuses the output buffer and the
//! continuity counters of the PIDs written before.

#[cfg(test)]
mod tests {
    use mpeg2_systems::ps;
    use mpeg2_systems::time::ClockReference;
    use mpeg2_systems::ts::Pid;
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
    use mpeg2_systems::ts::mux::Muxer;
    use mpeg2_systems_bench::{
        ACCESS_UNIT_LENGTH, CountingAllocator, PROGRAM, STREAMS, allocations_once_grown,
        demux_events, mux_pes_packets, program_stream_events, subscribed_demuxer,
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
        let mut muxer = Muxer::new();
        let mut out = Vec::new();
        let counted = allocations_once_grown(&ALLOCATOR, || {
            mux_pes_packets(&mut muxer, &access_unit, &mut out);
        });
        allocations.push(("pes packets", ACCESS_UNIT_LENGTH, "ts mux", counted));
        none.push(("pes packets", ACCESS_UNIT_LENGTH, "ts mux", 0));
        let pid = Pid::try_from(0x0100).unwrap();
        let counted = allocations_once_grown(&ALLOCATOR, || {
            out.clear();
            for base in 0..1_000 {
                let pcr = ClockReference::new(base, 0).unwrap();
                muxer
                    .write_program_clock_reference(pid, pcr, &mut out)
                    .unwrap();
            }
        });
        allocations.push(("pcrs on their own", 1_000, "ts mux", counted));
        none.push(("pcrs on their own", 1_000, "ts mux", 0));
        assert_eq!(allocations, none);
    }
}
