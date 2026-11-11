//! The streams registered with a [`Muxer`], the system header and the
//! program stream map made of them, and the changes of the map

use alloc::vec::Vec;

use super::csps::{self, DecoderBufferBound};
use super::{Error, Muxer, PackWriter, shortest_pack};
use crate::descriptor::DescriptorLoop;
use crate::pes::StreamId;
use crate::pes::extension::PStdBuffer;
use crate::ps::psm::{self, ElementaryStream, ElementaryStreamLoop};
use crate::ps::{ProgramStreamMap, StreamBound, StreamBoundLoop, SystemHeader, system_header};
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
    /// The bound of §2.7.9 the caller has declared for the stream, `None`
    /// before it does
    // Why not a table of bounds in `MuxerConfig`: it would hold the streams
    // by their ids a second time, apart from their registration.
    pub(super) decoder_buffer_bound: Option<DecoderBufferBound>,
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

/// The fields of the system header that the whole Program Stream sets, not
/// its streams
#[derive(Debug, Clone, Copy)]
pub(super) struct SystemHeaderFields {
    pub(super) rate_bound: u32,
    pub(super) fixed_flag: bool,
    pub(super) csps_flag: bool,
    pub(super) packet_rate_restriction_flag: bool,
}

/// Appends the system header of `fields` and of `streams`, and the program
/// stream map of `program_stream_info` and `streams`,
/// `program_stream_map_version` `version`, to `out`, writing their loops in
/// `entries`, and returns the bytes of the system header
pub(super) fn write_headers(
    streams: &[Stream],
    program_stream_info: &[u8],
    version: VersionNumber,
    fields: SystemHeaderFields,
    entries: &mut Vec<u8>,
    out: &mut Vec<u8>,
) -> Result<usize, Error> {
    let writer = PackWriter::new();
    let (audio_bound, video_bound) = bounds(streams);
    // Why not an error: registration refuses a stream past either bound,
    // which keeps both within a byte; the system header refuses the rest.
    let [audio_bound, video_bound] =
        [audio_bound, video_bound].map(|bound| u8::try_from(bound).unwrap_or(u8::MAX));
    entries.clear();
    StreamBoundLoop::write_from(streams.iter().map(Stream::stream_bound), entries)
        .map_err(|error| Error::SystemHeader { error })?;
    let mut system_header = SystemHeader::new(fields.rate_bound, audio_bound, video_bound)
        .with_fixed_flag(fields.fixed_flag)
        .with_csps_flag(fields.csps_flag)
        .with_packet_rate_restriction_flag(fields.packet_rate_restriction_flag);
    system_header.stream_bounds = StreamBoundLoop::from(entries.as_slice());
    let start = out.len();
    writer.write_system_header(&system_header, out)?;
    let system_header_length = out.len().saturating_sub(start);
    entries.clear();
    ElementaryStreamLoop::write_from(streams.iter().map(Stream::elementary_stream), entries)
        .map_err(|error| Error::ProgramStreamMap { error })?;
    let mut map = ProgramStreamMap::new(version);
    map.program_stream_info = DescriptorLoop::from(program_stream_info);
    map.elementary_streams = ElementaryStreamLoop::from(entries.as_slice());
    writer.write_program_stream_map(&map, out)?;
    Ok(system_header_length)
}

