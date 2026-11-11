//! Writing a Program Stream: PES packets multiplexed into packs with their
//! SCRs, the system header and the program stream map, or the elements of
//! a Program Stream written one by one
//!
//! A Program Stream is written into a buffer the caller owns, through one of
//! two entry points:
//!
//! - [`Muxer`] takes the PES packets of the streams registered with it, and
//!   writes the packs, their SCRs, the system header and the program stream
//!   map the stream needs on its own.
//! - [`PackWriter`] writes each element given, in the order ISO/IEC 13818-1
//!   §2.5.3 lays out, splitting a payload too long for one PES packet into
//!   several, and holds no state. Where a pack starts, its SCR and the
//!   bounds of a system header are up to the caller.
//!
//! The crate does not write a program stream directory. [`PackWriter`]
//! writes padding and pack stuffing as the caller gives them, and [`Muxer`]
//! writes them at a fixed rate alone. The example writes through
//! [`PackWriter`]; [`Muxer`] has its own.
//!
//! ```
//! use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
//! use mpeg2_systems::ps::demux::{Demuxer, Event};
//! use mpeg2_systems::ps::mux::PackWriter;
//! use mpeg2_systems::ps::{PackHeader, ProgramStreamMap};
//! use mpeg2_systems::psi::VersionNumber;
//! use mpeg2_systems::time::{ClockReference, Timestamp};
//!
//! let writer = PackWriter::new();
//! let mut stream = Vec::new();
//! // A pack: its header, the program stream map, and an access unit too long
//! // for one PES packet
//! let pack_header = PackHeader::new(ClockReference::new(0, 0)?, 2_000);
//! writer.write_pack_header(&pack_header, &mut stream)?;
//! let map = ProgramStreamMap::new(VersionNumber::try_from(0)?);
//! writer.write_program_stream_map(&map, &mut stream)?;
//! let access_unit = vec![0xaa; 100_000];
//! let packet = PesPacket::new(StreamId::from(0xe0), &access_unit)
//!     .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(90_000)?));
//! assert_eq!(writer.write_pes_packets(&packet, &mut stream)?, 2);
//! // The end of the Program Stream
//! writer.write_program_end_code(&mut stream);
//!
//! // Read back, the payloads of the PES packets join into the access unit
//! let mut demuxer = Demuxer::new();
//! demuxer.handle_input(&stream);
//! demuxer.finish();
//! let mut payload = Vec::new();
//! while let Some(event) = demuxer.poll_event() {
//!     if let Event::PesPacket(packet) = event {
//!         payload.extend_from_slice(packet.payload);
//!     }
//! }
//! assert_eq!(payload, access_unit);
//! # Ok::<(), Box<dyn core::error::Error>>(())
//! ```

use alloc::vec::Vec;

use crate::descriptor::DescriptorLoop;
use crate::field::write_or_truncate;
use crate::pes::extension::{PStdBuffer, PesExtension};
use crate::pes::{PesPacket, StreamId};
use crate::ps::pack_header;
use crate::ps::psm::ElementaryStream;
use crate::time::{StreamTime, stream_time};
use csps::Constraints;
use headers::Headers;
use pack_writer::PesPacketPieces;
use schedule::{Gap, Pack, PlannedPack, Rate};
use stream_definition::{StreamDefinition, SystemHeaderFields};

mod config;
mod csps;
mod error;
mod headers;
mod pack_writer;
mod schedule;
mod stream_definition;

pub use config::MuxerConfig;
pub use csps::DecoderBufferBound;
pub use error::Error;
pub use pack_writer::PackWriter;

/// Multiplexer of PES packets into a Program Stream, which writes the packs,
/// their SCRs, the system header and the program stream map the stream
/// needs on its own
///
/// The streams are registered first, then each PES packet is given with
/// [`handle_pes_packet`](Self::handle_pes_packet), in the order of their DTS
/// (their PTS when they have none) across the streams, and the packs are
/// taken with [`poll_output`](Self::poll_output). The output may be held
/// until `poll_output` is called; after [`finish`](Self::finish),
/// `poll_output` gives all that is held, then `MPEG_program_end_code`.
///
/// - A pack is at most [`MuxerConfig::pack_size`] bytes and at most the
///   bytes that arrive within 0.7 s at [`MuxerConfig::program_mux_rate`].
///   It carries one PES packet given, or at a fixed rate padding
///   ([`MuxerConfig::fixed_rate`]). A PES packet too long for one pack is
///   split into several, each in a pack of its own: the first has the
///   optional header given, the ones after it the same header without its
///   PTS, DTS and ESCR.
/// - The first pack has the system header immediately after its pack
///   header (ISO/IEC 13818-1 §2.7.8), then the program stream map. Both go
///   again in the first pack of a PES packet [`MuxerConfig::header_interval`]
///   or more after the last pack that carried them, and in the first pack of
///   the next PES packet after the map changes; in a CSPS at a fixed rate,
///   each in a pack of its own before that pack instead
///   ([`update_stream`](Self::update_stream),
///   [`set_program_stream_info`](Self::set_program_stream_info)). Every
///   system header is the same: `rate_bound`
///   [`MuxerConfig::program_mux_rate`], `fixed_flag` and `CSPS_flag` as
///   [`MuxerConfig::fixed_rate`] and [`MuxerConfig::csps`] set them, no other
///   flag set, an `audio_bound` of the
///   streams of `stream_type` 0x03 and 0x04, a `video_bound` of the video
///   streams, and the bound of each stream registered. The program stream
///   map has the `program_stream_info` set and names each stream with its
///   `stream_type` and descriptors; its `program_stream_map_version` is 0,
///   incremented by 1 modulo 32 when the map changes after it has been sent
///   (§2.5.4.2). The muxer takes for video the streams of `stream_type` 0x01,
///   0x02, 0x10, 0x1B, 0x21, 0x24, 0x32, 0x33 and 0x35.
/// - The first PES packet of a stream that carries the optional header has
///   the P-STD buffer size registered in its PES extension, unless it has
///   one of its own (§2.7.7). A P-STD buffer size a PES packet carries is
///   written as given, and one larger in bytes than the bound registered is
///   refused (§2.5.3.6, §2.4.3.7). The PTS and the DTS are written as given.
/// - The time of the stream is the DTS of each PES packet (its PTS when it
///   has none), held from going back, and counted on past the wrap of the
///   33 bits; a PES packet without either does not move it. The SCR of a
///   pack is the later of the time of its PES packet, less
///   [`MuxerConfig::mux_delay`], and the SCR of the pack before plus the
///   time its bytes take to arrive at `program_mux_rate` (§2.5.2.2 equation
///   2-21), rounded up to a tick of the 27 MHz system clock. A pack that
///   carries the rest of a split PES packet takes the latter.
/// - A leap of the time stamps is filled with packs of a pack header alone,
///   so that consecutive SCRs are never more than 0.7 s apart (§2.7.1).
/// - At a [`MuxerConfig::fixed_rate`], the SCR of each pack is on the line
///   of §2.5.3.6 equations 2-22 and 2-23 through the first, the bytes of one
///   pack after another arriving at `program_mux_rate`, and the time before
///   a PES packet is filled with packs of padding in place of a leap.
/// - In a CSPS ([`MuxerConfig::csps`]), the packets of each pack arrive no
///   faster than §2.7.9 allows, and the P-STD buffer size of each stream is
///   within its bound ([`set_decoder_buffer_bound`](Self::set_decoder_buffer_bound)).
///
/// The muxer does not guarantee that the stream fits the buffers of the
/// P-STD.
/// It writes padding and pack stuffing only at a fixed rate.
/// It does not read the elementary streams for the bounds of a CSPS; the
/// caller declares those their `stream_id` and `stream_type` do not say.
/// It does not pace the output in real time; the caller sends it at the pace
/// its SCRs give.
/// It does not check the order of the PES packets across the streams; the
/// caller gives them in the order of their DTS.
/// It does not send a program stream map ahead of when it applies;
/// `current_next_indicator` is `'1'`.
///
/// ```
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
///
/// // Register the stream, its P-STD buffer 224 KiB
/// let buffer = PStdBuffer::new(true, 224);
/// muxer.add_stream(ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, video), buffer)?;
/// // Give an access unit a second in, and take the packs
/// let access_unit = vec![0xaa; 5_000];
/// let packet = PesPacket::new(video, &access_unit)
///     .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(90_000)?));
/// muxer.handle_pes_packet(&packet)?;
/// muxer.poll_output(&mut stream);
/// // The end, and what is still held
/// muxer.finish();
/// muxer.poll_output(&mut stream);
///
/// // Read back, the payloads of the PES packets join into the access unit
/// let mut demuxer = Demuxer::new();
/// demuxer.handle_input(&stream);
/// demuxer.finish();
/// let mut payload = Vec::new();
/// let mut ended = false;
/// while let Some(event) = demuxer.poll_event() {
///     match event {
///         Event::PesPacket(packet) => payload.extend_from_slice(packet.payload),
///         Event::ProgramEnd => ended = true,
///         _ => {}
///     }
/// }
/// assert_eq!((payload, ended), (access_unit, true));
/// # Ok::<(), Box<dyn core::error::Error>>(())
/// ```
#[doc(alias("multiplexer", "mux"))]
#[derive(Debug, Clone)]
pub struct Muxer {
    config: MuxerConfig,
    definition: StreamDefinition,
    headers: Headers,
    /// The last pack written, `None` before the first PES packet
    last_pack: Option<Pack>,
    /// The packs [`poll_output`](Self::poll_output) gives next
    // Why not write into the caller's buffer on input: only
    // `handle_pes_packet` can fail, and an input refused must leave nothing.
    pending: Vec<u8>,
    end: End,
}

/// Where the input of the muxer is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// PES packets are taken
    Open,
    /// [`Muxer::finish`] has been called, and `MPEG_program_end_code` is
    /// still to give
    Finished,
    /// `MPEG_program_end_code` has been given
    Ended,
}

impl Muxer {
    /// Returns a muxer of `config` with nothing registered
    ///
    /// # Errors
    ///
    /// * [`ProgramMuxRate`](Error::ProgramMuxRate):
    ///   [`MuxerConfig::program_mux_rate`] is 0 or does not fit 22 bits
    ///   (§2.5.3.4).
    /// * [`PackSize`](Error::PackSize): [`MuxerConfig::pack_size`] cannot
    ///   hold a pack header, the shortest PES packet header and a byte of
    ///   payload, or in a CSPS at a fixed rate the shortest pack of one
    ///   packet that the packet rate asks for (§2.7.9), in which no system
    ///   header could go.
    pub fn new(config: MuxerConfig) -> Result<Self, Error> {
        let program_mux_rate = config.program_mux_rate;
        if !pack_header::is_program_mux_rate_allowed(program_mux_rate) {
            return Err(Error::ProgramMuxRate { program_mux_rate });
        }
        let pack_size = config.pack_size;
        // Why not refuse a program_mux_rate the shortest pack does not arrive
        // at within 0.7 s: it is shorter than what arrives at a rate of 1.
        if pack_size < schedule::shortest_pack() {
            return Err(Error::PackSize { pack_size });
        }
        let muxer = Self {
            config,
            definition: StreamDefinition::new(),
            headers: Headers::new(),
            last_pack: None,
            pending: Vec::new(),
            end: End::Open,
        };
        if muxer.rate().longest_first_pack(0, 0) > muxer.largest_pack() {
            return Err(Error::PackSize { pack_size });
        }
        Ok(muxer)
    }

    /// Returns the fields of the system header the settings give:
    /// `rate_bound` [`MuxerConfig::program_mux_rate`], and the flags of the
    /// rate and of a CSPS
    fn system_header_fields(&self) -> SystemHeaderFields {
        let constraints = self.constraints();
        SystemHeaderFields {
            rate_bound: self.config.program_mux_rate,
            fixed_flag: self.rate().fixed_flag(),
            csps_flag: constraints.csps_flag(),
            packet_rate_restriction_flag: constraints.packet_rate_restriction_flag(),
        }
    }

    /// Returns the most bytes of a pack: [`MuxerConfig::pack_size`], or
    /// fewer where fewer arrive at [`MuxerConfig::program_mux_rate`] within
    /// 0.7 s
    fn largest_pack(&self) -> usize {
        schedule::largest_pack(self.config.pack_size, self.config.program_mux_rate)
    }

    /// Returns the rate the settings ask for, fixed or variable
    // Why not a field: it follows from the settings, each time.
    fn rate(&self) -> Rate {
        let constraints = self.constraints();
        if self.config.fixed_rate {
            Rate::fixed(
                self.config.program_mux_rate,
                self.largest_pack(),
                constraints,
            )
        } else {
            Rate::variable(self.config.program_mux_rate, constraints)
        }
    }

    /// Returns the bounds of §2.7.9 the settings hold the stream to
    fn constraints(&self) -> Constraints {
        if self.config.csps {
            Constraints::csps(self.config.program_mux_rate)
        } else {
            Constraints::none()
        }
    }

