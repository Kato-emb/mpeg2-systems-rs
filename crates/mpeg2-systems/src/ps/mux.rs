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
use core::time::Duration;

use crate::field::write_or_truncate;
use crate::pes::extension::PesExtension;
use crate::pes::{self, PesPacket, StreamId};
use crate::ps::{PackHeader, SystemHeader, pack_header};
use crate::psi::VersionNumber;
use crate::time::SystemTimeClock;
use csps::Constraints;
use pack_writer::PesPacketPieces;
use schedule::{Gap, Pack, Rate};
use stream_definition::Stream;

mod config;
mod csps;
mod error;
mod pack_writer;
mod schedule;
mod stream_definition;

pub use config::MuxerConfig;
pub use csps::DecoderBufferBound;
pub use error::Error;
pub use pack_writer::PackWriter;

/// Number of values the 22 bits of `program_mux_rate` hold
const PROGRAM_MUX_RATE_LIMIT: u32 = 1 << 22;

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
    streams: Vec<Stream>,
    /// `program_stream_info` of the program stream map
    program_stream_info: Vec<u8>,
    /// `program_stream_map_version` of the program stream map as it stands
    // Why not count the changes: those made before the next PES packet are
    // one change of the map a receiver sees.
    map_version: VersionNumber,
    /// The time of the STC the SCR of the last pack that carried the system
    /// header and the program stream map as it stands samples, before
    /// [`MuxerConfig::mux_delay`] is taken off, `None` when no pack has
    // Why not a flag for a change of the map besides it: a change makes the
    // map due in the next pack, as no sending at all does.
    headers_sent_at: Option<SystemTimeClock>,
    /// The system header and the program stream map the PES packet being
    /// taken carries
    // Why not a Vec per PES packet: at a short `header_interval` every PES
    // packet carries them, and that path allocates nothing.
    headers: Vec<u8>,
    /// The stream bound loop and the elementary stream loop of `headers`, as
    /// they are written
    // Why not a Vec per PES packet: as for `headers`.
    header_loops: Vec<u8>,
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

/// Returns the bytes of the shortest pack: a pack header, the shortest PES
/// packet header and a byte of payload
fn shortest_pack() -> usize {
    pack_header::FIXED_LENGTH
        .saturating_add(pes::PREFIX_LENGTH)
        .saturating_add(1)
}

