//! The streams registered with a [`Muxer`](super::Muxer), the system header
//! and the program stream map made of them, and the changes of the map

use alloc::vec::Vec;

use super::csps::{self, BufferSizes, Constraints, DecoderBufferBound};
use super::{Error, PackWriter, pack_writer};
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
    stream_id: StreamId,
    stream_id_extension: Option<u8>,
    stream_type: StreamType,
    p_std_buffer: PStdBuffer,
    es_info: Vec<u8>,
    /// The time of the STC the DTS (the PTS without one) of the last PES
    /// packet given on the stream samples, `None` before the first
    // Why not a flag for the first PES packet besides it: the first of a
    // stream that carries the optional header has a PTS, so this is set
    // whenever that stream has had one.
    // Why not the clock of the muxer: the clock follows every stream, and a
    // stream may run behind another.
    last_timestamp: Option<SystemTimeClock>,
    /// The bound of §2.7.9 the caller has declared for the stream, `None`
    /// before it does
    // Why not a table of bounds in `MuxerConfig`: it would hold the streams
    // by their ids a second time, apart from their registration.
    decoder_buffer_bound: Option<DecoderBufferBound>,
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

    /// Returns the P-STD buffer size registered, which a PES packet of the
    /// stream that carries `given`, or none, carries in addition: where it
    /// is the first PES packet of the stream, has the optional header
    /// (`optional_header`) and carries none (§2.7.7)
    ///
    /// # Errors
    ///
    /// * [`PStdBufferSize`](Error::PStdBufferSize): `given` is larger in
    ///   bytes than the bound registered (§2.5.3.6, §2.4.3.7).
    /// * [`CspsBufferSize`](Error::CspsBufferSize): the P-STD buffer size the
    ///   PES packet carries is outside the bound `constraints` hold the
    ///   stream to (§2.7.9).
    ///
    /// # Panics
    ///
    /// Panics if `constraints` bound the P-STD buffer size of the stream by
    /// a bound [`StreamDefinition::set_decoder_buffer_bound`] has not
    /// declared.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub(super) fn p_std_buffer_to_add(
        &self,
        given: Option<PStdBuffer>,
        optional_header: bool,
        constraints: Constraints,
    ) -> Result<Option<PStdBuffer>, Error> {
        let stream_id = self.stream_id;
        let stream_id_extension = self.stream_id_extension;
        if let Some(p_std_buffer) = given
            && p_std_buffer.size_in_bytes() > self.p_std_buffer.size_in_bytes()
        {
            return Err(Error::PStdBufferSize {
                stream_id,
                stream_id_extension,
                p_std_buffer,
                bound: self.p_std_buffer,
            });
        }
        let added = (self.last_timestamp.is_none() && optional_header && given.is_none())
            .then_some(self.p_std_buffer);
        if let Some(p_std_buffer) = given.or(added) {
            match constraints.buffer_sizes(stream_id, self.stream_type, self.decoder_buffer_bound) {
                BufferSizes::Within(sizes) if !sizes.contains(&p_std_buffer.size_in_bytes()) => {
                    return Err(Error::CspsBufferSize {
                        stream_id,
                        stream_id_extension,
                        p_std_buffer,
                        smallest: *sizes.start(),
                        largest: *sizes.end(),
                    });
                }
                #[expect(
                    clippy::panic,
                    reason = "a P-STD buffer size written before its bound is declared is the \
                              caller's mistake, which panics as documented"
                )]
                BufferSizes::Undeclared => panic!(
                    "a P-STD buffer size of stream_id {:#04x} is written in a CSPS before \
                     set_decoder_buffer_bound",
                    stream_id.get()
                ),
                BufferSizes::Within(_) | BufferSizes::Unbounded => {}
            }
        }
        Ok(added)
    }

    /// Returns the time of the STC the last PES packet given on the stream
    /// samples, `None` before the first
    pub(super) const fn last_timestamp(&self) -> Option<SystemTimeClock> {
        self.last_timestamp
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

/// Returns whether `audio_bound` counts a stream of `stream_type`: an
/// ISO/IEC 13818-3 or ISO/IEC 11172-3 audio stream (§2.5.3.6)
fn is_audio(stream_type: StreamType) -> bool {
    matches!(
        stream_type,
        StreamType::ISO_IEC_11172_3_AUDIO | StreamType::ISO_IEC_13818_3_AUDIO
    )
}

/// Returns the `audio_bound` and the `video_bound` of `streams`
fn bounds(streams: &[Stream]) -> (usize, usize) {
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
fn check_map_length<'stream>(
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

/// Refuses `stream_id_extension` where it is present and `stream_id` is not
/// 0xFD, or absent and it is
fn check_stream_id_extension(
    stream_id: StreamId,
    stream_id_extension: Option<u8>,
) -> Result<(), Error> {
    if stream_id_extension.is_some() == (stream_id == StreamId::EXTENDED_STREAM_ID) {
        Ok(())
    } else {
        Err(Error::StreamIdExtension { stream_id })
    }
}

/// The streams registered and the descriptors of the program, of which the
/// system header and the program stream map are made
#[derive(Debug, Clone)]
pub(super) struct StreamDefinition {
    streams: Vec<Stream>,
    /// `program_stream_info` of the program stream map
    program_stream_info: Vec<u8>,
}

impl StreamDefinition {
    /// Returns the definition of no stream
    pub(super) const fn new() -> Self {
        Self {
            streams: Vec::new(),
            program_stream_info: Vec::new(),
        }
    }

    /// Returns the time the streams have reached: the latest of the times of
    /// their last PES packets, `None` before the first
    // Why not a field of the time reached: it is the latest `last_timestamp`
    // of the streams, which every time stamp taken sets.
    pub(super) fn clock(&self) -> Option<SystemTimeClock> {
        self.streams
            .iter()
            .filter_map(|stream| stream.last_timestamp)
            .max()
    }

    /// Sets `time` the time of the last PES packet given on the stream at
    /// `index`
    pub(super) fn set_last_timestamp(&mut self, index: usize, time: SystemTimeClock) {
        if let Some(stream) = self.streams.get_mut(index) {
            stream.last_timestamp = Some(time);
        }
    }

    /// Returns the stream registered of `stream_id` and `stream_id_extension`,
    /// and its index
    fn registered(
        &self,
        stream_id: StreamId,
        stream_id_extension: Option<u8>,
    ) -> Option<(usize, &Stream)> {
        self.streams.iter().enumerate().find(|(_, stream)| {
            stream.stream_id == stream_id && stream.stream_id_extension == stream_id_extension
        })
    }

    /// Returns the stream registered of `stream_id` and, when it is 0xFD, of
    /// `stream_id_extension`, and its index
    ///
    /// # Errors
    ///
    /// * [`StreamIdExtension`](Error::StreamIdExtension): `stream_id_extension`
    ///   is present where `stream_id` is not 0xFD, or absent where it is.
    /// * [`UnregisteredStream`](Error::UnregisteredStream): no stream of them
    ///   is registered.
    pub(super) fn find(
        &self,
        stream_id: StreamId,
        stream_id_extension: Option<u8>,
    ) -> Result<(usize, &Stream), Error> {
        check_stream_id_extension(stream_id, stream_id_extension)?;
        self.registered(stream_id, stream_id_extension)
            .ok_or(Error::UnregisteredStream {
                stream_id,
                stream_id_extension,
            })
    }

    /// Returns the definition with `stream` registered after the others, its
    /// bound `p_std_buffer`
    ///
    /// # Errors
    ///
    /// Those of [`Muxer::add_stream`](super::Muxer::add_stream) that refuse
    /// the stream given. The system header and the program stream map of the
    /// definition returned may still be refused, as
    /// [`header_lengths`](Self::header_lengths) says.
    pub(super) fn with_stream(
        &self,
        stream: ElementaryStream<'_>,
        p_std_buffer: PStdBuffer,
    ) -> Result<Self, Error> {
        let stream_id = stream.elementary_stream_id;
        let stream_id_extension = stream.elementary_stream_id_extension;
        if !pack_writer::is_written_in_pes_packets(stream_id) {
            return Err(Error::StreamId { stream_id });
        }
        check_stream_id_extension(stream_id, stream_id_extension)?;
        if self.registered(stream_id, stream_id_extension).is_some() {
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
        let (audio_bound, video_bound) = bounds(&self.streams);
        if is_audio(stream.stream_type)
            && !system_header::is_audio_bound_allowed(audio_bound.saturating_add(1))
        {
            return Err(Error::AudioBound {
                audio_bound: audio_bound.saturating_add(1),
            });
        }
        if stream.stream_type.is_video()
            && !system_header::is_video_bound_allowed(video_bound.saturating_add(1))
        {
            return Err(Error::VideoBound {
                video_bound: video_bound.saturating_add(1),
            });
        }
        let mut definition = self.clone();
        definition.streams.push(Stream {
            stream_id,
            stream_id_extension,
            stream_type: stream.stream_type,
            p_std_buffer,
            es_info: stream.elementary_stream_info.as_bytes().to_vec(),
            last_timestamp: None,
            decoder_buffer_bound: None,
        });
        Ok(definition)
    }

    /// Returns the definition with the `stream_type` and the
    /// `elementary_stream_info` of the stream registered of `stream` replaced,
    /// `None` where that changes nothing; `system_header_written` where a
    /// system header of the streams as they are has been written
    ///
    /// # Errors
    ///
    /// Those of [`Muxer::update_stream`](super::Muxer::update_stream) that refuse
    /// the stream given. The system header and the program stream map of the
    /// definition returned may still be refused, as
    /// [`header_lengths`](Self::header_lengths) says.
    pub(super) fn with_update(
        &self,
        stream: ElementaryStream<'_>,
        system_header_written: bool,
    ) -> Result<Option<Self>, Error> {
        let stream_id = stream.elementary_stream_id;
        let stream_id_extension = stream.elementary_stream_id_extension;
        let (index, registered) = self.find(stream_id, stream_id_extension)?;
        let es_info = stream.elementary_stream_info.as_bytes();
        if (registered.stream_type, registered.es_info.as_slice()) == (stream.stream_type, es_info)
        {
            return Ok(None);
        }
        let counted = |stream_type: StreamType| (is_audio(stream_type), stream_type.is_video());
        if system_header_written && counted(registered.stream_type) != counted(stream.stream_type) {
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
        let mut definition = self.clone();
        if let Some(registered) = definition.streams.get_mut(index) {
            registered.stream_type = stream.stream_type;
            registered.es_info = es_info.to_vec();
        }
        Ok(Some(definition))
    }

    /// Returns the definition with `program_stream_info` in place of the
    /// descriptors before, `None` where that changes nothing
    pub(super) fn with_program_stream_info(&self, program_stream_info: &[u8]) -> Option<Self> {
        (program_stream_info != self.program_stream_info).then(|| {
            let mut definition = self.clone();
            definition.program_stream_info = program_stream_info.to_vec();
            definition
        })
    }

    /// Declares `decoder_buffer_bound` the bound of the stream registered of
    /// `stream_id` and, when it is 0xFD, of `stream_id_extension`
    ///
    /// # Errors
    ///
    /// Those of
    /// [`Muxer::set_decoder_buffer_bound`](super::Muxer::set_decoder_buffer_bound).
    pub(super) fn set_decoder_buffer_bound(
        &mut self,
        stream_id: StreamId,
        stream_id_extension: Option<u8>,
        decoder_buffer_bound: DecoderBufferBound,
    ) -> Result<(), Error> {
        let (index, registered) = self.find(stream_id, stream_id_extension)?;
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

    /// Appends the system header of `fields` and of the streams, and the
    /// program stream map, `program_stream_map_version` `version`, to `out`,
    /// writing their loops in `entries`, and returns the bytes of the system
    /// header
    pub(super) fn write_headers(
        &self,
        version: VersionNumber,
        fields: SystemHeaderFields,
        entries: &mut Vec<u8>,
        out: &mut Vec<u8>,
    ) -> Result<usize, Error> {
        let writer = PackWriter::new();
        let (audio_bound, video_bound) = bounds(&self.streams);
        // Why not an error: registration refuses a stream past either bound,
        // which keeps both within a byte; the system header refuses the rest.
        let [audio_bound, video_bound] =
            [audio_bound, video_bound].map(|bound| u8::try_from(bound).unwrap_or(u8::MAX));
        entries.clear();
        StreamBoundLoop::write_from(self.streams.iter().map(Stream::stream_bound), entries)
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
        ElementaryStreamLoop::write_from(
            self.streams.iter().map(Stream::elementary_stream),
            entries,
        )
        .map_err(|error| Error::ProgramStreamMap { error })?;
        let mut map = ProgramStreamMap::new(version);
        map.program_stream_info = DescriptorLoop::from(self.program_stream_info.as_slice());
        map.elementary_streams = ElementaryStreamLoop::from(entries.as_slice());
        writer.write_program_stream_map(&map, out)?;
        Ok(system_header_length)
    }

    /// Returns the bytes of the system header of `fields` and of the streams,
    /// and of the program stream map, `program_stream_map_version`
    /// `version`, as [`write_headers`](Self::write_headers) writes them, or
    /// refuses them where [`SystemHeader::write`] or
    /// [`ProgramStreamMap::write`] does
    pub(super) fn header_lengths(
        &self,
        version: VersionNumber,
        fields: SystemHeaderFields,
    ) -> Result<(usize, usize), Error> {
        let mut headers = Vec::new();
        let system_header_length =
            self.write_headers(version, fields, &mut Vec::new(), &mut headers)?;
        Ok((
            system_header_length,
            headers.len().saturating_sub(system_header_length),
        ))
    }
}