impl Muxer {
    /// Registers `stream`, its PES packets of its `elementary_stream_id`
    /// and, when it is 0xFD, of its `elementary_stream_id_extension`
    ///
    /// `p_std_buffer` is both the bound of the stream in the system header
    /// and the P-STD buffer size of its first PES packet, and no PES packet
    /// of the stream may carry a larger one. The muxer copies the
    /// `elementary_stream_info` of `stream`, the descriptors of the stream as
    /// written.
    ///
    /// # Errors
    ///
    /// * [`StreamId`](Error::StreamId): `elementary_stream_id` is below 0xBD,
    ///   which this entry point does not take: 0xBC, the
    ///   `program_stream_map`, is written with
    ///   [`ProgramStreamMap`], and those below it
    ///   start the other elements of a Program Stream.
    /// * [`StreamIdExtension`](Error::StreamIdExtension):
    ///   `elementary_stream_id_extension` is present where
    ///   `elementary_stream_id` is not 0xFD, or absent where it is.
    /// * [`StreamInUse`](Error::StreamInUse): the stream is registered
    ///   already.
    /// * [`AudioBound`](Error::AudioBound), [`VideoBound`](Error::VideoBound):
    ///   the stream would be the 33rd audio stream or the 17th video stream
    ///   (§2.5.3.6).
    /// * [`SystemHeader`](Error::SystemHeader):
    ///   [`SystemHeader::write`](crate::ps::SystemHeader::write) refuses the
    ///   bound of the stream, as it does a `p_std_buffer_scale` of `'1'` for
    ///   audio or of `'0'` for video.
    /// * [`ProgramStreamMap`](Error::ProgramStreamMap):
    ///   [`ProgramStreamMap::write`](crate::ps::ProgramStreamMap::write)
    ///   refuses the map with the stream, as it does
    ///   `elementary_stream_info` that is not whole descriptors, a
    ///   `stream_type` carried in sections, or a map past the 1018 bytes of
    ///   `program_stream_map_length`, which is refused before
    ///   `elementary_stream_info` is copied.
    /// * [`PackLength`](Error::PackLength): the first pack, with the system
    ///   header and the program stream map, could not carry a PES packet in
    ///   [`MuxerConfig::pack_size`], or in the bytes that arrive within 0.7 s
    ///   at [`MuxerConfig::program_mux_rate`] (§2.7.1), or in a CSPS at a
    ///   fixed rate the packs of the system header and of the program stream
    ///   map could not be as long as the packet rate asks (§2.7.9).
    ///
    /// # Panics
    ///
    /// Panics if a PES packet has been taken already: every system header,
    /// the first included, names every stream of the Program Stream
    /// (§2.5.3.6, §2.7.8), and the muxer makes it of the streams registered.
    ///
    /// [`MuxerConfig::pack_size`]: super::MuxerConfig::pack_size
    /// [`MuxerConfig::program_mux_rate`]: super::MuxerConfig::program_mux_rate
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn add_stream(
        &mut self,
        stream: ElementaryStream<'_>,
        p_std_buffer: PStdBuffer,
    ) -> Result<(), Error> {
        assert!(
            self.last_pack.is_none(),
            "a stream is registered after the first PES packet"
        );
        let stream_id = stream.elementary_stream_id;
        let stream_id_extension = stream.elementary_stream_id_extension;
        if !stream_id.is_pes_packet() || stream_id == StreamId::PROGRAM_STREAM_MAP {
            return Err(Error::StreamId { stream_id });
        }
        if stream_id_extension.is_some() != (stream_id == StreamId::EXTENDED_STREAM_ID) {
            return Err(Error::StreamIdExtension { stream_id });
        }
        if self.stream(stream_id, stream_id_extension).is_some() {
            return Err(Error::StreamInUse {
                stream_id,
                stream_id_extension,
            });
        }
        check_map_length(
            &self.program_stream_info,
            self.streams
                .iter()
                .map(Stream::elementary_stream)
                .chain(core::iter::once(stream)),
        )?;
        let stream = Stream {
            stream_id,
            stream_id_extension,
            stream_type: stream.stream_type,
            p_std_buffer,
            es_info: stream.elementary_stream_info.as_bytes().to_vec(),
            last_timestamp: None,
            decoder_buffer_bound: None,
        };
        let (audio_bound, video_bound) = bounds(&self.streams);
        if is_audio(stream.stream_type) && audio_bound >= system_header::MAX_AUDIO_BOUND.into() {
            return Err(Error::AudioBound {
                audio_bound: audio_bound.saturating_add(1),
            });
        }
        if stream.stream_type.is_video() && video_bound >= system_header::MAX_VIDEO_BOUND.into() {
            return Err(Error::VideoBound {
                video_bound: video_bound.saturating_add(1),
            });
        }
        self.streams.push(stream);
        self.check_headers_of(&self.streams, &self.program_stream_info)
            .inspect_err(|_| {
                self.streams.pop();
            })
    }

