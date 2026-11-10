//! The streams registered with a [`Muxer`], the system header and the
//! program stream map made of them, and the changes of the map

use alloc::vec::Vec;

use super::{Error, Muxer, PackWriter, largest_pack, shortest_pack};
use crate::descriptor::DescriptorLoop;
use crate::pes::StreamId;
use crate::pes::extension::PStdBuffer;
use crate::ps::psm::{self, ElementaryStream, ElementaryStreamLoop};
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
    pub(super) fn elementary_stream(&self) -> ElementaryStream<'_> {
        let mut entry = ElementaryStream::new(self.stream_type, self.stream_id);
        entry.elementary_stream_id_extension = self.stream_id_extension;
        entry.elementary_stream_info = DescriptorLoop::from(self.es_info.as_slice());
        entry
    }
}

/// Returns whether `audio_bound` counts a stream of `stream_type`: an
/// ISO/IEC 13818-3 or ISO/IEC 11172-3 audio stream (§2.5.3.6)
pub(super) fn is_audio(stream_type: StreamType) -> bool {
    matches!(
        stream_type,
        StreamType::ISO_IEC_11172_3_AUDIO | StreamType::ISO_IEC_13818_3_AUDIO
    )
}

/// Returns the `audio_bound` and the `video_bound` of `streams`
pub(super) fn bounds(streams: &[Stream]) -> (usize, usize) {
    (
        streams
            .iter()
            .filter(|stream| is_audio(stream.stream_type))
            .count(),
        streams
            .iter()
            .filter(|stream| stream.stream_type.is_video())
            .count(),
    )
}

/// Refuses the program stream map of `program_stream_info` and
/// `elementary_streams` where it would be longer than its syntax allows,
/// before anything of them is copied
pub(super) fn check_map_length<'stream>(
    program_stream_info: &[u8],
    elementary_streams: impl IntoIterator<Item = ElementaryStream<'stream>>,
) -> Result<(), Error> {
    psm::check_lengths(
        DescriptorLoop::from(program_stream_info),
        elementary_streams,
    )
    .map_err(|error| Error::ProgramStreamMap { error })
}

