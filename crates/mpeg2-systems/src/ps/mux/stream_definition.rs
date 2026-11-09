//! The streams registered with a [`Muxer`](super::Muxer), and the system
//! header and the program stream map made of them

use alloc::vec::Vec;

use super::{Error, PackWriter};
use crate::descriptor::DescriptorLoop;
use crate::pes::StreamId;
use crate::pes::extension::PStdBuffer;
use crate::ps::psm::{ElementaryStream, ElementaryStreamLoop};
use crate::ps::{ProgramStreamMap, StreamBound, StreamBoundLoop, SystemHeader};
use crate::psi::VersionNumber;
use crate::stream_type::StreamType;
use crate::time::SystemTimeClock;

/// A stream registered
#[derive(Debug, Clone)]
pub(super) struct Stream {
    pub(super) stream_id: StreamId,
    pub(super) stream_id_extension: Option<u8>,
    pub(super) stream_type: StreamType,
    pub(super) p_std_buffer: PStdBuffer,
    pub(super) es_info: Vec<u8>,
    /// The time of the STC the DTS (the PTS without one) of the last PES
    /// packet given on the stream samples, `None` before the first
    // Why not a flag for the first PES packet besides it: the first of a
    // stream that carries the optional header has a PTS, so this is set
    // whenever that stream has had one.
    // Why not the clock of the muxer: the clock follows every stream, and a
    // stream may run behind another.
    pub(super) last_timestamp: Option<SystemTimeClock>,
}

impl Stream {
    /// Returns whether `audio_bound` counts the stream: an ISO/IEC 13818-3
    /// or ISO/IEC 11172-3 audio stream (§2.5.3.6)
    pub(super) fn is_audio(&self) -> bool {
        matches!(
            self.stream_type,
            StreamType::ISO_IEC_11172_3_AUDIO | StreamType::ISO_IEC_13818_3_AUDIO
        )
    }

    /// Returns the entry of the stream in the stream bound loop: 0xB7 with
    /// its `stream_id_extension`, or its `stream_id`
    fn stream_bound(&self) -> StreamBound {
        let stream_id = match self.stream_id_extension {
            Some(_) => StreamBound::WITH_STREAM_ID_EXTENSION,
            None => self.stream_id,
        };
        let mut bound = StreamBound::new(
            stream_id,
            self.p_std_buffer.p_std_buffer_scale,
            self.p_std_buffer.p_std_buffer_size,
        );
        bound.stream_id_extension = self.stream_id_extension;
        bound
    }

    /// Returns the entry of the stream in the elementary stream loop of the
    /// program stream map
    fn elementary_stream(&self) -> ElementaryStream<'_> {
        let mut entry = ElementaryStream::new(self.stream_type, self.stream_id);
        entry.elementary_stream_id_extension = self.stream_id_extension;
        entry.elementary_stream_info = DescriptorLoop::from(self.es_info.as_slice());
        entry
    }
}

/// Returns the `audio_bound` and the `video_bound` of `streams`
pub(super) fn bounds(streams: &[Stream]) -> (usize, usize) {
    (
        streams.iter().filter(|stream| stream.is_audio()).count(),
        streams
            .iter()
            .filter(|stream| stream.stream_type.is_video())
            .count(),
    )
}

/// Appends the system header and the program stream map of `streams` to
/// `out`, `rate_bound` `program_mux_rate`
pub(super) fn write_headers(
    streams: &[Stream],
    program_mux_rate: u32,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    let writer = PackWriter::new();
    let (audio_bound, video_bound) = bounds(streams);
    // Why not an error: registration refuses a stream past either bound,
    // which keeps both within a byte; the system header refuses the rest.
    let [audio_bound, video_bound] =
        [audio_bound, video_bound].map(|bound| u8::try_from(bound).unwrap_or(u8::MAX));
    let mut entries = Vec::new();
    StreamBoundLoop::write_from(streams.iter().map(Stream::stream_bound), &mut entries)
        .map_err(|error| Error::SystemHeader { error })?;
    let mut system_header = SystemHeader::new(program_mux_rate, audio_bound, video_bound);
    system_header.stream_bounds = StreamBoundLoop::from(entries.as_slice());
    writer.write_system_header(&system_header, out)?;
    entries.clear();
    ElementaryStreamLoop::write_from(streams.iter().map(Stream::elementary_stream), &mut entries)
        .map_err(|error| Error::ProgramStreamMap { error })?;
    let mut map = ProgramStreamMap::new(VersionNumber(0));
    map.elementary_streams = ElementaryStreamLoop::from(entries.as_slice());
    writer.write_program_stream_map(&map, out)
}