/// Returns the pack header whose SCR samples `at` less `mux_delay`, at
/// `program_mux_rate`
fn pack_header_at(at: SystemTimeClock, mux_delay: Duration, program_mux_rate: u32) -> PackHeader {
    PackHeader::new(at.to_clock_reference(mux_delay), program_mux_rate)
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
        if program_mux_rate == 0 || program_mux_rate >= PROGRAM_MUX_RATE_LIMIT {
            return Err(Error::ProgramMuxRate { program_mux_rate });
        }
        let pack_size = config.pack_size;
        // Why not refuse a program_mux_rate the shortest pack does not arrive
        // at within 0.7 s: it is shorter than what arrives at a rate of 1.
        if pack_size < shortest_pack() {
            return Err(Error::PackSize { pack_size });
        }
        let muxer = Self {
            config,
            streams: Vec::new(),
            program_stream_info: Vec::new(),
            map_version: VersionNumber(0),
            headers_sent_at: None,
            headers: Vec::new(),
            header_loops: Vec::new(),
            last_pack: None,
            pending: Vec::new(),
            end: End::Open,
        };
        if muxer.rate().longest_first_pack(0, 0, shortest_pack()) > muxer.largest_pack() {
            return Err(Error::PackSize { pack_size });
        }
        Ok(muxer)
    }

    /// Returns the system header of the settings, its bounds and its
    /// streams none: `rate_bound` [`MuxerConfig::program_mux_rate`] and its
    /// flags
    fn system_header(&self) -> SystemHeader<'static> {
        let constraints = self.constraints();
        SystemHeader::new(self.config.program_mux_rate, 0, 0)
            .with_fixed_flag(self.rate().fixed_flag())
            .with_csps_flag(constraints.csps_flag())
            .with_packet_rate_restriction_flag(constraints.packet_rate_restriction_flag())
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

    /// Returns the stream registered of `stream_id` and `stream_id_extension`,
    /// and its index
    fn stream(
        &self,
        stream_id: StreamId,
        stream_id_extension: Option<u8>,
    ) -> Option<(usize, &Stream)> {
        self.streams.iter().enumerate().find(|(_, stream)| {
            stream.stream_id == stream_id && stream.stream_id_extension == stream_id_extension
        })
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
            // Why not the whole first byte of PES_extension_field: its first
            // bit is stream_id_extension_flag, whose '1' is reserved and
            // carries no stream_id_extension (§2.4.3.7).
            let field = header
                .and_then(|header| header.pes_extension.as_ref())
                .and_then(|extension| extension.pes_extension_field)
                .and_then(|field| field.first());
            Some(
                field
                    .filter(|&&first| first & 0x80 == 0)
                    .map(|first| first & 0x7f)
                    .ok_or(Error::StreamIdExtension { stream_id })?,
            )
        } else {
            None
        };
        let Some((index, stream)) = self.stream(stream_id, stream_id_extension) else {
            return Err(Error::UnregisteredStream {
                stream_id,
                stream_id_extension,
            });
        };
        let missing_pts = Error::MissingPts {
            stream_id,
            stream_id_extension,
        };
        let first_of_stream = stream.last_timestamp.is_none();
        // Why not ask a PTS of the first PES packet of every stream: the PTS
        // of the first access unit §2.7.5 asks for sits in the optional
        // header, which the PES packets of some streams do not carry.
        if first_of_stream
            && stream_id.carries_optional_header()
            && header.and_then(|header| header.pts).is_none()
        {
            return Err(missing_pts);
        }
        let timestamp = header.and_then(|header| header.dts.or(header.pts));
        // Why not a field of the time reached: it is the latest
        // `last_timestamp` of the streams, which every time stamp taken sets.
        let clock = self
            .streams
            .iter()
            .filter_map(|stream| stream.last_timestamp)
            .max();
        let time = match (clock, timestamp) {
            // Why not a time of its own: the SCR of the first pack is the
            // time of its PES packet, and this one has none.
            (None, None) => return Err(missing_pts),
            (None, Some(timestamp)) => Some(SystemTimeClock::first(timestamp)),
            (Some(clock), timestamp) => timestamp.map(|timestamp| clock.expand(timestamp)),
        };
        if let (Some(time), Some(previous)) = (time, stream.last_timestamp)
            && time < previous
        {
            return Err(Error::DecreasingTimestamp {
                stream_id,
                stream_id_extension,
                timestamp: time.to_timestamp(),
                previous: previous.to_timestamp(),
            });
        }
        let given_p_std_buffer = header
            .and_then(|header| header.pes_extension.as_ref())
            .and_then(|extension| extension.p_std_buffer);
        if let Some(p_std_buffer) = given_p_std_buffer
            && p_std_buffer.size_in_bytes() > stream.p_std_buffer.size_in_bytes()
        {
            return Err(Error::PStdBufferSize {
                stream_id,
                stream_id_extension,
                p_std_buffer,
                bound: stream.p_std_buffer,
            });
        }
        let written_p_std_buffer = given_p_std_buffer
            .or((first_of_stream && header.is_some()).then_some(stream.p_std_buffer));
        if let Some(p_std_buffer) = written_p_std_buffer
            && let Some(sizes) = self.constraints().buffer_sizes(
                stream_id,
                stream.stream_type,
                stream.decoder_buffer_bound,
            )
            && !sizes.contains(&p_std_buffer.size_in_bytes())
        {
            return Err(Error::CspsBufferSize {
                stream_id,
                stream_id_extension,
                p_std_buffer,
                smallest: *sizes.start(),
                largest: *sizes.end(),
            });
        }
        let with_p_std_buffer;
        let packet = match header {
            Some(header) if first_of_stream && given_p_std_buffer.is_none() => {
                let mut header = header.clone();
                header
                    .pes_extension
                    .get_or_insert_with(PesExtension::new)
                    .p_std_buffer = Some(stream.p_std_buffer);
                let mut packet = packet.clone();
                packet.optional_header = Some(header);
                with_p_std_buffer = packet;
                &with_p_std_buffer
            }
            _ => packet,
        };

        let rate = self.rate();
        let largest_pack = self.largest_pack();
        let system_header = self.system_header();
        let Self {
            config,
            streams,
            program_stream_info,
            map_version,
            headers_sent_at,
            headers,
            header_loops,
            last_pack,
            pending,
            ..
        } = self;
        let pack_header_length = pack_header::FIXED_LENGTH;
        let program_mux_rate = config.program_mux_rate;
        let first_at = rate.plan(*last_pack, time, 0, 0).first_scr();
        let headers_due = headers_sent_at.zip(first_at).is_none_or(|(sent_at, at)| {
            at.ticks_since(sent_at) >= SystemTimeClock::ticks_in(config.header_interval)
        });
        headers.clear();
        let system_header_length = if headers_due {
            stream_definition::write_headers(
                streams,
                program_stream_info,
                *map_version,
                system_header,
                header_loops,
                headers,
            )?
        } else {
            0
        };
        let plan = rate.plan(
            *last_pack,
            time,
            system_header_length,
            headers.len().saturating_sub(system_header_length),
        );
        let first_headers = plan.first_headers().len();
        let room = largest_pack.saturating_sub(pack_header_length);
        let longest_continued = room.min(pes::LONGEST_PES_PACKET);
        let longest = room
            .saturating_sub(first_headers)
            .min(pes::LONGEST_PES_PACKET);
        let length = pack_header_length
            .saturating_add(first_headers)
            .saturating_add(packet.encoded_len().saturating_sub(packet.payload.len()))
            .saturating_add(packet.payload.len().min(1));
        if length > largest_pack {
            return Err(Error::PackLength { length });
        }
        let pieces = PesPacketPieces::new(packet, longest, longest_continued);
        let gap = plan.gap();
        let gap_length = gap.map_or(0, |gap| usize::try_from(gap.bytes()).unwrap_or(usize::MAX));
        if gap_length > config.buffer_limit {
            return Err(Error::TimestampGap {
                gap: clock
                    .zip(time)
                    .map_or(0, |(clock, time)| time.timestamp_ticks_since(clock)),
            });
        }
        let length = plan
            .header_packs()
            .map(|(pack, _, _)| pack.length())
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
            let pack_header = |at| pack_header_at(at, mux_delay, program_mux_rate);
            for pack in gap.into_iter().flat_map(Gap::iter) {
                writer.write_padding_pack(&pack_header(pack.at()), pack.length(), out)?;
                written = Some(pack);
            }
            for (pack, carried, fill) in plan.header_packs() {
                writer.write_pack_header(
                    &pack_header(pack.at()).with_pack_stuffing_length(fill.pack_stuffing_length()),
                    out,
                )?;
                out.extend_from_slice(headers.get(carried).unwrap_or_default());
                fill.write_padding(out);
                written = Some(pack);
            }
            for (index, piece) in pieces.enumerate() {
                let Some((pack, carried, fill)) =
                    plan.pes_pack(index, piece.encoded_len(), written)
                else {
                    return Err(missing_pts);
                };
                writer.write_pack_header(
                    &pack_header(pack.at()).with_pack_stuffing_length(fill.pack_stuffing_length()),
                    out,
                )?;
                out.extend_from_slice(headers.get(carried).unwrap_or_default());
                writer.write_pes_packets(&piece, out)?;
                fill.write_padding(out);
                written = Some(pack);
            }
            Ok(())
        })?;
        *last_pack = written;
        if headers_due {
            *headers_sent_at = plan.headers_scr();
        }
        if let (Some(time), Some(stream)) = (time, streams.get_mut(index)) {
            stream.last_timestamp = Some(time);
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
                    let header = pack_header_at(
                        closing.at(),
                        self.config.mux_delay,
                        self.config.program_mux_rate,
                    );
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
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "a stream is cut at the lengths it gives"
)]
mod tests;