/// Appends the system header of `streams`, `rate_bound` and `fixed_flag`
/// as given, and the program stream map of `program_stream_info` and
/// `streams`, `program_stream_map_version` `version`, to `out`, writing their
/// loops in `entries`
pub(super) fn write_headers(
    streams: &[Stream],
    program_stream_info: &[u8],
    version: VersionNumber,
    rate_bound: u32,
    fixed_flag: bool,
    entries: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<(), Error> {
    let writer = PackWriter::new();
    let (audio_bound, video_bound) = bounds(streams);
    // Why not an error: registration refuses a stream past either bound,
    // which keeps both within a byte; the system header refuses the rest.
    let [audio_bound, video_bound] =
        [audio_bound, video_bound].map(|bound| u8::try_from(bound).unwrap_or(u8::MAX));
    entries.clear();
    StreamBoundLoop::write_from(streams.iter().map(Stream::stream_bound), entries)
        .map_err(|error| Error::SystemHeader { error })?;
    let mut system_header =
        SystemHeader::new(rate_bound, audio_bound, video_bound).with_fixed_flag(fixed_flag);
    system_header.stream_bounds = StreamBoundLoop::from(entries.as_slice());
    writer.write_system_header(&system_header, out)?;
    entries.clear();
    ElementaryStreamLoop::write_from(streams.iter().map(Stream::elementary_stream), entries)
        .map_err(|error| Error::ProgramStreamMap { error })?;
    let mut map = ProgramStreamMap::new(version);
    map.program_stream_info = DescriptorLoop::from(program_stream_info);
    map.elementary_streams = ElementaryStreamLoop::from(entries.as_slice());
    writer.write_program_stream_map(&map, out)
}

impl Muxer {
    /// Replaces the `stream_type` and the `elementary_stream_info` of the
    /// stream registered of the `elementary_stream_id` of `stream` and, when
    /// it is 0xFD, of its `elementary_stream_id_extension`
    ///
    /// The muxer copies the `elementary_stream_info` of `stream`, and keeps
    /// the bound registered with the stream. The program stream map changes
    /// as [`Muxer`] says, and a call that changes nothing changes nothing. A
    /// call refused leaves the muxer as it was.
    ///
    /// # Errors
    ///
    /// * [`StreamIdExtension`](Error::StreamIdExtension):
    ///   `elementary_stream_id_extension` is present where
    ///   `elementary_stream_id` is not 0xFD, or absent where it is.
    /// * [`UnregisteredStream`](Error::UnregisteredStream): no stream of
    ///   `stream` is registered.
    /// * [`SystemHeaderChange`](Error::SystemHeaderChange): a PES packet has
    ///   been taken already, and the `stream_type` given would add the stream
    ///   to, or take it from, those that `audio_bound` or `video_bound`
    ///   counts. The muxer does not change the system header it has written.
    /// * [`SystemHeader`](Error::SystemHeader):
    ///   [`SystemHeader::write`](crate::ps::SystemHeader::write) refuses the
    ///   system header, as it does the 33rd audio stream or the 17th video
    ///   stream (§2.5.3.6).
    /// * [`ProgramStreamMap`](Error::ProgramStreamMap):
    ///   [`ProgramStreamMap::write`](crate::ps::ProgramStreamMap::write)
    ///   refuses the map with the stream replaced, as it does
    ///   `elementary_stream_info` that is not whole descriptors, a
    ///   `stream_type` carried in sections, or a map past the 1018 bytes of
    ///   `program_stream_map_length`, which is refused before
    ///   `elementary_stream_info` is copied.
    /// * [`PackLength`](Error::PackLength): the first pack of a PES packet,
    ///   with the system header and the program stream map, could not carry a
    ///   PES packet in [`MuxerConfig::pack_size`], or in the bytes that
    ///   arrive within 0.7 s at [`MuxerConfig::program_mux_rate`] (§2.7.1).
    ///
    /// [`MuxerConfig::pack_size`]: super::MuxerConfig::pack_size
    /// [`MuxerConfig::program_mux_rate`]: super::MuxerConfig::program_mux_rate
    ///
    /// ```
    /// use mpeg2_systems::descriptor::DescriptorLoop;
    /// use mpeg2_systems::pes::extension::PStdBuffer;
    /// use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    /// use mpeg2_systems::ps::demux::{Demuxer, Event};
    /// use mpeg2_systems::ps::mux::{Muxer, MuxerConfig};
    /// use mpeg2_systems::ps::psm::ElementaryStream;
    /// use mpeg2_systems::stream_type::StreamType;
    /// use mpeg2_systems::time::Timestamp;
    ///
    /// let video = StreamId::from(0xe0);
    /// let mut muxer = Muxer::new(MuxerConfig::new(25_200))?;
    /// let mut stream = Vec::new();
    /// muxer.add_stream(
    ///     ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video),
    ///     PStdBuffer::new(true, 224),
    /// )?;
    /// let access_unit = |pts| {
    ///     PesPacket::new(video, &[0xaa; 100])
    ///         .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(pts).unwrap()))
    /// };
    /// muxer.handle_pes_packet(&access_unit(90_000))?;
    /// // The stream becomes AVC video, with a user private descriptor
    /// let descriptors = [0x80, 0x01, 0xaa];
    /// muxer.update_stream(
    ///     ElementaryStream::new(StreamType::AVC_VIDEO, video)
    ///         .with_elementary_stream_info(DescriptorLoop::from(descriptors.as_slice())),
    /// )?;
    /// muxer.handle_pes_packet(&access_unit(93_600))?;
    /// muxer.finish();
    /// muxer.poll_output(&mut stream);
    ///
    /// // Read back, the map of the second access unit is version 1
    /// let mut demuxer = Demuxer::new();
    /// demuxer.handle_input(&stream);
    /// demuxer.finish();
    /// let mut versions = Vec::new();
    /// while let Some(event) = demuxer.poll_event() {
    ///     if let Event::ProgramStreamMap(map) = event {
    ///         versions.push(map.program_stream_map_version.get());
    ///     }
    /// }
    /// assert_eq!(versions, [0, 1]);
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    pub fn update_stream(&mut self, stream: ElementaryStream<'_>) -> Result<(), Error> {
        let stream_id = stream.elementary_stream_id;
        let stream_id_extension = stream.elementary_stream_id_extension;
        if stream_id_extension.is_some() != (stream_id == StreamId::EXTENDED_STREAM_ID) {
            return Err(Error::StreamIdExtension { stream_id });
        }
        let Some((index, registered)) = self.stream(stream_id, stream_id_extension) else {
            return Err(Error::UnregisteredStream {
                stream_id,
                stream_id_extension,
            });
        };
        let es_info = stream.elementary_stream_info.as_bytes();
        if (registered.stream_type, registered.es_info.as_slice()) == (stream.stream_type, es_info)
        {
            return Ok(());
        }
        let counted = |stream_type: StreamType| (is_audio(stream_type), stream_type.is_video());
        if self.last_pack.is_some()
            && counted(registered.stream_type) != counted(stream.stream_type)
        {
            return Err(Error::SystemHeaderChange {
                stream_id,
                stream_id_extension,
            });
        }
        check_map_length(
            &self.program_stream_info,
            self.streams.iter().enumerate().map(|(at, registered)| {
                if at == index {
                    stream
                } else {
                    registered.elementary_stream()
                }
            }),
        )?;
        let mut streams = self.streams.clone();
        if let Some(registered) = streams.get_mut(index) {
            registered.stream_type = stream.stream_type;
            registered.es_info = es_info.to_vec();
        }
        check_headers(
            &streams,
            &self.program_stream_info,
            self.map_version,
            self.config.program_mux_rate,
            self.config.fixed_rate,
            largest_pack(&self.config),
        )?;
        self.streams = streams;
        self.change_map();
        Ok(())
    }

    /// Sets `program_stream_info`, the descriptors of the program in the
    /// program stream map, which the muxer copies
    ///
    /// A later call replaces the descriptors set before. The program stream
    /// map changes as [`Muxer`] says, and a call that changes nothing changes
    /// nothing. A call refused leaves the muxer as it was.
    ///
    /// # Errors
    ///
    /// * [`ProgramStreamMap`](Error::ProgramStreamMap):
    ///   [`ProgramStreamMap::write`](crate::ps::ProgramStreamMap::write)
    ///   refuses the map with `program_stream_info`, as it does
    ///   `program_stream_info` that is not whole descriptors, or a map past
    ///   the 1018 bytes of `program_stream_map_length`.
    /// * [`PackLength`](Error::PackLength): the first pack of a PES packet,
    ///   with the system header and the program stream map, could not carry a
    ///   PES packet in [`MuxerConfig::pack_size`], or in the bytes that
    ///   arrive within 0.7 s at [`MuxerConfig::program_mux_rate`] (§2.7.1).
    ///
    /// [`MuxerConfig::pack_size`]: super::MuxerConfig::pack_size
    /// [`MuxerConfig::program_mux_rate`]: super::MuxerConfig::program_mux_rate
    pub fn set_program_stream_info(
        &mut self,
        program_stream_info: DescriptorLoop<'_>,
    ) -> Result<(), Error> {
        let program_stream_info = program_stream_info.as_bytes();
        if program_stream_info == self.program_stream_info {
            return Ok(());
        }
        check_headers(
            &self.streams,
            program_stream_info,
            self.map_version,
            self.config.program_mux_rate,
            self.config.fixed_rate,
            largest_pack(&self.config),
        )?;
        self.program_stream_info = program_stream_info.to_vec();
        self.change_map();
        Ok(())
    }

    /// Makes the program stream map as it now stands due in the next pack,
    /// its version incremented when one has been sent since the last change
    pub(super) fn change_map(&mut self) {
        if self.headers_sent_at.take().is_some() {
            self.map_version = self.map_version.next();
        }
    }
}

/// Refuses the system header of `streams`, `rate_bound` and `fixed_flag` as
/// given, and the program stream map of `program_stream_info` and `streams`,
/// `program_stream_map_version` `version`, where [`SystemHeader::write`] or
/// [`ProgramStreamMap::write`] does, or where the first pack of a PES packet,
/// with them, could not carry a PES packet in `largest_pack` bytes
pub(super) fn check_headers(
    streams: &[Stream],
    program_stream_info: &[u8],
    version: VersionNumber,
    rate_bound: u32,
    fixed_flag: bool,
    largest_pack: usize,
) -> Result<(), Error> {
    let mut headers = Vec::new();
    write_headers(
        streams,
        program_stream_info,
        version,
        rate_bound,
        fixed_flag,
        &mut Vec::new(),
        &mut headers,
    )?;
    let length = shortest_pack(headers.len());
    if length > largest_pack {
        return Err(Error::PackLength { length });
    }
    Ok(())
}