    /// Takes `definition` in place of the one before, unless its system
    /// header or its program stream map is refused, or the first pack with
    /// them could not carry a PES packet
    fn replace_definition(&mut self, definition: StreamDefinition) -> Result<(), Error> {
        let (system_header, map) =
            definition.header_lengths(self.headers.map_version(), self.system_header_fields())?;
        let length = self.rate().longest_first_pack(system_header, map);
        if length > self.largest_pack() {
            return Err(Error::PackLength { length });
        }
        self.definition = definition;
        Ok(())
    }

    /// Takes `packet`, a PES packet of a stream registered, in the packs of
    /// its own, for [`poll_output`](Self::poll_output)
    ///
    /// The stream is the `stream_id` of the packet and, when it is 0xFD, the
    /// `stream_id_extension` of its PES extension. An input refused leaves
    /// the muxer as it was.
    ///
    /// # Errors
    ///
    /// * [`StreamIdExtension`](Error::StreamIdExtension): the `stream_id` is
    ///   0xFD and the packet has no `stream_id_extension`.
    /// * [`UnregisteredStream`](Error::UnregisteredStream): no stream of the
    ///   packet is registered.
    /// * [`MissingPts`](Error::MissingPts): the PES packet has no PTS and is
    ///   the first of a stream whose `stream_id` carries the optional header
    ///   ([`StreamId::carries_optional_header`]), or has no PTS or DTS and is
    ///   the first given to the muxer. The first PES packet of any other
    ///   stream needs neither.
    /// * [`PStdBufferSize`](Error::PStdBufferSize): it carries a P-STD buffer
    ///   size larger in bytes than the bound registered for its stream.
    /// * [`CspsBufferSize`](Error::CspsBufferSize): in a CSPS, the P-STD
    ///   buffer size it carries, or the bound registered that its stream's
    ///   first PES packet is given, is outside the bound of §2.7.9 for the
    ///   stream.
    /// * [`DecreasingTimestamp`](Error::DecreasingTimestamp): its DTS, or its
    ///   PTS when it has none, is before that of the PES packet given before
    ///   it on its stream.
    /// * [`PackLength`](Error::PackLength): a pack cannot carry the header
    ///   of the PES packet and a byte of its payload in
    ///   [`MuxerConfig::pack_size`], or in the bytes that arrive within 0.7 s
    ///   at [`MuxerConfig::program_mux_rate`].
    /// * [`TimestampGap`](Error::TimestampGap): the packs that fill the leap
    ///   since the PES packet before, or at a fixed rate the packs of padding
    ///   before it, do not fit
    ///   [`MuxerConfig::buffer_limit`], however much `poll_output` takes.
    /// * [`BufferFull`](Error::BufferFull): the packs would grow the output
    ///   held past [`MuxerConfig::buffer_limit`]; once `poll_output` has
    ///   taken it, the same input may be given again.
    /// * [`LengthUnspecified`](Error::LengthUnspecified): `length_unspecified`
    ///   is set.
    /// * [`PesPacket`](Error::PesPacket): [`PesPacket::write`] refuses the
    ///   packet.
    ///
    /// # Panics
    ///
    /// Panics if [`finish`](Self::finish) has been called, or in a CSPS if
    /// the packet would carry a P-STD buffer size of a stream whose bound
    /// [`set_decoder_buffer_bound`](Self::set_decoder_buffer_bound) has not
    /// declared where the muxer needs it.
    #[expect(
        clippy::panic_in_result_fn,
        reason = "a call out of order is the caller's mistake, which panics as documented"
    )]
    pub fn handle_pes_packet(&mut self, packet: &PesPacket<'_>) -> Result<(), Error> {
        assert!(self.end == End::Open, "a PES packet is given after finish");
        let stream_id = packet.stream_id;
        let header = packet.optional_header.as_ref();
        let stream_id_extension = if stream_id == StreamId::EXTENDED_STREAM_ID {
            Some(
                header
                    .and_then(|header| header.pes_extension.as_ref())
                    .and_then(PesExtension::stream_id_extension)
                    .ok_or(Error::StreamIdExtension { stream_id })?,
            )
        } else {
            None
        };
        let (index, stream) = self.definition.find(stream_id, stream_id_extension)?;
        let clock = self.definition.clock();
        let time = match stream_time(
            clock,
            stream.last_timestamp(),
            stream_id.carries_optional_header(),
            header.and_then(|header| header.pts),
            header.and_then(|header| header.dts),
        ) {
            StreamTime::At(time) => Some(time),
            StreamTime::Unchanged => None,
            StreamTime::MissingPts => {
                return Err(Error::MissingPts {
                    stream_id,
                    stream_id_extension,
                });
            }
            StreamTime::Decreasing { time, previous } => {
                return Err(Error::DecreasingTimestamp {
                    stream_id,
                    stream_id_extension,
                    timestamp: time.to_timestamp(),
                    previous: previous.to_timestamp(),
                });
            }
        };
        let registered_p_std_buffer = stream.p_std_buffer_to_add(
            header
                .and_then(|header| header.pes_extension.as_ref())
                .and_then(|extension| extension.p_std_buffer),
            header.is_some(),
            self.constraints(),
        )?;
        let with_p_std_buffer;
        let packet = match (header, registered_p_std_buffer) {
            (Some(header), Some(p_std_buffer)) => {
                let mut header = header.clone();
                header
                    .pes_extension
                    .get_or_insert_with(PesExtension::new)
                    .p_std_buffer = Some(p_std_buffer);
                let mut packet = packet.clone();
                packet.optional_header = Some(header);
                with_p_std_buffer = packet;
                &with_p_std_buffer
            }
            _ => packet,
        };

        let rate = self.rate();
        let largest_pack = self.largest_pack();
        let system_header_fields = self.system_header_fields();
        let Self {
            config,
            definition,
            headers,
            last_pack,
            pending,
            ..
        } = self;
        let program_mux_rate = config.program_mux_rate;
        let first_at = rate.plan(*last_pack, time, 0, 0).first_scr();
        let headers_due = headers.is_due(first_at, config.header_interval);
        let system_header_length = if headers_due {
            headers.write(|version, loops, out| {
                definition.write_headers(version, system_header_fields, loops, out)
            })?
        } else {
            headers.clear();
            0
        };
        let plan = rate.plan(
            *last_pack,
            time,
            system_header_length,
            headers.bytes().len().saturating_sub(system_header_length),
        );
        let (longest, longest_continued) = plan.longest_pieces(largest_pack);
        let pieces = PesPacketPieces::new(packet, longest, longest_continued);
        let length = plan.first_pes_pack_contents(pieces.shortest_next());
        if length > largest_pack {
            return Err(Error::PackLength { length });
        }
        let gap = plan.gap();
        let gap_length = gap.map_or(0, Gap::bytes);
        if gap_length > config.buffer_limit {
            return Err(Error::TimestampGap {
                gap: clock
                    .zip(time)
                    .map_or(0, |(clock, time)| time.timestamp_ticks_since(clock)),
            });
        }
        let length = plan
            .header_packs()
            .map(|planned| planned.pack.length())
            .chain(
                pieces
                    .clone()
                    .enumerate()
                    .map(|(index, piece)| plan.pes_pack_length(index, piece.encoded_len())),
            )
            .fold(
                gap_length.saturating_add(pending.len()),
                usize::saturating_add,
            );
        if length > config.buffer_limit {
            return Err(Error::BufferFull { length });
        }

        let writer = PackWriter::new();
        let mux_delay = config.mux_delay;
        let mut written = *last_pack;
        write_or_truncate(pending, |out| {
            let write = |planned: PlannedPack, piece: Option<&PesPacket<'_>>, out: &mut Vec<u8>| {
                writer
                    .write_pack(
                        &planned.pack.pack_header(mux_delay, program_mux_rate),
                        headers.bytes().get(planned.headers).unwrap_or_default(),
                        piece,
                        planned.fill,
                        out,
                    )
                    .map(|()| planned.pack)
            };
            for planned in gap
                .into_iter()
                .flat_map(Gap::iter)
                .chain(plan.header_packs())
            {
                written = Some(write(planned, None, out)?);
            }
            for (index, piece) in pieces.enumerate() {
                let Some(planned) = plan.pes_pack(index, piece.encoded_len(), written) else {
                    return Err(Error::MissingPts {
                        stream_id,
                        stream_id_extension,
                    });
                };
                written = Some(write(planned, Some(&piece), out)?);
            }
            Ok(())
        })?;
        *last_pack = written;
        if headers_due {
            headers.sent(plan.headers_scr());
        }
        if let Some(time) = time {
            definition.set_last_timestamp(index, time);
        }
        Ok(())
    }

    /// Appends to `out` all the packs that can be given
    ///
    /// After [`finish`](Self::finish), they are followed by
    /// `MPEG_program_end_code`, given once, when a pack has been written; in
    /// a CSPS, after a pack header alone where the end code would come
    /// sooner than the packet rate allows (§2.7.9).
    pub fn poll_output(&mut self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.pending);
        self.pending.clear();
        if self.end == End::Finished {
            if let Some(last) = self.last_pack {
                let writer = PackWriter::new();
                if let Some(closing) = self.rate().closing(last) {
                    let header =
                        closing.pack_header(self.config.mux_delay, self.config.program_mux_rate);
                    // Why not report the error: a pack header is refused only
                    // for stuffing past 7 bytes or a program_mux_rate `new`
                    // refuses.
                    let _ = writer.write_pack_header(&header, out);
                }
                writer.write_program_end_code(out);
            }
            self.end = End::Ended;
        }
    }

    /// Declares the end of the input, after which
    /// [`poll_output`](Self::poll_output) gives all that is held
    ///
    /// It writes nothing itself; a second call does nothing.
    pub fn finish(&mut self) {
        if self.end == End::Open {
            self.end = End::Finished;
        }
    }

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
    ///   [`ProgramStreamMap`](crate::ps::ProgramStreamMap), and those below it
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
        let definition = self.definition.with_stream(stream, p_std_buffer)?;
        self.replace_definition(definition)
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
        let Some(definition) = self
            .definition
            .with_update(stream, self.last_pack.is_some())?
        else {
            return Ok(());
        };
        self.replace_definition(definition)?;
        self.headers.map_changed();
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
    pub fn set_program_stream_info(
        &mut self,
        program_stream_info: DescriptorLoop<'_>,
    ) -> Result<(), Error> {
        let Some(definition) = self
            .definition
            .with_program_stream_info(program_stream_info.as_bytes())
        else {
            return Ok(());
        };
        self.replace_definition(definition)?;
        self.headers.map_changed();
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
        self.definition.set_decoder_buffer_bound(
            stream_id,
            stream_id_extension,
            decoder_buffer_bound,
        )
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "a stream is cut at the lengths it gives"
)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::time::Duration;

    use super::*;
    use crate::descriptor;
    use crate::descriptor::DescriptorLoop;
    use crate::pes::OptionalHeader;
    use crate::pes::extension::PStdBuffer;
    use crate::ps::PackHeader;
    use crate::ps::psm::{ElementaryStream, ElementaryStreamLoop};
    use crate::ps::{
        ProgramStreamMap, StreamBound, StreamBoundLoop, SystemHeader, psm, system_header,
    };
    use crate::psi::VersionNumber;
    use crate::stream_type::StreamType;
    use crate::time::{ClockReference, Timestamp};

    const VIDEO: u8 = 0xe0;
    const AUDIO: u8 = 0xc0;

    /// The P-STD buffer registered for the video stream, 224 KiB
    const VIDEO_BUFFER: PStdBuffer = PStdBuffer::new(true, 224);

    /// The P-STD buffer registered for the audio stream, 4 KiB
    const AUDIO_BUFFER: PStdBuffer = PStdBuffer::new(false, 32);

    /// The `program_mux_rate` of the tests, 1 260 000 bytes a second
    const RATE: u32 = 25_200;

    fn timestamp(value: u64) -> Timestamp {
        Timestamp::try_from(value).unwrap()
    }

    /// A PES packet of `stream_id` carrying `payload`, with a PTS of `pts` 90
    /// kHz ticks, or no time stamp
    fn pes_packet(stream_id: u8, payload: &[u8], pts: Option<u64>) -> PesPacket<'_> {
        let mut header = OptionalHeader::new();
        header.pts = pts.map(timestamp);
        let mut packet = PesPacket::new(StreamId::from(stream_id), payload);
        packet.optional_header = Some(header);
        packet
    }

    /// A muxer of `config` with a video and an audio stream
    fn muxer_of(config: MuxerConfig) -> Muxer {
        let mut muxer = Muxer::new(config).unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                VIDEO_BUFFER,
            )
            .unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
                AUDIO_BUFFER,
            )
            .unwrap();
        muxer
    }

    fn muxer() -> Muxer {
        muxer_of(MuxerConfig::new(RATE))
    }

    /// Whole descriptors of tag 0x80 that come to `length` bytes, which is not 1
    fn descriptors(length: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut rest = length;
        while rest > 0 {
            let body = rest.saturating_sub(2).min(255);
            bytes.extend([0x80, u8::try_from(body).unwrap()]);
            bytes.extend(core::iter::repeat_n(0xaa, body));
            rest = rest.saturating_sub(body.saturating_add(2));
        }
        bytes
    }

    /// What `muxer` gives of `packets`, each taken before the next is given
    fn multiplexed_by(mut muxer: Muxer, packets: &[PesPacket<'_>]) -> Vec<u8> {
        let mut stream = Vec::new();
        for packet in packets {
            muxer.handle_pes_packet(packet).unwrap();
            muxer.poll_output(&mut stream);
        }
        muxer.finish();
        muxer.poll_output(&mut stream);
        stream
    }

    /// An element of a Program Stream, cut by hand by its start code and length
    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Element<'a> {
        PackHeader(PackHeader),
        SystemHeader(&'a [u8]),
        ProgramStreamMap(&'a [u8]),
        PesPacket(PesPacket<'a>),
        ProgramEnd,
    }

    /// The elements of `bytes`, one after another, with the bytes of each
    fn elements(bytes: &[u8]) -> Vec<(Element<'_>, usize)> {
        let mut elements = Vec::new();
        let mut rest = bytes;
        while let Some(&[0x00, 0x00, 0x01, code]) = rest.first_chunk::<4>() {
            let length = match code {
                0xba => 14 + usize::from(rest[13] & 0x07),
                0xb9 => 4,
                _ => 6 + usize::from(u16::from_be_bytes([rest[4], rest[5]])),
            };
            let (element, after) = rest.split_at(length);
            let element = match code {
                0xba => Element::PackHeader(PackHeader::read(element).unwrap()),
                0xbb => Element::SystemHeader(element),
                0xbc => Element::ProgramStreamMap(element),
                0xb9 => Element::ProgramEnd,
                _ => Element::PesPacket(PesPacket::read(element).unwrap()),
            };
            elements.push((element, length));
            rest = after;
        }
        assert!(rest.is_empty(), "bytes after the elements: {rest:02x?}");
        elements
    }

    /// The PES packets of `bytes`
    fn pes_packets(bytes: &[u8]) -> Vec<PesPacket<'_>> {
        elements(bytes)
            .into_iter()
            .filter_map(|(element, _)| {
                if let Element::PesPacket(packet) = element {
                    Some(packet)
                } else {
                    None
                }
            })
            .collect()
    }

    /// What a pack carries: the index in the stream of the byte that carries
    /// the last bit of its SCR, its SCR in 27 MHz ticks, its bytes, its system
    /// header, its program stream map, and the `stream_id` of each PES packet
    #[derive(Debug, PartialEq)]
    struct Carried<'a> {
        scr_byte: u64,
        scr: u64,
        length: usize,
        system_header: Option<&'a [u8]>,
        map: Option<ProgramStreamMap<'a>>,
        stream_ids: Vec<u8>,
    }

    /// What each pack of `bytes` carries
    fn carried(bytes: &[u8]) -> Vec<Carried<'_>> {
        let mut packs: Vec<Carried<'_>> = Vec::new();
        let mut position: u64 = 0;
        for (element, length) in elements(bytes) {
            if let Element::PackHeader(header) = element {
                packs.push(Carried {
                    scr_byte: position.saturating_add(8),
                    scr: header.system_clock_reference.ticks(),
                    length: 0,
                    system_header: None,
                    map: None,
                    stream_ids: Vec::new(),
                });
            }
            position = position.saturating_add(u64::try_from(length).unwrap());
            let Some(pack) = packs.last_mut() else {
                continue;
            };
            if element != Element::ProgramEnd {
                pack.length = pack.length.saturating_add(length);
            }
            match element {
                Element::SystemHeader(bytes) => pack.system_header = Some(bytes),
                Element::ProgramStreamMap(bytes) => {
                    pack.map = Some(ProgramStreamMap::read(bytes).unwrap());
                }
                Element::PesPacket(packet) => pack.stream_ids.push(packet.stream_id.get()),
                Element::PackHeader(_) | Element::ProgramEnd => {}
            }
        }
        packs
    }

    /// The SCR of each pack of `bytes`, in 27 MHz ticks
    fn scrs(bytes: &[u8]) -> Vec<u64> {
        carried(bytes).into_iter().map(|pack| pack.scr).collect()
    }

    /// The P-STD buffer of each PES packet of `bytes`, by `stream_id`
    fn p_std_buffers(bytes: &[u8]) -> Vec<(u8, Option<PStdBuffer>)> {
        pes_packets(bytes)
            .into_iter()
            .map(|packet| {
                (
                    packet.stream_id.get(),
                    packet
                        .optional_header
                        .and_then(|header| header.pes_extension)
                        .and_then(|extension| extension.p_std_buffer),
                )
            })
            .collect()
    }

    /// The 27 MHz ticks the default `mux_delay` comes to
    const DELAY: u64 = 18_900_000;

    #[test]
    fn a_program_mux_rate_of_0_or_past_22_bits_is_refused() {
        let new = |program_mux_rate| Muxer::new(MuxerConfig::new(program_mux_rate)).map(|_| ());
        assert_eq!(
            [0, 1 << 22, (1 << 22) - 1].map(new),
            [
                Err(Error::ProgramMuxRate {
                    program_mux_rate: 0
                }),
                Err(Error::ProgramMuxRate {
                    program_mux_rate: 1 << 22
                }),
                Ok(()),
            ]
        );
    }

    #[test]
    fn a_pack_size_is_taken_from_the_shortest_pack_up() {
        let new = |pack_size| {
            let mut config = MuxerConfig::new(RATE);
            config.pack_size = pack_size;
            Muxer::new(config).map(|_| ())
        };
        // A pack header of 14 bytes, a PES packet header of 6, and a byte
        assert_eq!(
            [new(20), new(21), new(usize::MAX)],
            [Err(Error::PackSize { pack_size: 20 }), Ok(()), Ok(())]
        );
    }

    #[test]
    fn the_first_pack_carries_the_system_header_and_the_map_of_the_streams_registered() {
        let payload = [0xaa; 100];
        let stream = multiplexed_by(muxer(), &[pes_packet(VIDEO, &payload, Some(90_000))]);

        let mut bounds = Vec::new();
        StreamBoundLoop::write_from(
            [
                StreamBound::new(StreamId::from(VIDEO), true, 224),
                StreamBound::new(StreamId::from(AUDIO), false, 32),
            ],
            &mut bounds,
        )
        .unwrap();
        let mut system_header = SystemHeader::new(RATE, 1, 1);
        system_header.stream_bounds = StreamBoundLoop::from(bounds.as_slice());
        let mut streams = Vec::new();
        ElementaryStreamLoop::write_from(
            [
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
            ],
            &mut streams,
        )
        .unwrap();
        let mut map = ProgramStreamMap::new(VersionNumber(0));
        map.elementary_streams = ElementaryStreamLoop::from(streams.as_slice());
        let mut header = OptionalHeader::new();
        header.pts = Some(timestamp(90_000));
        let mut extension = PesExtension::new();
        extension.p_std_buffer = Some(VIDEO_BUFFER);
        header.pes_extension = Some(extension);
        let mut packet = PesPacket::new(StreamId::from(VIDEO), &payload);
        packet.optional_header = Some(header);
        let writer = PackWriter::new();
        let mut expected = Vec::new();
        writer
            .write_pack_header(
                &PackHeader::new(ClockReference::from_ticks(90_000 * 300 - DELAY), RATE),
                &mut expected,
            )
            .unwrap();
        writer
            .write_system_header(&system_header, &mut expected)
            .unwrap();
        writer
            .write_program_stream_map(&map, &mut expected)
            .unwrap();
        writer.write_pes_packets(&packet, &mut expected).unwrap();
        writer.write_program_end_code(&mut expected);
        assert_eq!(stream, expected);
    }

    #[test]
    fn a_pes_packet_too_long_for_a_pack_is_split_into_packs_of_at_most_pack_size() {
        let payload: Vec<u8> = (0..10_000_u32)
            .map(|index| index.to_be_bytes()[3])
            .collect();
        let stream = multiplexed_by(
            muxer(),
            &[
                pes_packet(VIDEO, &payload, Some(90_000)),
                pes_packet(VIDEO, &payload, Some(93_600)),
            ],
        );

        // The bytes of each pack, and the elements after its pack header
        let mut packs: Vec<(usize, Vec<Element<'_>>)> = Vec::new();
        for (element, length) in elements(&stream) {
            match element {
                Element::PackHeader(_) => packs.push((length, Vec::new())),
                Element::ProgramEnd => {}
                Element::SystemHeader(_) | Element::ProgramStreamMap(_) | Element::PesPacket(_) => {
                    let pack = packs.last_mut().unwrap();
                    pack.0 += length;
                    pack.1.push(element);
                }
            }
        }
        let packets = pes_packets(&stream);
        let joined: Vec<u8> = packets
            .iter()
            .flat_map(|packet| packet.payload)
            .copied()
            .collect();
        let ptss: Vec<Option<u64>> = packets
            .iter()
            .map(|packet| {
                packet
                    .optional_header
                    .as_ref()
                    .and_then(|header| header.pts)
                    .map(Timestamp::get)
            })
            .collect();
        let shapes: Vec<usize> = packs.iter().map(|(_, elements)| elements.len()).collect();
        assert_eq!(
            (
                packs.iter().all(|&(length, _)| length <= 2048),
                &shapes[..2],
                shapes[1..].iter().all(|&count| count == 1),
                joined == [payload.clone(), payload].concat(),
                ptss.iter().flatten().count(),
                ptss.len(),
            ),
            (true, &[3, 1][..], true, true, 2, packs.len())
        );
    }

    #[test]
    fn the_first_pes_packet_of_each_stream_carries_the_p_std_buffer_registered_unless_it_has_its_own()
     {
        let mut own = pes_packet(AUDIO, &[0xbb; 10], Some(90_000));
        let mut extension = PesExtension::new();
        extension.p_std_buffer = Some(PStdBuffer::new(false, 8));
        own.optional_header.as_mut().unwrap().pes_extension = Some(extension);
        let mut private = Muxer::new(MuxerConfig::new(RATE)).unwrap();
        private
            .add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                VIDEO_BUFFER,
            )
            .unwrap();
        private
            .add_stream(
                ElementaryStream::new(
                    StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                    StreamId::PRIVATE_STREAM_2,
                ),
                PStdBuffer::new(false, 8),
            )
            .unwrap();

        assert_eq!(
            (
                p_std_buffers(&multiplexed_by(
                    muxer(),
                    &[
                        pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                        own,
                        pes_packet(VIDEO, &[0xaa; 10], Some(93_600)),
                        pes_packet(AUDIO, &[0xbb; 10], Some(92_160)),
                    ]
                )),
                p_std_buffers(&multiplexed_by(
                    private,
                    &[
                        pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                        PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc; 10]),
                    ]
                ))
            ),
            (
                vec![
                    (VIDEO, Some(VIDEO_BUFFER)),
                    (AUDIO, Some(PStdBuffer::new(false, 8))),
                    (VIDEO, None),
                    (AUDIO, None),
                ],
                vec![(VIDEO, Some(VIDEO_BUFFER)), (0xbf, None)]
            )
        );
    }

    #[test]
    fn a_stream_is_refused_for_its_stream_id_its_extension_a_bound_or_its_descriptors() {
        let add = |muxer: &mut Muxer, value: u8, extension: Option<u8>, es_info: &[u8]| {
            let mut stream = ElementaryStream::new(
                StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                StreamId::from(value),
            )
            .with_elementary_stream_info(DescriptorLoop::from(es_info));
            stream.elementary_stream_id_extension = extension;
            muxer.add_stream(stream, PStdBuffer::new(false, 8))
        };
        let mut muxer = muxer();
        assert_eq!(
            [
                add(&mut muxer, 0xbb, None, &[]),
                add(&mut muxer, 0xbc, None, &[]),
                add(&mut muxer, 0xfd, None, &[]),
                add(&mut muxer, 0xbd, Some(0x55), &[]),
                add(&mut muxer, VIDEO, None, &[]),
                add(&mut muxer, 0xbd, None, &[0x0a, 0x05, 0x00]),
                muxer.add_stream(
                    ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(0xc1)),
                    PStdBuffer::new(true, 8)
                ),
            ],
            [
                Err(Error::StreamId {
                    stream_id: StreamId::from(0xbb)
                }),
                Err(Error::StreamId {
                    stream_id: StreamId::from(0xbc)
                }),
                Err(Error::StreamIdExtension {
                    stream_id: StreamId::from(0xfd)
                }),
                Err(Error::StreamIdExtension {
                    stream_id: StreamId::from(0xbd)
                }),
                Err(Error::StreamInUse {
                    stream_id: StreamId::from(VIDEO),
                    stream_id_extension: None
                }),
                Err(Error::ProgramStreamMap {
                    error: psm::Error::Descriptor(descriptor::Error::Length {
                        descriptor_length: 5,
                        available_bytes: 1
                    })
                }),
                Err(Error::SystemHeader {
                    error: system_header::Error::Forbidden {
                        field: "P-STD_buffer_bound_scale",
                        value: 1
                    }
                }),
            ]
        );
        // The streams refused are not registered
        assert_eq!(
            multiplexed_by(muxer, &[pes_packet(VIDEO, &[0xaa; 10], Some(90_000))]),
            multiplexed_by(
                self::muxer(),
                &[pes_packet(VIDEO, &[0xaa; 10], Some(90_000))]
            )
        );
    }

    #[test]
    fn a_stream_past_32_audio_or_16_video_streams_is_refused() {
        let mut muxer = Muxer::new(MuxerConfig::new(RATE)).unwrap();
        for value in 0xc0..=0xdf {
            muxer
                .add_stream(
                    ElementaryStream::new(StreamType::ISO_IEC_13818_3_AUDIO, StreamId::from(value)),
                    AUDIO_BUFFER,
                )
                .unwrap();
        }
        for value in 0xe0..=0xef {
            muxer
                .add_stream(
                    ElementaryStream::new(StreamType::AVC_VIDEO, StreamId::from(value)),
                    VIDEO_BUFFER,
                )
                .unwrap();
        }
        let add = |muxer: &mut Muxer, stream_type| {
            muxer.add_stream(
                ElementaryStream::new(stream_type, StreamId::PRIVATE_STREAM_1),
                PStdBuffer::new(true, 8),
            )
        };
        assert_eq!(
            [
                add(&mut muxer, StreamType::ISO_IEC_11172_3_AUDIO),
                add(&mut muxer, StreamType::HEVC_VIDEO),
                add(&mut muxer, StreamType::ISO_IEC_13818_7_AUDIO),
            ],
            [
                Err(Error::AudioBound { audio_bound: 33 }),
                Err(Error::VideoBound { video_bound: 17 }),
                Ok(()),
            ]
        );
    }

    #[test]
    fn a_stream_is_refused_whose_first_pack_would_not_fit_pack_size() {
        let mut config = MuxerConfig::new(RATE);
        config.pack_size = 55;
        let mut muxer = Muxer::new(config).unwrap();
        let add = |muxer: &mut Muxer| {
            muxer.add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                VIDEO_BUFFER,
            )
        };
        // 14 of pack header, 15 of system header, 20 of map, 6 of PES packet
        // header and a byte of payload
        let refused = add(&mut muxer);
        config.pack_size = 56;
        let mut fits = Muxer::new(config).unwrap();
        assert_eq!(
            (refused, add(&mut fits)),
            (Err(Error::PackLength { length: 56 }), Ok(()))
        );
    }

    #[test]
    fn a_pes_packet_is_refused_whose_header_and_a_byte_do_not_fit_a_pack() {
        let mut config = MuxerConfig::new(RATE);
        config.pack_size = 100;
        let mut muxer = muxer_of(config);
        let mut packet = pes_packet(VIDEO, &[0xaa; 10], Some(90_000));
        packet.optional_header.as_mut().unwrap().stuffing_bytes = 40;
        // 14 + 18 + 24 + 6 + 3 + 5 of PTS + 3 of P-STD buffer + 40 + 1
        assert_eq!(
            muxer.handle_pes_packet(&packet),
            Err(Error::PackLength { length: 114 })
        );
    }

    #[test]
    fn a_stream_of_stream_id_0xfd_is_told_by_the_stream_id_extension_of_its_packets() {
        let extended = |extension: Option<u8>, payload: &'static [u8]| {
            let mut packet = pes_packet(0xfd, payload, Some(90_000));
            let mut pes_extension = PesExtension::new();
            pes_extension.pes_extension_field = extension.map(|extension| match extension {
                0x55 => &[0x55][..],
                0x56 => &[0x56][..],
                _ => &[0x80][..],
            });
            packet.optional_header.as_mut().unwrap().pes_extension = Some(pes_extension);
            packet
        };
        let mut muxer = Muxer::new(MuxerConfig::new(RATE)).unwrap();
        for extension in [0x55, 0x56] {
            muxer
                .add_stream(
                    ElementaryStream::new(
                        StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                        StreamId::EXTENDED_STREAM_ID,
                    )
                    .with_elementary_stream_id_extension(extension),
                    PStdBuffer::new(false, 8),
                )
                .unwrap();
        }
        let mut bounds = Vec::new();
        let bound = |extension| {
            let mut bound = StreamBound::new(StreamBound::WITH_STREAM_ID_EXTENSION, false, 8);
            bound.stream_id_extension = Some(extension);
            bound
        };
        StreamBoundLoop::write_from([bound(0x55), bound(0x56)], &mut bounds).unwrap();
        let mut stream = Vec::new();
        let results = [
            muxer.handle_pes_packet(&extended(None, &[0x01])),
            muxer.handle_pes_packet(&extended(Some(0x80), &[0x01])),
            muxer.handle_pes_packet(&extended(Some(0x56), &[0x02])),
        ];
        muxer.poll_output(&mut stream);
        let mut system_header = SystemHeader::new(RATE, 0, 0);
        system_header.stream_bounds = StreamBoundLoop::from(bounds.as_slice());
        let mut header_bytes = Vec::new();
        system_header.write(&mut header_bytes).unwrap();
        let written_header = elements(&stream).into_iter().find_map(|(element, _)| {
            if let Element::SystemHeader(bytes) = element {
                Some(bytes.to_vec())
            } else {
                None
            }
        });
        let payloads: Vec<Vec<u8>> = pes_packets(&stream)
            .into_iter()
            .map(|packet| packet.payload.to_vec())
            .collect();
        let stream_id = StreamId::EXTENDED_STREAM_ID;
        assert_eq!(
            (results, written_header, payloads),
            (
                [
                    Err(Error::StreamIdExtension { stream_id }),
                    Err(Error::StreamIdExtension { stream_id }),
                    Ok(())
                ],
                Some(header_bytes),
                vec![vec![0x02]]
            )
        );
    }

    #[test]
    fn a_pes_packet_of_no_stream_registered_is_refused() {
        assert_eq!(
            muxer().handle_pes_packet(&pes_packet(0xe1, &[0xaa], Some(90_000))),
            Err(Error::UnregisteredStream {
                stream_id: StreamId::from(0xe1),
                stream_id_extension: None
            })
        );
    }

    #[test]
    fn a_stream_whose_first_pes_packet_has_no_pts_or_whose_time_stamp_goes_back_is_refused() {
        let mut muxer = muxer();
        let first = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], None));
        muxer
            .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(90_000)))
            .unwrap();
        let audio_first = muxer.handle_pes_packet(&pes_packet(AUDIO, &[0xbb], None));
        let back = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(89_999)));
        let unstamped = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], None));
        let audio_behind = muxer.handle_pes_packet(&pes_packet(AUDIO, &[0xbb], Some(80_000)));
        let missing_pts = |value| Error::MissingPts {
            stream_id: StreamId::from(value),
            stream_id_extension: None,
        };
        assert_eq!(
            [first, audio_first, back, unstamped, audio_behind],
            [
                Err(missing_pts(VIDEO)),
                Err(missing_pts(AUDIO)),
                Err(Error::DecreasingTimestamp {
                    stream_id: StreamId::from(VIDEO),
                    stream_id_extension: None,
                    timestamp: timestamp(89_999),
                    previous: timestamp(90_000)
                }),
                Ok(()),
                Ok(()),
            ]
        );
    }

    #[test]
    fn a_first_pes_packet_without_a_time_stamp_is_refused_on_a_stream_without_an_optional_header() {
        let mut muxer = Muxer::new(MuxerConfig::new(RATE)).unwrap();
        muxer
            .add_stream(
                ElementaryStream::new(
                    StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                    StreamId::PRIVATE_STREAM_2,
                ),
                PStdBuffer::new(false, 8),
            )
            .unwrap();
        assert_eq!(
            muxer.handle_pes_packet(&PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0xcc])),
            Err(Error::MissingPts {
                stream_id: StreamId::PRIVATE_STREAM_2,
                stream_id_extension: None
            })
        );
    }

    #[test]
    fn a_time_stamp_past_the_wrap_of_33_bits_does_not_go_back() {
        let mut muxer = muxer();
        muxer
            .handle_pes_packet(&pes_packet(
                VIDEO,
                &[0xaa],
                Some(Timestamp::MODULUS - 1_800),
            ))
            .unwrap();
        assert_eq!(
            muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(1_800))),
            Ok(())
        );
    }

    #[test]
    fn a_refused_pes_packet_leaves_the_muxer_as_it_was() {
        let packets = [
            pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
            pes_packet(AUDIO, &[0xbb; 300], Some(91_000)),
            pes_packet(VIDEO, &[0xaa; 3_000], Some(93_600)),
        ];
        let mut config = MuxerConfig::new(RATE);
        config.buffer_limit = 5_000;
        let mut muxer = muxer_of(config);
        let mut stream = Vec::new();
        muxer.handle_pes_packet(&packets[0]).unwrap();
        muxer.handle_pes_packet(&packets[1]).unwrap();
        // 2048 + 1051 + 331 held, and 2048 + 1003 to come
        let full = muxer.handle_pes_packet(&packets[2]);
        muxer.poll_output(&mut stream);
        let mut length_unspecified = packets[2].clone();
        length_unspecified.length_unspecified = true;
        let unspecified = muxer.handle_pes_packet(&length_unspecified);
        muxer.handle_pes_packet(&packets[2]).unwrap();
        muxer.finish();
        muxer.poll_output(&mut stream);
        assert_eq!(
            (full, unspecified, stream),
            (
                Err(Error::BufferFull { length: 6_481 }),
                Err(Error::LengthUnspecified {
                    stream_id: StreamId::from(VIDEO)
                }),
                multiplexed_by(self::muxer(), &packets)
            )
        );
    }

    #[test]
    fn a_buffer_limit_of_0_refuses_every_pes_packet_and_of_usize_max_none() {
        let mut config = MuxerConfig::new(RATE);
        config.buffer_limit = 0;
        let refused = muxer_of(config).handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)));
        config.buffer_limit = usize::MAX;
        let mut muxer = muxer_of(config);
        let taken: Vec<_> = (0..100)
            .map(|index| {
                muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 60_000], Some(index * 3_600)))
            })
            .collect();
        assert_eq!(
            (refused, taken),
            (Err(Error::BufferFull { length: 74 }), vec![Ok(()); 100])
        );
    }

    #[test]
    fn a_leap_too_long_to_fill_within_the_buffer_limit_is_refused_however_much_is_taken() {
        let mut config = MuxerConfig::new(RATE);
        config.buffer_limit = 150;
        let mut muxer = muxer_of(config);
        let mut stream = Vec::new();
        muxer
            .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 10], Some(90_000)))
            .unwrap();
        muxer.poll_output(&mut stream);
        // A leap of 10 s takes 14 packs of a pack header alone, 196 bytes
        let leap = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 10], Some(990_000)));
        muxer
            .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 10], Some(93_600)))
            .unwrap();
        muxer.finish();
        muxer.poll_output(&mut stream);
        assert_eq!(
            (leap, stream),
            (
                Err(Error::TimestampGap { gap: 900_000 }),
                multiplexed_by(
                    self::muxer(),
                    &[
                        pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                        pes_packet(VIDEO, &[0xaa; 10], Some(93_600)),
                    ]
                )
            )
        );
    }

    /// The SCR of each pack the muxer writes of two PES packets of video, the
    /// second `leap` ticks of 90 kHz after the first
    fn scrs_across(leap: u64) -> Vec<u64> {
        scrs(&multiplexed_by(
            muxer(),
            &[
                pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                pes_packet(VIDEO, &[0xaa; 10], Some(leap.saturating_add(90_000))),
            ],
        ))
    }

    #[test]
    fn a_leap_past_0_7_s_is_filled_with_packs_of_a_pack_header_alone_0_7_s_apart() {
        // The 27 MHz ticks of 0.7 s, and the SCR of the first pack
        const LONGEST: u64 = 18_900_000;
        let first = 90_000 * 300 - DELAY;
        // A leap of exactly 0.7 s needs no pack, and one 90 kHz tick more needs
        // one; a leap of 2 s needs ⌈2 / 0.7⌉ − 1 = 2, and one of 3 s 4
        assert_eq!(
            [63_000, 63_001, 180_000, 270_000].map(scrs_across),
            [
                vec![first, first + 63_000 * 300],
                vec![first, first + LONGEST, first + 63_001 * 300],
                vec![
                    first,
                    first + LONGEST,
                    first + 2 * LONGEST,
                    first + 180_000 * 300
                ],
                vec![
                    first,
                    first + LONGEST,
                    first + 2 * LONGEST,
                    first + 3 * LONGEST,
                    first + 4 * LONGEST,
                    first + 270_000 * 300
                ],
            ]
        );
    }

    #[test]
    fn the_scr_is_the_time_less_the_mux_delay_or_after_the_bytes_of_the_pack_before() {
        let mut config = MuxerConfig::new(RATE);
        config.mux_delay = Duration::ZERO;
        let stream = multiplexed_by(
            muxer_of(config),
            &[
                pes_packet(VIDEO, &[0xaa; 10], Some(90_000)),
                pes_packet(AUDIO, &[0xbb; 10], Some(90_000)),
                pes_packet(VIDEO, &[0xaa; 10], None),
                pes_packet(VIDEO, &[0xaa; 10], Some(93_600)),
            ],
        );
        // The first pack of 14 + 18 + 24 + 27 bytes, the next of 14 + 27, at
        // 1 260 000 bytes a second, 27 000 000 / 1 260 000 ticks a byte, rounded
        // up: 1 779 and 879 ticks
        assert_eq!(
            scrs(&stream),
            [
                27_000_000,
                27_000_000 + 1_779,
                27_000_000 + 1_779 + 879,
                93_600 * 300
            ]
        );
    }

    #[test]
    #[should_panic(expected = "a stream is registered after the first PES packet")]
    fn a_stream_registered_after_the_first_pes_packet_panics() {
        let mut muxer = muxer();
        muxer
            .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
            .unwrap();
        let _ = muxer.add_stream(
            ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(0xe1)),
            VIDEO_BUFFER,
        );
    }

    #[test]
    #[should_panic(expected = "a PES packet is given after finish")]
    fn a_pes_packet_after_finish_panics() {
        let mut muxer = muxer();
        muxer.finish();
        let _ = muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)));
    }

    #[test]
    fn after_finish_poll_output_gives_what_is_held_and_the_end_code_once() {
        let mut muxer = muxer();
        muxer
            .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(90_000)))
            .unwrap();
        let mut held = Vec::new();
        muxer.clone().poll_output(&mut held);
        muxer.finish();
        muxer.finish();
        let mut first = Vec::new();
        muxer.poll_output(&mut first);
        let mut second = Vec::new();
        muxer.poll_output(&mut second);
        let mut nothing_given = Muxer::new(MuxerConfig::new(RATE)).unwrap();
        nothing_given.finish();
        let mut empty = Vec::new();
        nothing_given.poll_output(&mut empty);
        assert_eq!(
            (first, second, empty),
            (
                [held, vec![0x00, 0x00, 0x01, 0xb9]].concat(),
                vec![],
                vec![]
            )
        );
    }

    #[test]
    fn a_stream_is_taken_whose_descriptors_fill_the_map_and_refused_one_byte_past_it() {
        // A map of video and audio: 10 bytes besides its loop, two entries of
        // 4, and 4 for the stream added, leave 996 bytes of descriptors
        let add = |es_info: &[u8]| {
            muxer().add_stream(
                ElementaryStream::new(
                    StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA,
                    StreamId::PRIVATE_STREAM_1,
                )
                .with_elementary_stream_info(DescriptorLoop::from(es_info)),
                PStdBuffer::new(false, 8),
            )
        };
        assert_eq!(
            [add(&descriptors(996)), add(&descriptors(997))],
            [
                Ok(()),
                Err(Error::ProgramStreamMap {
                    error: psm::Error::ProgramStreamMapLength {
                        program_stream_map_length: 1_019,
                        limit: 1_018
                    }
                })
            ]
        );
    }

    #[test]
    fn a_pes_packet_too_long_for_the_buffer_limit_is_refused_with_the_bytes_of_all_its_packs() {
        let mut config = MuxerConfig::new(RATE);
        config.pack_size = 128;
        config.buffer_limit = 128;
        let payload = vec![0xaa; 8 << 20];
        // The first pack: 14 + 18 + 24, and a PES packet header of 6 + 3 + 5 of
        // PTS + 3 of P-STD buffer, leaving 55 bytes of payload; the ones after
        // it: 14 and a header of 6 + 3 + 3, leaving 102. 82 241 packs follow
        // the first.
        let length = 42 + 82_242 * 14 + 17 + (8 << 20) + 82_241 * 12;
        assert_eq!(
            muxer_of(config).handle_pes_packet(&pes_packet(VIDEO, &payload, Some(90_000))),
            Err(Error::BufferFull { length })
        );
    }

    #[test]
    fn a_stream_is_refused_whose_first_pack_would_not_arrive_within_0_7_s() {
        // 100 bytes a second: 70 bytes arrive in 0.7 s. A first pack of 14 +
        // 15 of system header + 20 of map with the descriptors + 6 + 1 is 56
        // bytes, and 70 with 14 bytes of descriptors
        let add = |es_info: &[u8]| {
            Muxer::new(MuxerConfig::new(2)).unwrap().add_stream(
                ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO))
                    .with_elementary_stream_info(DescriptorLoop::from(es_info)),
                VIDEO_BUFFER,
            )
        };
        let descriptor = |length: u8| {
            let mut bytes = vec![0x80, length - 2];
            bytes.extend(core::iter::repeat_n(0xaa, usize::from(length - 2)));
            bytes
        };
        assert_eq!(
            [add(&descriptor(14)), add(&descriptor(15))],
            [Ok(()), Err(Error::PackLength { length: 71 })]
        );
    }

    #[test]
    fn a_p_std_buffer_size_past_the_bound_registered_is_refused_on_any_pes_packet() {
        // A bound of 1 KiB, and PES packets whose own P-STD buffer is counted in
        // units of 128 bytes
        let with_buffer = |pts: u64, p_std_buffer: PStdBuffer| {
            let mut packet = pes_packet(VIDEO, &[0xaa; 10], Some(pts));
            let mut extension = PesExtension::new();
            extension.p_std_buffer = Some(p_std_buffer);
            packet.optional_header.as_mut().unwrap().pes_extension = Some(extension);
            packet
        };
        let registered = || {
            let mut muxer = Muxer::new(MuxerConfig::new(RATE)).unwrap();
            muxer
                .add_stream(
                    ElementaryStream::new(StreamType::ISO_IEC_13818_2_VIDEO, StreamId::from(VIDEO)),
                    PStdBuffer::new(true, 1),
                )
                .unwrap();
            muxer
        };
        let past = |p_std_buffer| Error::PStdBufferSize {
            stream_id: StreamId::from(VIDEO),
            stream_id_extension: None,
            p_std_buffer,
            bound: PStdBuffer::new(true, 1),
        };
        let mut muxer = registered();
        let first = [
            muxer.handle_pes_packet(&with_buffer(90_000, PStdBuffer::new(false, 9))),
            muxer.handle_pes_packet(&with_buffer(90_000, PStdBuffer::new(false, 8))),
        ];
        let later = [
            muxer.handle_pes_packet(&with_buffer(93_600, PStdBuffer::new(true, 2))),
            muxer.handle_pes_packet(&with_buffer(93_600, PStdBuffer::new(true, 1))),
        ];
        let mut stream = Vec::new();
        muxer.finish();
        muxer.poll_output(&mut stream);
        assert_eq!(
            (first, later, stream),
            (
                [Err(past(PStdBuffer::new(false, 9))), Ok(())],
                [Err(past(PStdBuffer::new(true, 2))), Ok(())],
                multiplexed_by(
                    registered(),
                    &[
                        with_buffer(90_000, PStdBuffer::new(false, 8)),
                        with_buffer(93_600, PStdBuffer::new(true, 1)),
                    ]
                )
            )
        );
    }

    mod csps {
        //! A constrained system parameter stream: the packet rate of equation 2-28,
        //! and the P-STD buffer of each stream within the bound of §2.7.9

        use super::*;

        /// The `program_mux_rate` of a Video CD, 176 400 bytes a second, at which
        /// equation 2-28 asks 1/300 s a packet
        const VCD_RATE: u32 = 3_528;

        /// The `pack_size` of a Video CD
        const VCD_PACK_SIZE: usize = 2_324;

        /// The VBV declared for the video stream, under the 224 KiB registered for
        /// it by at most the 6 KiB `BSadd` allows
        const VBV: DecoderBufferBound = DecoderBufferBound::Vbv {
            size: 220 * 1024,
            rv_max: 1_150_000,
        };

        /// A muxer of `config`, a CSPS, with a video and an audio stream, the VBV
        /// of the video declared
        fn csps_muxer(config: MuxerConfig) -> Muxer {
            let mut muxer = muxer_of(config.with_csps(true));
            muxer
                .set_decoder_buffer_bound(StreamId::from(VIDEO), None, VBV)
                .unwrap();
            muxer
        }

        /// Whether each pack of `stream`, a Program Stream of `rate_bound` and
        /// `program_mux_rate` `rate`, meets equation 2-28: NP ≤ (t(i′) − t(i)) × 300 ×
        /// max[1, `R_max` / 2.5 × 10⁶], `R_max` = 400 × `rate_bound` bits a second, t(i′)
        /// for the last pack the arrival of the last byte of the end code
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "the products of the equation are taken in u128, far from its bounds"
        )]
        fn meets_the_packet_rate(stream: &[u8], rate: u32) -> Vec<bool> {
            let packs = carried(stream);
            let knee: u128 = 2_500_000;
            let allowed = 300 * knee.max(400 * u128::from(rate));
            let end_code_last_byte = u64::try_from(stream.len()).unwrap() - 1;
            packs
                .iter()
                .enumerate()
                .map(|(index, pack)| {
                    let packets = u128::from(pack.system_header.is_some())
                        + u128::from(pack.map.is_some())
                        + u128::try_from(pack.stream_ids.len()).unwrap();
                    // NP × X ≤ Δt × 300 × max[X, R_max], Δt in seconds
                    match packs.get(index + 1) {
                        Some(next) => {
                            packets * knee * 27_000_000 <= u128::from(next.scr - pack.scr) * allowed
                        }
                        None => {
                            let bytes = u128::from(end_code_last_byte - pack.scr_byte);
                            packets * knee * 50 * u128::from(rate) <= bytes * allowed
                        }
                    }
                })
                .collect()
        }

        /// Audio PES packets of 100 bytes 1 ms apart, faster than a Video CD may
        /// carry packets
        fn audio_every_millisecond() -> Vec<PesPacket<'static>> {
            (90_000..90_900)
                .step_by(90)
                .map(|pts| pes_packet(AUDIO, &[0xbb; 100], Some(pts)))
                .collect()
        }

        #[test]
        fn at_a_variable_rate_each_pack_waits_for_the_packet_rate_of_equation_2_28() {
            let inputs = audio_every_millisecond();
            let csps = multiplexed_by(csps_muxer(MuxerConfig::new(VCD_RATE)), &inputs);
            let not_csps = multiplexed_by(muxer_of(MuxerConfig::new(VCD_RATE)), &inputs);

            let met = meets_the_packet_rate(&csps, VCD_RATE);
            assert_eq!(met, vec![true; met.len()]);
            assert!(meets_the_packet_rate(&not_csps, VCD_RATE).contains(&false));
            assert_eq!(pes_packets(&csps), pes_packets(&not_csps));
        }

        #[test]
        fn a_pack_header_alone_goes_before_the_end_code_where_it_would_come_sooner_than_the_packet_rate()
         {
            let inputs = [pes_packet(AUDIO, &[0xbb; 100], Some(90_000))];
            let csps = multiplexed_by(csps_muxer(MuxerConfig::new(VCD_RATE)), &inputs);
            let not_csps = multiplexed_by(muxer_of(MuxerConfig::new(VCD_RATE)), &inputs);
            let tail = |stream: &[u8]| -> Vec<bool> {
                elements(stream)
                    .into_iter()
                    .map(|(element, _)| matches!(element, Element::PackHeader(_)))
                    .rev()
                    .take(3)
                    .collect()
            };

            // The first pack carries 3 packets: 3/300 s, past the 1 ms its bytes
            // take at 176 400 bytes a second
            assert_eq!(meets_the_packet_rate(&csps, VCD_RATE), [true, true]);
            assert_eq!(meets_the_packet_rate(&not_csps, VCD_RATE), [false]);
            assert_eq!(
                (tail(&csps), tail(&not_csps)),
                (vec![false, true, false], vec![false, false, false])
            );
        }

        /// PES packets of video and audio, one split over packs, with a leap
        fn video_and_audio() -> Vec<PesPacket<'static>> {
            vec![
                pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
                pes_packet(AUDIO, &[0xbb; 300], Some(91_000)),
                pes_packet(VIDEO, &[0xaa; 100], Some(93_600)),
                pes_packet(AUDIO, &[0xbb; 30], Some(95_000)),
                pes_packet(VIDEO, &[0xaa; 5_000], Some(97_200)),
                pes_packet(VIDEO, &[0xaa; 100], Some(232_200)),
            ]
        }

        fn vcd() -> MuxerConfig {
            MuxerConfig::new(VCD_RATE)
                .with_pack_size(VCD_PACK_SIZE)
                .with_fixed_rate(true)
        }

        #[test]
        fn at_a_fixed_rate_each_pack_is_filled_to_the_packet_rate_on_the_line() {
            let stream = multiplexed_by(csps_muxer(vcd()), &video_and_audio());
            let packs = carried(&stream);

            let met = meets_the_packet_rate(&stream, VCD_RATE);
            assert_eq!(met, vec![true; met.len()]);
            let (first_at, first) = (packs[0].scr_byte, packs[0].scr);
            let scrs: Vec<_> = packs.iter().map(|pack| pack.scr).collect();
            let on_line: Vec<_> = packs
                .iter()
                .map(|pack| {
                    first + (pack.scr_byte - first_at) * 27_000_000 / (50 * u64::from(VCD_RATE))
                })
                .collect();
            assert_eq!(scrs, on_line);
            assert_eq!(
                payloads(&stream),
                payloads(&multiplexed_by(muxer_of(vcd()), &video_and_audio()))
            );
        }

        /// The payloads of the PES packets of `stream` joined by `stream_id`, those
        /// of `padding_stream` left out
        fn payloads(stream: &[u8]) -> Vec<(u8, Vec<u8>)> {
            let mut payloads: Vec<(u8, Vec<u8>)> = Vec::new();
            for packet in pes_packets(stream) {
                let stream_id = packet.stream_id.get();
                if packet.stream_id == StreamId::PADDING_STREAM {
                    continue;
                }
                match payloads.iter_mut().find(|(id, _)| *id == stream_id) {
                    Some((_, payload)) => payload.extend_from_slice(packet.payload),
                    None => payloads.push((stream_id, packet.payload.to_vec())),
                }
            }
            payloads
        }

        #[test]
        fn at_a_fixed_rate_the_system_header_and_the_map_go_in_packs_of_their_own() {
            let stream = multiplexed_by(
                csps_muxer(vcd().with_header_interval(Duration::ZERO)),
                &video_and_audio()[..3],
            );
            // A packet takes 90 000 ticks, 588 bytes at 176 400 bytes a second; a
            // pack of a system header or a map, or of a PES packet shorter than 588
            // bytes, is filled with a packet of padding to 2 × 588 bytes, and the
            // rest of the video, 733 bytes, is not
            let shapes: Vec<_> = carried(&stream)
                .into_iter()
                .map(|pack| {
                    (
                        pack.length,
                        pack.system_header.is_some(),
                        pack.map.is_some(),
                        pack.stream_ids,
                    )
                })
                .collect();

            assert_eq!(
                shapes[..7],
                [
                    (1_176, true, false, vec![0xbe]),
                    (1_176, false, true, vec![0xbe]),
                    (VCD_PACK_SIZE, false, false, vec![VIDEO]),
                    (733, false, false, vec![VIDEO]),
                    (1_176, true, false, vec![0xbe]),
                    (1_176, false, true, vec![0xbe]),
                    (1_176, false, false, vec![AUDIO, 0xbe]),
                ]
            );
        }

        #[test]
        fn the_system_header_of_a_csps_has_csps_flag_set_and_packet_rate_restriction_flag_clear() {
            let inputs = [pes_packet(VIDEO, &[0xaa; 100], Some(90_000))];
            let csps = multiplexed_by(csps_muxer(MuxerConfig::new(VCD_RATE)), &inputs);
            let not_csps = multiplexed_by(muxer_of(MuxerConfig::new(VCD_RATE)), &inputs);
            let system_header =
                |stream| SystemHeader::read(carried(stream)[0].system_header.unwrap()).unwrap();

            assert_eq!(
                system_header(&csps),
                system_header(&not_csps)
                    .with_csps_flag(true)
                    .with_packet_rate_restriction_flag(false)
            );
        }

        #[test]
        fn at_10_mbit_a_second_a_csps_at_a_fixed_rate_takes_a_pack_size_that_holds_two_packets() {
            // 1 042 bytes a packet; a pack of a system header alone is filled with a
            // packet of padding to 2 084 bytes
            let new = |pack_size| {
                Muxer::new(
                    MuxerConfig::new(RATE)
                        .with_pack_size(pack_size)
                        .with_fixed_rate(true)
                        .with_csps(true),
                )
                .map(|_| ())
            };

            assert_eq!(
                [new(2_048), new(2_083), new(2_084)],
                [
                    Err(Error::PackSize { pack_size: 2_048 }),
                    Err(Error::PackSize { pack_size: 2_083 }),
                    Ok(())
                ]
            );
            assert!(
                Muxer::new(
                    MuxerConfig::new(RATE)
                        .with_pack_size(2_048)
                        .with_fixed_rate(true)
                )
                .is_ok()
            );
        }

        /// What a CSPS muxer of a Video CD rate says of the first PES packet of a
        /// stream of `stream_type` on `stream_id`, its bound `p_std_buffer`
        /// registered, `declared` its bound, or `p_std_buffer` the PES packet carries
        fn first_of(
            stream_id: u8,
            stream_type: StreamType,
            registered: PStdBuffer,
            declared: Option<DecoderBufferBound>,
            carried: Option<PStdBuffer>,
        ) -> Result<(), Error> {
            let mut muxer = Muxer::new(MuxerConfig::new(VCD_RATE).with_csps(true)).unwrap();
            let stream_id = StreamId::from(stream_id);
            muxer
                .add_stream(ElementaryStream::new(stream_type, stream_id), registered)
                .unwrap();
            if let Some(declared) = declared {
                muxer
                    .set_decoder_buffer_bound(stream_id, None, declared)
                    .unwrap();
            }
            let mut packet = pes_packet(stream_id.get(), &[0xaa; 100], Some(90_000));
            if let Some(carried) = carried {
                let mut extension = PesExtension::new();
                extension.p_std_buffer = Some(carried);
                packet.optional_header.as_mut().unwrap().pes_extension = Some(extension);
            }
            muxer.handle_pes_packet(&packet)
        }

        fn csps_buffer_size(
            stream_id: u8,
            p_std_buffer: PStdBuffer,
            smallest: u32,
            largest: u32,
        ) -> Result<(), Error> {
            Err(Error::CspsBufferSize {
                stream_id: StreamId::from(stream_id),
                stream_id_extension: None,
                p_std_buffer,
                smallest,
                largest,
            })
        }

        #[test]
        fn the_p_std_buffer_of_an_audio_stream_is_at_most_4096_bytes_and_8976_for_adts() {
            let audio = |stream_id, stream_type, units| {
                first_of(
                    stream_id,
                    stream_type,
                    PStdBuffer::new(false, units),
                    None,
                    None,
                )
            };

            // In units of 128 bytes: 32 is 4 096, 70 is 8 960 and 71 is 9 088
            assert_eq!(
                [
                    audio(0xc0, StreamType::ISO_IEC_11172_3_AUDIO, 32),
                    audio(0xc0, StreamType::ISO_IEC_11172_3_AUDIO, 33),
                    audio(0xbd, StreamType::ISO_IEC_14496_3_AUDIO_LATM, 33),
                    audio(0xc1, StreamType::ISO_IEC_13818_7_AUDIO, 70),
                    audio(0xc1, StreamType::ISO_IEC_13818_7_AUDIO, 71),
                ],
                [
                    Ok(()),
                    csps_buffer_size(0xc0, PStdBuffer::new(false, 33), 0, 4_096),
                    csps_buffer_size(0xbd, PStdBuffer::new(false, 33), 0, 4_096),
                    Ok(()),
                    csps_buffer_size(0xc1, PStdBuffer::new(false, 71), 0, 8_976),
                ]
            );
        }

        #[test]
        fn the_p_std_buffer_of_a_video_stream_is_its_vbv_and_at_most_bs_add_more() {
            let vbv = DecoderBufferBound::Vbv {
                size: 40 * 1024,
                rv_max: 1_150_000,
            };
            let video = |units, carried| {
                first_of(
                    VIDEO,
                    StreamType::ISO_IEC_11172_2_VIDEO,
                    PStdBuffer::new(true, units),
                    Some(vbv),
                    carried,
                )
            };

            // BSadd at most 6 KiB: 40 to 46 KiB
            assert_eq!(
                [
                    video(46, None),
                    video(47, None),
                    video(39, None),
                    video(224, Some(PStdBuffer::new(true, 39))),
                    video(224, Some(PStdBuffer::new(true, 40))),
                ],
                [
                    Ok(()),
                    csps_buffer_size(VIDEO, PStdBuffer::new(true, 47), 40_960, 47_104),
                    csps_buffer_size(VIDEO, PStdBuffer::new(true, 39), 40_960, 47_104),
                    csps_buffer_size(VIDEO, PStdBuffer::new(true, 39), 40_960, 47_104),
                    Ok(()),
                ]
            );
        }

        #[test]
        fn outside_a_csps_the_p_std_buffer_is_not_bounded_by_its_stream() {
            let mut muxer = Muxer::new(MuxerConfig::new(VCD_RATE)).unwrap();
            muxer
                .add_stream(
                    ElementaryStream::new(StreamType::ISO_IEC_11172_3_AUDIO, StreamId::from(AUDIO)),
                    PStdBuffer::new(false, 33),
                )
                .unwrap();

            assert_eq!(
                muxer.handle_pes_packet(&pes_packet(AUDIO, &[0xbb; 100], Some(90_000))),
                Ok(())
            );
        }

        #[test]
        #[should_panic(expected = "before set_decoder_buffer_bound")]
        fn a_p_std_buffer_of_a_stream_whose_bound_is_not_declared_is_the_callers_mistake_in_a_csps()
        {
            let _ = first_of(
                0xbd,
                StreamType::from(0x81),
                PStdBuffer::new(false, 32),
                None,
                None,
            );
        }

        #[test]
        fn a_bound_declared_is_taken_where_the_stream_may_have_it() {
            let declare = |stream_type: StreamType, bound| {
                let mut muxer = Muxer::new(MuxerConfig::new(VCD_RATE).with_csps(true)).unwrap();
                muxer
                    .add_stream(
                        ElementaryStream::new(stream_type, StreamId::from(VIDEO)),
                        VIDEO_BUFFER,
                    )
                    .unwrap();
                muxer.set_decoder_buffer_bound(StreamId::from(VIDEO), None, bound)
            };
            let refused = |bound| {
                Err(Error::DecoderBufferBound {
                    stream_id: StreamId::from(VIDEO),
                    stream_id_extension: None,
                    decoder_buffer_bound: bound,
                })
            };

            assert_eq!(
                [
                    declare(StreamType::ISO_IEC_13818_2_VIDEO, VBV),
                    declare(StreamType::ISO_IEC_13818_2_VIDEO, DecoderBufferBound::Audio),
                    declare(
                        StreamType::MVC_VIDEO_SUB_BITSTREAM,
                        DecoderBufferBound::Unbounded
                    ),
                    declare(StreamType::MVC_VIDEO_SUB_BITSTREAM, VBV),
                ],
                [
                    Ok(()),
                    refused(DecoderBufferBound::Audio),
                    Ok(()),
                    refused(VBV)
                ]
            );
        }

        #[test]
        fn a_stream_type_the_bound_declared_for_the_stream_does_not_fit_is_refused() {
            let mut declared = csps_muxer(MuxerConfig::new(VCD_RATE));
            let mut undeclared = muxer_of(MuxerConfig::new(VCD_RATE).with_csps(true));
            let avc = ElementaryStream::new(StreamType::AVC_VIDEO, StreamId::from(VIDEO));

            assert_eq!(
                (declared.update_stream(avc), undeclared.update_stream(avc)),
                (
                    Err(Error::DecoderBufferBound {
                        stream_id: StreamId::from(VIDEO),
                        stream_id_extension: None,
                        decoder_buffer_bound: VBV,
                    }),
                    Ok(())
                )
            );
        }

        #[test]
        fn at_a_fixed_rate_a_pack_header_alone_goes_before_the_end_code_where_it_would_come_sooner_than_the_packet_rate()
         {
            // The last pack is filled to the 588 bytes a packet takes to the next
            // SCR, and the end code ends 5 bytes before the byte that SCR is in
            let ends: Vec<(bool, bool)> = (540..=600)
                .map(|length| {
                    let stream = multiplexed_by(
                        csps_muxer(vcd()),
                        &[
                            pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
                            pes_packet(AUDIO, &vec![0xbb; length], Some(91_000)),
                        ],
                    );
                    let last = carried(&stream).pop().unwrap();
                    (
                        meets_the_packet_rate(&stream, VCD_RATE)
                            .iter()
                            .all(|&met| met),
                        last.stream_ids.is_empty(),
                    )
                })
                .collect();

            assert!(ends.iter().all(|&(met, _)| met), "{ends:?}");
            assert!(ends.iter().any(|&(_, closing)| closing), "{ends:?}");
            assert!(ends.iter().any(|&(_, closing)| !closing), "{ends:?}");
        }
    }
    mod fixed_rate {
        //! A fixed bitrate: the SCRs on the line of equations 2-22 and 2-23, and the
        //! padding that fills the time before a PES packet

        use super::*;

        /// Ticks of the STC that 7 bytes take to arrive at [`RATE`], 1 260 000 bytes
        /// a second
        const TICKS_OF_SEVEN_BYTES: u64 = 150;

        /// Ticks of the STC in the longest time from one SCR to the next, 0.7 s
        /// (§2.7.1)
        const LONGEST_SCR_INTERVAL: u64 = 18_900_000;

        /// The packs of padding that fill `bytes` in packs of at most `longest`, as
        /// their bytes and the `stream_id` of their PES packets: `longest` each but
        /// the last, which takes the rest, the one before it giving up what the
        /// last needs to make a pack header; a pack header carries the bytes past
        /// it as stuffing up to 7, and past them as many PES packets of padding as
        /// the longest PES packet, 65 541 bytes, needs
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "the lengths are those of a few packs, written as the rule reads"
        )]
        fn padding_packs(bytes: usize, longest: usize) -> Vec<(usize, Vec<u8>)> {
            let count = bytes.div_ceil(longest);
            let rest = bytes - (count - 1) * longest;
            let mut lengths = vec![longest; count - 1];
            lengths.push(rest);
            if rest < 14 {
                lengths[count - 2] -= 14 - rest;
                lengths[count - 1] = 14;
            }
            lengths
                .into_iter()
                .map(|length| {
                    let padding = length - 14;
                    let stream_ids = if padding > 7 {
                        vec![0xbe; padding.div_ceil(65_541)]
                    } else {
                        vec![]
                    };
                    (length, stream_ids)
                })
                .collect()
        }

        /// The packs of `stream`, as their bytes and the `stream_id` of their PES
        /// packets
        fn pack_shapes(stream: &[u8]) -> Vec<(usize, Vec<u8>)> {
            carried(stream)
                .into_iter()
                .map(|pack| (pack.length, pack.stream_ids))
                .collect()
        }

        fn fixed_rate() -> MuxerConfig {
            MuxerConfig::new(RATE).with_fixed_rate(true)
        }

        /// PES packets of video and audio over 3 s, one split over packs, with a
        /// leap of 1.5 s
        fn inputs() -> Vec<PesPacket<'static>> {
            vec![
                pes_packet(VIDEO, &[0xaa; 3_000], Some(90_000)),
                pes_packet(AUDIO, &[0xbb; 300], Some(91_000)),
                pes_packet(VIDEO, &[0xaa; 100], Some(93_600)),
                pes_packet(AUDIO, &[0xbb; 300], Some(95_000)),
                pes_packet(VIDEO, &[0xaa; 5_000], Some(97_200)),
                pes_packet(VIDEO, &[0xaa; 100], Some(232_200)),
                pes_packet(AUDIO, &[0xbb; 300], Some(232_300)),
                pes_packet(VIDEO, &[0xaa; 100], Some(270_000)),
            ]
        }

        #[test]
        fn at_a_fixed_rate_every_scr_is_on_the_line_of_equations_2_22_and_2_23() {
            let stream = multiplexed_by(muxer_of(fixed_rate()), &inputs());

            // SCR(i) = c1 × i + c2 with c1 = 150 / 7 ticks a byte, and c2 = SCR(i₀)
            // − c1 × i₀ through the first SCR; ⌊c1 × i + c2⌋ = SCR(i₀) + ⌊c1 × (i −
            // i₀)⌋
            let scrs: Vec<_> = carried(&stream)
                .into_iter()
                .map(|pack| (pack.scr_byte, pack.scr))
                .collect();
            let (first_at, first) = scrs[0];
            let on_line: Vec<_> = scrs
                .iter()
                .map(|&(at, _)| (at, first + (at - first_at) * TICKS_OF_SEVEN_BYTES / 7))
                .collect();
            assert_eq!(scrs, on_line);
            assert!(scrs.len() > 100, "{} packs", scrs.len());
            let longest_step = scrs
                .windows(2)
                .map(|pair| pair[1].1 - pair[0].1)
                .max()
                .unwrap();
            assert!(longest_step <= LONGEST_SCR_INTERVAL, "{longest_step}");
        }

        #[test]
        fn at_a_fixed_rate_the_system_header_has_fixed_flag_set() {
            let stream = multiplexed_by(muxer_of(fixed_rate()), &inputs()[..1]);
            let fixed = SystemHeader::read(carried(&stream)[0].system_header.unwrap()).unwrap();
            let variable = multiplexed_by(muxer(), &inputs()[..1]);
            let variable =
                SystemHeader::read(carried(&variable)[0].system_header.unwrap()).unwrap();

            assert_eq!(fixed, variable.with_fixed_flag(true));
        }

        /// What the muxer of `config` writes of two PES packets of video 1.5 s
        /// apart, and how many bytes the packs of padding between them take
        #[expect(
            clippy::arithmetic_side_effects,
            reason = "a stream has at least the packs of its two PES packets"
        )]
        fn padded_across_a_second_and_a_half(config: MuxerConfig) -> (Vec<u8>, usize) {
            let packets = [
                pes_packet(VIDEO, &[0xaa; 100], Some(90_000)),
                pes_packet(VIDEO, &[0xaa; 100], Some(225_000)),
            ];
            let stream = multiplexed_by(muxer_of(config), &packets);
            let packs = carried(&stream);
            let padding = packs[1..packs.len() - 1]
                .iter()
                .map(|pack| pack.length)
                .sum();
            (stream, padding)
        }

        #[test]
        fn the_time_before_a_pes_packet_is_filled_with_padding_up_to_its_time() {
            let (stream, padding) = padded_across_a_second_and_a_half(fixed_rate());

            let packs = carried(&stream);
            let mut expected = vec![(packs[0].length, vec![VIDEO])];
            expected.extend(padding_packs(padding, 2_048));
            expected.push((packs.last().unwrap().length, vec![VIDEO]));
            assert_eq!(pack_shapes(&stream), expected);
            let padding_bytes: Vec<u8> = pes_packets(&stream)
                .into_iter()
                .filter(|packet| packet.stream_id == StreamId::PADDING_STREAM)
                .flat_map(|packet| packet.payload.to_vec())
                .collect();
            assert_eq!(padding_bytes, vec![0xff; padding_bytes.len()]);
            // The SCR of the PES packet is the first on the line at or after its
            // time, 1.5 s after the first, less mux_delay
            let (first, last) = (&packs[0], packs.last().unwrap());
            let on_line =
                |byte: u64| first.scr + (byte - first.scr_byte) * TICKS_OF_SEVEN_BYTES / 7;
            let due = first.scr + 135_000 * 300;
            assert_eq!(
                (on_line(last.scr_byte - 1) < due, last.scr >= due),
                (true, true)
            );
        }

        #[test]
        fn a_pack_of_padding_past_the_longest_pes_packet_carries_more_than_one_padding_packet() {
            let (stream, padding) =
                padded_across_a_second_and_a_half(fixed_rate().with_pack_size(100_000));

            // Packs of padding up to pack_size, past the 14 + 65 541 bytes of a pack
            // header and the longest PES packet
            let packs = carried(&stream);
            let mut expected = vec![(packs[0].length, vec![VIDEO])];
            expected.extend(padding_packs(padding, 100_000));
            expected.push((packs.last().unwrap().length, vec![VIDEO]));
            assert_eq!(pack_shapes(&stream), expected);
        }

        #[test]
        fn at_a_fixed_rate_the_payloads_join_back_into_the_access_units() {
            let inputs = inputs();
            let stream = multiplexed_by(muxer_of(fixed_rate()), &inputs);

            let joined = |stream_id: u8, packets: Vec<PesPacket<'_>>| -> Vec<u8> {
                packets
                    .into_iter()
                    .filter(|packet| packet.stream_id.get() == stream_id)
                    .flat_map(|packet| packet.payload.to_vec())
                    .collect()
            };
            assert_eq!(
                (
                    joined(VIDEO, pes_packets(&stream)),
                    joined(AUDIO, pes_packets(&stream))
                ),
                (joined(VIDEO, inputs.clone()), joined(AUDIO, inputs))
            );
        }

        #[test]
        fn padding_past_the_buffer_limit_is_refused_as_a_leap() {
            let mut muxer = muxer_of(fixed_rate().with_buffer_limit(10_000));
            muxer
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(90_000)))
                .unwrap();

            assert_eq!(
                muxer.handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(180_000))),
                Err(Error::TimestampGap { gap: 90_000 })
            );
        }

        #[test]
        fn a_pes_packet_is_at_its_time_or_up_to_13_bytes_earlier_but_never_later() {
            // Of the second of two PES packets of video, `step` 90 kHz ticks after
            // the first: whether padding filled the time to it exactly, or fewer
            // bytes than a pack header were missing and none did
            let shape = |step: u64| {
                let packets = [
                    pes_packet(VIDEO, &[0xaa; 100], Some(90_000)),
                    pes_packet(VIDEO, &[0xaa; 100], Some(90_000 + step)),
                ];
                let stream = multiplexed_by(muxer_of(fixed_rate()), &packets);
                let packs = carried(&stream);
                let (first, last) = (&packs[0], packs.last().unwrap());
                let on_line =
                    |byte: u64| first.scr + (byte - first.scr_byte) * TICKS_OF_SEVEN_BYTES / 7;
                let due = first.scr + step * 300;
                let next = on_line(first.scr_byte + u64::try_from(first.length).unwrap());
                if due <= next {
                    "on the line after the first"
                } else if packs.len() > 2 && last.scr >= due && on_line(last.scr_byte - 1) < due {
                    "padded to its time"
                } else if packs.len() == 2
                    && last.scr < due
                    && due - last.scr <= 13 * TICKS_OF_SEVEN_BYTES / 7
                {
                    "up to 13 bytes early"
                } else {
                    "wrong"
                }
            };

            let shapes: Vec<_> = (0..200).map(shape).collect();
            assert!(!shapes.contains(&"wrong"), "{shapes:?}");
            assert!(
                shapes.contains(&"padded to its time") && shapes.contains(&"up to 13 bytes early")
            );
        }
    }
    mod headers {
        //! The system header and the program stream map sent again, and the map
        //! updated

        use super::*;

        /// 0.5 s in 90 kHz ticks
        const HALF_SECOND: u64 = 45_000;

        /// Whether each pack of `bytes` carries the system header and the map
        fn headers_by_pack(bytes: &[u8]) -> Vec<(bool, bool)> {
            carried(bytes)
                .iter()
                .map(|pack| (pack.system_header.is_some(), pack.map.is_some()))
                .collect()
        }

        /// A PES packet of video every 0.5 s, from 0 to 3 s
        fn every_half_second() -> Vec<PesPacket<'static>> {
            (0..7)
                .map(|index| {
                    pes_packet(VIDEO, &[0xaa; 100], Some(HALF_SECOND.saturating_mul(index)))
                })
                .collect()
        }

        /// The bytes of a program stream map of `program_stream_info` and the
        /// video and audio streams of `video_type` and `audio_type`, with
        /// `video_info` for the video stream
        fn map_bytes(
            version: u8,
            program_stream_info: &[u8],
            (video_type, video_info): (StreamType, &[u8]),
            audio_type: StreamType,
        ) -> Vec<u8> {
            let mut streams = Vec::new();
            ElementaryStreamLoop::write_from(
                [
                    ElementaryStream::new(video_type, StreamId::from(VIDEO))
                        .with_elementary_stream_info(DescriptorLoop::from(video_info)),
                    ElementaryStream::new(audio_type, StreamId::from(AUDIO)),
                ],
                &mut streams,
            )
            .unwrap();
            let mut map = ProgramStreamMap::new(VersionNumber(version));
            map.program_stream_info = DescriptorLoop::from(program_stream_info);
            map.elementary_streams = ElementaryStreamLoop::from(streams.as_slice());
            let mut bytes = Vec::new();
            map.write(&mut bytes).unwrap();
            bytes
        }

        /// The video stream as an AVC video stream with `es_info`
        fn avc_video(es_info: &[u8]) -> ElementaryStream<'_> {
            ElementaryStream::new(StreamType::AVC_VIDEO, StreamId::from(VIDEO))
                .with_elementary_stream_info(DescriptorLoop::from(es_info))
        }

        /// The map version of each pack of `bytes` that carries a map
        fn map_versions(bytes: &[u8]) -> Vec<u8> {
            carried(bytes)
                .into_iter()
                .filter_map(|pack| pack.map.map(|map| map.program_stream_map_version.get()))
                .collect()
        }

        #[test]
        fn by_default_the_system_header_and_the_map_are_in_the_first_pack_alone() {
            let stream = multiplexed_by(muxer(), &every_half_second());

            let mut expected = vec![(false, false); 7];
            expected[0] = (true, true);
            assert_eq!(headers_by_pack(&stream), expected);
        }

        #[test]
        fn the_headers_go_again_in_the_first_pack_header_interval_or_more_after_the_last() {
            let at_interval = |interval: Duration| {
                multiplexed_by(
                    muxer_of(MuxerConfig::new(RATE).with_header_interval(interval)),
                    &every_half_second(),
                )
            };
            let second = at_interval(Duration::from_secs(1));
            // 27 000 001 ticks of the STC, a tick past 1 s
            let past_a_second = at_interval(Duration::from_nanos(1_000_000_038));

            let with = (true, true);
            let without = (false, false);
            assert_eq!(
                (headers_by_pack(&second), headers_by_pack(&past_a_second)),
                (
                    vec![with, without, with, without, with, without, with],
                    vec![with, without, without, with, without, without, with]
                )
            );
            let packs = carried(&second);
            let first = &packs[0];
            assert!(
                packs
                    .iter()
                    .filter(|pack| pack.system_header.is_some())
                    .all(|pack| (&pack.system_header, &pack.map)
                        == (&first.system_header, &first.map))
            );
        }

        #[test]
        fn at_a_header_interval_of_0_each_pes_packet_carries_the_headers_and_no_pack_after_it() {
            let packets = [
                pes_packet(VIDEO, &[0xaa; 3_000], Some(0)),
                pes_packet(VIDEO, &[0xaa; 100], Some(3 * HALF_SECOND)),
                pes_packet(AUDIO, &[0xbb; 100], Some(3 * HALF_SECOND)),
            ];
            let stream = multiplexed_by(
                muxer_of(MuxerConfig::new(RATE).with_header_interval(Duration::ZERO)),
                &packets,
            );

            let packs: Vec<_> = carried(&stream)
                .iter()
                .map(|pack| (pack.system_header.is_some(), pack.stream_ids.len()))
                .collect();
            // The split PES packet, two packs of a pack header alone over the leap,
            // and the two PES packets at 1.5 s
            assert_eq!(
                packs,
                [
                    (true, 1),
                    (false, 1),
                    (false, 0),
                    (false, 0),
                    (true, 1),
                    (true, 1)
                ]
            );
        }

        #[test]
        fn an_updated_stream_goes_in_the_map_of_the_next_pes_packet_its_version_incremented() {
            let mut muxer = muxer();
            let mut stream = Vec::new();
            muxer
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 100], Some(0)))
                .unwrap();
            let info = descriptors(6);
            muxer.update_stream(avc_video(&info)).unwrap();
            muxer
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 100], Some(3_600)))
                .unwrap();
            muxer
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa; 100], Some(7_200)))
                .unwrap();
            muxer.finish();
            muxer.poll_output(&mut stream);

            let packs = carried(&stream);
            let updated = map_bytes(
                1,
                &[],
                (StreamType::AVC_VIDEO, &info),
                StreamType::ISO_IEC_11172_3_AUDIO,
            );
            assert_eq!(
                packs.iter().map(|pack| &pack.map).collect::<Vec<_>>(),
                [
                    &Some(
                        ProgramStreamMap::read(&map_bytes(
                            0,
                            &[],
                            (StreamType::ISO_IEC_13818_2_VIDEO, &[]),
                            StreamType::ISO_IEC_11172_3_AUDIO
                        ))
                        .unwrap()
                    ),
                    &Some(ProgramStreamMap::read(&updated).unwrap()),
                    &None
                ]
            );
            assert_eq!(packs[1].system_header, packs[0].system_header);
        }

        #[test]
        fn updates_before_the_next_pes_packet_are_one_change_of_version_and_none_before_the_first()
        {
            let info = descriptors(6);
            let mut before_first = muxer();
            before_first.update_stream(avc_video(&info)).unwrap();
            let mut twice = muxer();
            twice
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
                .unwrap();
            twice.poll_output(&mut Vec::new());
            twice.update_stream(avc_video(&[])).unwrap();
            twice.update_stream(avc_video(&info)).unwrap();
            let packets = [
                pes_packet(VIDEO, &[0xaa], Some(0)),
                pes_packet(VIDEO, &[0xaa], Some(3_600)),
            ];

            let first = multiplexed_by(before_first, &packets);
            let mut stream = Vec::new();
            twice.handle_pes_packet(&packets[1]).unwrap();
            twice.poll_output(&mut stream);
            assert_eq!(
                (carried(&first)[0].map, map_versions(&stream)),
                (
                    Some(
                        ProgramStreamMap::read(&map_bytes(
                            0,
                            &[],
                            (StreamType::AVC_VIDEO, &info),
                            StreamType::ISO_IEC_11172_3_AUDIO
                        ))
                        .unwrap()
                    ),
                    vec![1]
                )
            );
        }

        #[test]
        fn an_update_or_program_stream_info_that_changes_nothing_writes_no_map() {
            let mut muxer = muxer();
            muxer
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
                .unwrap();
            muxer.poll_output(&mut Vec::new());
            muxer
                .update_stream(ElementaryStream::new(
                    StreamType::ISO_IEC_13818_2_VIDEO,
                    StreamId::from(VIDEO),
                ))
                .unwrap();
            muxer
                .set_program_stream_info(DescriptorLoop::from([].as_slice()))
                .unwrap();
            let mut stream = Vec::new();
            muxer
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(3_600)))
                .unwrap();
            muxer.poll_output(&mut stream);

            assert_eq!(headers_by_pack(&stream), [(false, false)]);
        }

        #[test]
        fn the_version_of_the_map_wraps_from_31_to_0() {
            let mut muxer = muxer();
            let mut stream = Vec::new();
            for index in 0..33 {
                if index > 0 {
                    let stream_type = if index % 2 == 1 {
                        StreamType::AVC_VIDEO
                    } else {
                        StreamType::ISO_IEC_13818_2_VIDEO
                    };
                    muxer
                        .update_stream(ElementaryStream::new(stream_type, StreamId::from(VIDEO)))
                        .unwrap();
                }
                muxer
                    .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(index * 3_600)))
                    .unwrap();
            }
            muxer.poll_output(&mut stream);

            let mut expected: Vec<u8> = (0..32).collect();
            expected.push(0);
            assert_eq!(map_versions(&stream), expected);
        }

        #[test]
        fn program_stream_info_goes_in_the_map_its_version_incremented_after_the_first_pes_packet()
        {
            let info = descriptors(8);
            let mut before_first = muxer();
            before_first
                .set_program_stream_info(DescriptorLoop::from(info.as_slice()))
                .unwrap();
            let mut after_first = muxer();
            let mut stream = Vec::new();
            after_first
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
                .unwrap();
            after_first
                .set_program_stream_info(DescriptorLoop::from(info.as_slice()))
                .unwrap();
            after_first
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(3_600)))
                .unwrap();
            after_first.poll_output(&mut stream);

            let video = (StreamType::ISO_IEC_13818_2_VIDEO, [].as_slice());
            let audio = StreamType::ISO_IEC_11172_3_AUDIO;
            let first = multiplexed_by(before_first, &[pes_packet(VIDEO, &[0xaa], Some(0))]);
            let maps: Vec<_> = carried(&stream)
                .into_iter()
                .filter_map(|pack| pack.map)
                .collect();
            assert_eq!(
                (carried(&first)[0].map, maps),
                (
                    Some(ProgramStreamMap::read(&map_bytes(0, &info, video, audio)).unwrap()),
                    vec![
                        ProgramStreamMap::read(&map_bytes(0, &[], video, audio)).unwrap(),
                        ProgramStreamMap::read(&map_bytes(1, &info, video, audio)).unwrap(),
                    ]
                )
            );
        }

        #[test]
        fn an_update_of_a_stream_not_registered_is_refused() {
            let mut muxer = muxer();
            assert_eq!(
                [
                    muxer.update_stream(ElementaryStream::new(
                        StreamType::ISO_IEC_13818_2_VIDEO,
                        StreamId::from(0xe1)
                    )),
                    muxer.update_stream(ElementaryStream::new(
                        StreamType::ISO_IEC_13818_2_VIDEO,
                        StreamId::EXTENDED_STREAM_ID
                    )),
                ],
                [
                    Err(Error::UnregisteredStream {
                        stream_id: StreamId::from(0xe1),
                        stream_id_extension: None
                    }),
                    Err(Error::StreamIdExtension {
                        stream_id: StreamId::EXTENDED_STREAM_ID
                    }),
                ]
            );
        }

        #[test]
        fn an_update_into_or_out_of_audio_or_video_is_refused_after_the_first_pes_packet() {
            let update = |muxer: &mut Muxer, stream_id: u8, stream_type: StreamType| {
                muxer.update_stream(ElementaryStream::new(
                    stream_type,
                    StreamId::from(stream_id),
                ))
            };
            let mut before_first = muxer();
            let mut after_first = muxer();
            after_first
                .handle_pes_packet(&pes_packet(VIDEO, &[0xaa], Some(0)))
                .unwrap();

            let private = StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA;
            assert_eq!(
                [
                    update(&mut before_first, VIDEO, private),
                    update(&mut after_first, VIDEO, private),
                    update(&mut after_first, AUDIO, StreamType::ISO_IEC_13818_7_AUDIO),
                    update(&mut after_first, AUDIO, StreamType::ISO_IEC_13818_3_AUDIO),
                ],
                [
                    Ok(()),
                    Err(Error::SystemHeaderChange {
                        stream_id: StreamId::from(VIDEO),
                        stream_id_extension: None
                    }),
                    Err(Error::SystemHeaderChange {
                        stream_id: StreamId::from(AUDIO),
                        stream_id_extension: None
                    }),
                    Ok(()),
                ]
            );
        }

        #[test]
        fn a_map_past_1018_bytes_or_its_pack_past_pack_size_is_refused_and_leaves_the_muxer_as_it_was()
         {
            // A map of video and audio: 10 bytes besides its loop and two entries of
            // 4 leave 1 000 bytes of descriptors
            let too_long = Err(Error::ProgramStreamMap {
                error: psm::Error::ProgramStreamMapLength {
                    program_stream_map_length: 1_019,
                    limit: 1_018,
                },
            });
            let past_1018 = descriptors(1_001);
            let mut muxer = muxer();
            let packets = [
                pes_packet(VIDEO, &[0xaa], Some(0)),
                pes_packet(VIDEO, &[0xaa], Some(3_600)),
            ];
            muxer.handle_pes_packet(&packets[0]).unwrap();
            let refused = [
                muxer.update_stream(avc_video(&past_1018)),
                muxer.set_program_stream_info(DescriptorLoop::from(past_1018.as_slice())),
            ];
            // 14 of pack header, 18 of system header, 24 of map, 6 of PES packet
            // header and a byte of payload
            let mut tight = muxer_of(MuxerConfig::new(RATE).with_pack_size(63));
            let info = descriptors(2);
            let past_pack_size = [
                tight.update_stream(avc_video(&info)),
                tight.set_program_stream_info(DescriptorLoop::from(info.as_slice())),
            ];
            let mut stream = Vec::new();
            muxer.poll_output(&mut stream);
            muxer.handle_pes_packet(&packets[1]).unwrap();
            muxer.finish();
            muxer.poll_output(&mut stream);

            let past = Err(Error::PackLength { length: 65 });
            assert_eq!(
                (refused, past_pack_size, stream),
                (
                    [too_long, too_long],
                    [past, past],
                    multiplexed_by(self::muxer(), &packets)
                )
            );
        }

        #[test]
        fn a_pes_packet_whose_pack_cannot_also_carry_the_headers_sent_again_is_refused() {
            // The first pack: 14 + 18 + 24 + 6 + 3 + 5 of PTS + 3 of P-STD buffer
            // and a byte of payload. One with a PTS and a DTS takes 14 + 6 + 3 + 10
            // and a byte alone, 18 + 24 more with the headers
            let config = MuxerConfig::new(RATE).with_pack_size(74);
            let mut with_dts = pes_packet(VIDEO, &[0xaa], Some(3_600));
            with_dts.optional_header.as_mut().unwrap().dts = Some(timestamp(3_000));
            let first = pes_packet(VIDEO, &[0xaa], Some(0));
            let give = |config: MuxerConfig| {
                let mut muxer = muxer_of(config);
                muxer.handle_pes_packet(&first).unwrap();
                muxer.handle_pes_packet(&with_dts)
            };

            assert_eq!(
                [
                    give(config),
                    give(config.with_header_interval(Duration::ZERO))
                ],
                [Ok(()), Err(Error::PackLength { length: 76 })]
            );
        }
    }
}