    /// Refuses the system header of `streams` and the program stream map of
    /// `program_stream_info` and `streams` where
    /// [`check_headers`](check_headers) does at the
    /// settings and the map version of the muxer
    fn check_headers_of(
        &self,
        streams: &[Stream],
        program_stream_info: &[u8],
    ) -> Result<(), Error> {
        let (system_header, map) = check_headers(
            streams,
            program_stream_info,
            self.map_version,
            self.system_header_fields(),
        )?;
        let length = self
            .rate()
            .longest_first_pack(system_header, map, shortest_pack());
        if length > self.largest_pack() {
            return Err(Error::PackLength { length });
        }
        Ok(())
    }

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
    /// * [`DecoderBufferBound`](Error::DecoderBufferBound): the bound
    ///   declared for the stream with
    ///   [`set_decoder_buffer_bound`](Self::set_decoder_buffer_bound) is not
    ///   one the `stream_type` given may take.
    /// * [`PackLength`](Error::PackLength): the first pack of a PES packet,
    ///   with the system header and the program stream map, could not carry a
    ///   PES packet in [`MuxerConfig::pack_size`], or in the bytes that
    ///   arrive within 0.7 s at [`MuxerConfig::program_mux_rate`] (§2.7.1),
    ///   or in a CSPS at a fixed rate the packs of the system header and of
    ///   the program stream map could not be as long as the packet rate asks
    ///   (§2.7.9).
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
        if let Some(declared) = registered.decoder_buffer_bound
            && !csps::is_declarable(stream_id, stream.stream_type, declared)
        {
            return Err(Error::DecoderBufferBound {
                stream_id,
                stream_id_extension,
                decoder_buffer_bound: declared,
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
        self.check_headers_of(&streams, &self.program_stream_info)?;
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
        self.check_headers_of(&self.streams, program_stream_info)?;
        self.program_stream_info = program_stream_info.to_vec();
        self.change_map();
        Ok(())
    }

    /// Declares `decoder_buffer_bound` the bound of the stream registered of
    /// `stream_id` and, when it is 0xFD, of `stream_id_extension`, which
    /// holds its P-STD buffer in a CSPS ([`MuxerConfig::csps`])
    ///
    /// ISO/IEC 13818-1 §2.7.9 bounds the buffer by what the stream is. The
    /// muxer tells it from the `stream_id` (Table 2-22) and the
    /// `stream_type` (Table 2-34) where they say it: an audio stream, an
    /// ISO/IEC 13818-7 ADTS audio stream, or a stream the clause does not
    /// bound. The caller declares the size of the VBV of an H.262 or ISO/IEC
    /// 11172-2 video stream (`stream_type` 0x01, 0x02 and 0x22) and of the CPB
    /// of an H.264 video stream (0x1B and 0x23), whether a video
    /// sub-bitstream of an AVC video stream (0x1F, 0x20 and 0x26) has the
    /// bound of its CPB or none, and the bound of a stream whose
    /// `stream_type` does not say what it carries (0x06, the user private
    /// 0x80 to 0xFF, and the reserved values). The muxer does not read the
    /// elementary stream for any of them. A declaration replaces the one
    /// before it, and is kept outside a CSPS.
    ///
    /// # Errors
    ///
    /// * [`StreamIdExtension`](Error::StreamIdExtension): `stream_id_extension`
    ///   is present where `stream_id` is not 0xFD, or absent where it is.
    /// * [`UnregisteredStream`](Error::UnregisteredStream): no stream of
    ///   `stream_id` and `stream_id_extension` is registered.
    /// * [`DecoderBufferBound`](Error::DecoderBufferBound):
    ///   `decoder_buffer_bound` is not one the `stream_id` and the
    ///   `stream_type` of the stream may take, such as [`Audio`] for a stream
    ///   of `stream_type` 0x02.
    ///
    /// [`MuxerConfig::csps`]: super::MuxerConfig::csps
    /// [`Audio`]: DecoderBufferBound::Audio
    ///
    /// ```
    /// use mpeg2_systems::pes::extension::PStdBuffer;
    /// use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
    /// use mpeg2_systems::ps::mux::{DecoderBufferBound, Error, Muxer, MuxerConfig};
    /// use mpeg2_systems::ps::psm::ElementaryStream;
    /// use mpeg2_systems::stream_type::StreamType;
    /// use mpeg2_systems::time::Timestamp;
    ///
    /// let video = StreamId::from(0xe0);
    /// let mut muxer = Muxer::new(MuxerConfig::new(3_528).with_csps(true))?;
    /// // A VBV of 40 KiB, at 1.15 Mbit/s
    /// muxer.add_stream(
    ///     ElementaryStream::new(StreamType::ISO_IEC_11172_2_VIDEO, video),
    ///     PStdBuffer::new(true, 46),
    /// )?;
    /// muxer.set_decoder_buffer_bound(
    ///     video,
    ///     None,
    ///     DecoderBufferBound::Vbv { size: 40_960, rv_max: 1_150_000 },
    /// )?;
    /// let access_unit = PesPacket::new(video, &[0xaa; 100])
    ///     .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(90_000)?));
    /// muxer.handle_pes_packet(&access_unit)?;
    ///
    /// // A P-STD buffer past the VBV and 6 KiB is refused
    /// let mut muxer = Muxer::new(MuxerConfig::new(3_528).with_csps(true))?;
    /// muxer.add_stream(
    ///     ElementaryStream::new(StreamType::ISO_IEC_11172_2_VIDEO, video),
    ///     PStdBuffer::new(true, 47),
    /// )?;
    /// muxer.set_decoder_buffer_bound(
    ///     video,
    ///     None,
    ///     DecoderBufferBound::Vbv { size: 40_960, rv_max: 1_150_000 },
    /// )?;
    /// assert!(matches!(
    ///     muxer.handle_pes_packet(&access_unit),
    ///     Err(Error::CspsBufferSize { .. })
    /// ));
    /// # Ok::<(), Box<dyn core::error::Error>>(())
    /// ```
    pub fn set_decoder_buffer_bound(
        &mut self,
        stream_id: StreamId,
        stream_id_extension: Option<u8>,
        decoder_buffer_bound: DecoderBufferBound,
    ) -> Result<(), Error> {
        if stream_id_extension.is_some() != (stream_id == StreamId::EXTENDED_STREAM_ID) {
            return Err(Error::StreamIdExtension { stream_id });
        }
        let Some((index, registered)) = self.stream(stream_id, stream_id_extension) else {
            return Err(Error::UnregisteredStream {
                stream_id,
                stream_id_extension,
            });
        };
        if !csps::is_declarable(stream_id, registered.stream_type, decoder_buffer_bound) {
            return Err(Error::DecoderBufferBound {
                stream_id,
                stream_id_extension,
                decoder_buffer_bound,
            });
        }
        if let Some(registered) = self.streams.get_mut(index) {
            registered.decoder_buffer_bound = Some(decoder_buffer_bound);
        }
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

/// Returns the bytes of `system_header`, its bounds those of `streams`, and
/// of the program stream map of `program_stream_info` and `streams`,
/// `program_stream_map_version` `version`, as [`write_headers`] writes them,
/// or refuses them where [`SystemHeader::write`] or
/// [`ProgramStreamMap::write`] does
pub(super) fn check_headers(
    streams: &[Stream],
    program_stream_info: &[u8],
    version: VersionNumber,
    fields: SystemHeaderFields,
) -> Result<(usize, usize), Error> {
    let mut headers = Vec::new();
    let system_header_length = write_headers(
        streams,
        program_stream_info,
        version,
        fields,
        &mut Vec::new(),
        &mut headers,
    )?;
    Ok((
        system_header_length,
        headers.len().saturating_sub(system_header_length),
    ))
}
