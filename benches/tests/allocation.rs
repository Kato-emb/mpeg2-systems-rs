//! The demuxer allocates nothing once its buffers have grown
//!
//! After passes over an input, one more pass over the same input allocates
//! nothing: handing over input copies it into the buffer the passes before
//! grew, and the packets, the PAT and PMT the stream resends, and the
//! sections in flight reuse what was allocated before.

#[cfg(test)]
mod tests {
    use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig};
    use mpeg2_systems_bench::{CountingAllocator, STREAMS, allocations_once_grown};

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
                let mut demuxer = Demuxer::new(DemuxerConfig::default());
                let counted = allocations_once_grown(&ALLOCATOR, &mut demuxer, input, chunk_length);
                allocations.push((*name, chunk_length, counted));
                none.push((*name, chunk_length, 0));
            }
        }
        assert_eq!(allocations, none);
    }
}
