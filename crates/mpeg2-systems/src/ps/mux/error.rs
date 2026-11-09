//! How writing a Program Stream, or registering or multiplexing a stream,
//! fails

use core::fmt;

use crate::pes::extension::PStdBuffer;
use crate::pes::{self, StreamId};
use crate::ps::{pack_header, psm, system_header};
use crate::time::Timestamp;

/// Failure to write an element of a Program Stream, or to register or
/// multiplex a stream with a [`Muxer`](super::Muxer)
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The `stream_id` given is below 0xBD, which this entry point does not
    /// take: 0xBC, the `program_stream_map`, is written with
    /// [`ProgramStreamMap`](crate::ps::ProgramStreamMap), and those below it
    /// start the other elements of a Program Stream, such as a pack header
    StreamId {
        /// The `stream_id` given
        stream_id: StreamId,
    },
    /// The PES packet has `length_unspecified` set, which a Program Stream
    /// does not allow (ISO/IEC 13818-1 §2.4.3.7)
    LengthUnspecified {
        /// The `stream_id` of the packet
        stream_id: StreamId,
    },
    /// A pack header could not be written
    PackHeader {
        /// How the pack header failed
        error: pack_header::Error,
    },
    /// A system header could not be written
    SystemHeader {
        /// How the system header failed
        error: system_header::Error,
    },
    /// A program stream map could not be written
    ProgramStreamMap {
        /// How the program stream map failed
        error: psm::Error,
    },
    /// A PES packet could not be written
    PesPacket {
        /// How the PES packet failed
        error: pes::Error,
    },
    /// The `program_mux_rate` set is 0, which ISO/IEC 13818-1 §2.5.3.4
    /// forbids, or does not fit its 22 bits
    ProgramMuxRate {
        /// The `program_mux_rate` set
        program_mux_rate: u32,
    },
    /// The `pack_size` set cannot hold a pack header, the shortest PES packet
    /// header and a byte of payload
    PackSize {
        /// The `pack_size` set
        pack_size: usize,
    },
    /// The shortest pack that can carry what is given is longer than
    /// [`MuxerConfig::pack_size`](super::MuxerConfig::pack_size), or than
    /// the bytes that arrive within 0.7 s at the `program_mux_rate` set
    PackLength {
        /// The bytes of the shortest pack
        length: usize,
    },
    /// The stream given is registered already
    StreamInUse {
        /// The `stream_id` of the stream
        stream_id: StreamId,
        /// The `stream_id_extension` of the stream, if it has one
        stream_id_extension: Option<u8>,
    },
    /// No stream is registered of the `stream_id`, and the
    /// `stream_id_extension`, of the PES packet given
    UnregisteredStream {
        /// The `stream_id` of the PES packet
        stream_id: StreamId,
        /// The `stream_id_extension` of the PES packet, if it has one
        stream_id_extension: Option<u8>,
    },
    /// A `stream_id_extension` is given where the `stream_id` is not 0xFD,
    /// or none where it is
    StreamIdExtension {
        /// The `stream_id` given
        stream_id: StreamId,
    },
    /// Registering the stream would take `audio_bound` past 32
    AudioBound {
        /// The audio streams there would be
        audio_bound: usize,
    },
    /// Registering the stream would take `video_bound` past 16
    VideoBound {
        /// The video streams there would be
        video_bound: usize,
    },
    /// The first PES packet supplied for a stream that carries the optional
    /// header has no PTS, which this muxer requires to initialize the
    /// stream's timeline, or the first PES packet supplied to the muxer has
    /// no time stamp to start the SCRs from
    MissingPts {
        /// The `stream_id` of the stream
        stream_id: StreamId,
        /// The `stream_id_extension` of the stream, if it has one
        stream_id_extension: Option<u8>,
    },
    /// The P-STD buffer size a PES packet carries is larger than the bound
    /// registered for its stream, which the system header gives as at least
    /// the buffer of every PES packet of the stream (ISO/IEC 13818-1
    /// §2.5.3.6, §2.4.3.7)
    PStdBufferSize {
        /// The `stream_id` of the stream
        stream_id: StreamId,
        /// The `stream_id_extension` of the stream, if it has one
        stream_id_extension: Option<u8>,
        /// The P-STD buffer size the PES packet carries
        p_std_buffer: PStdBuffer,
        /// The bound registered for the stream
        bound: PStdBuffer,
    },
    /// The DTS of a PES packet, or its PTS when it has none, is before that
    /// of the PES packet given before it on the same stream
    DecreasingTimestamp {
        /// The `stream_id` of the stream
        stream_id: StreamId,
        /// The `stream_id_extension` of the stream, if it has one
        stream_id_extension: Option<u8>,
        /// The DTS, or the PTS, given
        timestamp: Timestamp,
        /// The DTS, or the PTS, of the PES packet before it
        previous: Timestamp,
    },
    /// The time stamp of the input leaps so far past the one before it that
    /// the packs of a pack header alone that fill the leap do not fit
    /// [`MuxerConfig::buffer_limit`](super::MuxerConfig::buffer_limit) even
    /// with nothing held
    TimestampGap {
        /// The leap, in ticks of the 90 kHz clock of PTS and DTS
        gap: u64,
    },
    /// The input would grow the output held for
    /// [`Muxer::poll_output`](super::Muxer::poll_output) past
    /// [`MuxerConfig::buffer_limit`](super::MuxerConfig::buffer_limit)
    BufferFull {
        /// The bytes the output held would grow to
        length: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamId { stream_id } => write!(
                formatter,
                "stream_id {:#04x} is below 0xbd, which is not taken here",
                stream_id.get()
            ),
            Self::LengthUnspecified { stream_id } => write!(
                formatter,
                "a PES packet of stream_id {:#04x} leaves its length unspecified, which a \
                 program stream does not allow",
                stream_id.get()
            ),
            Self::PackHeader { .. } => write!(formatter, "a pack header could not be written"),
            Self::SystemHeader { .. } => write!(formatter, "a system header could not be written"),
            Self::ProgramStreamMap { .. } => {
                write!(formatter, "a program stream map could not be written")
            }
            Self::PesPacket { .. } => write!(formatter, "a PES packet could not be written"),
            Self::ProgramMuxRate { program_mux_rate } => write!(
                formatter,
                "program_mux_rate {program_mux_rate} is outside 1 to 2^22 - 1"
            ),
            Self::PackSize { pack_size } => write!(
                formatter,
                "pack_size {pack_size} is shorter than the shortest pack"
            ),
            Self::PackLength { length } => write!(
                formatter,
                "the shortest pack to write takes {length} bytes, past pack_size or what \
                 program_mux_rate delivers in 0.7 s"
            ),
            Self::StreamInUse {
                stream_id,
                stream_id_extension,
            } => write!(
                formatter,
                "the stream of stream_id {:#04x}{} is registered already",
                stream_id.get(),
                ExtensionOf(*stream_id_extension)
            ),
            Self::UnregisteredStream {
                stream_id,
                stream_id_extension,
            } => write!(
                formatter,
                "no stream of stream_id {:#04x}{} is registered",
                stream_id.get(),
                ExtensionOf(*stream_id_extension)
            ),
            Self::StreamIdExtension { stream_id } => write!(
                formatter,
                "stream_id {:#04x} has a stream_id_extension where it is not 0xfd, or none \
                 where it is",
                stream_id.get()
            ),
            Self::AudioBound { audio_bound } => {
                write!(formatter, "audio_bound {audio_bound} is past 32")
            }
            Self::VideoBound { video_bound } => {
                write!(formatter, "video_bound {video_bound} is past 16")
            }
            Self::MissingPts {
                stream_id,
                stream_id_extension,
            } => write!(
                formatter,
                "the first PES packet of stream_id {:#04x}{} has no PTS",
                stream_id.get(),
                ExtensionOf(*stream_id_extension)
            ),
            Self::PStdBufferSize {
                stream_id,
                stream_id_extension,
                p_std_buffer,
                bound,
            } => write!(
                formatter,
                "a P-STD buffer of {} bytes on stream_id {:#04x}{} is past the {} bytes of its bound",
                p_std_buffer.size_in_bytes(),
                stream_id.get(),
                ExtensionOf(*stream_id_extension),
                bound.size_in_bytes()
            ),
            Self::DecreasingTimestamp {
                stream_id,
                stream_id_extension,
                timestamp,
                previous,
            } => write!(
                formatter,
                "time stamp {} of stream_id {:#04x}{} is before {} of the PES packet before it",
                timestamp.get(),
                stream_id.get(),
                ExtensionOf(*stream_id_extension),
                previous.get()
            ),
            Self::TimestampGap { gap } => write!(
                formatter,
                "a leap of {gap} ticks in the time stamps needs more packs than buffer_limit holds"
            ),
            Self::BufferFull { length } => write!(
                formatter,
                "the output held would grow to {length} bytes, past buffer_limit"
            ),
        }
    }
}

/// The `stream_id_extension` of a stream, as [`Error`] names it after its
/// `stream_id`
struct ExtensionOf(Option<u8>);

impl fmt::Display for ExtensionOf {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(extension) => write!(formatter, " and stream_id_extension {extension:#04x}"),
            None => Ok(()),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::PackHeader { error } => Some(error),
            Self::SystemHeader { error } => Some(error),
            Self::ProgramStreamMap { error } => Some(error),
            Self::PesPacket { error } => Some(error),
            Self::StreamId { .. }
            | Self::LengthUnspecified { .. }
            | Self::ProgramMuxRate { .. }
            | Self::PackSize { .. }
            | Self::PackLength { .. }
            | Self::StreamInUse { .. }
            | Self::UnregisteredStream { .. }
            | Self::StreamIdExtension { .. }
            | Self::AudioBound { .. }
            | Self::VideoBound { .. }
            | Self::MissingPts { .. }
            | Self::PStdBufferSize { .. }
            | Self::DecreasingTimestamp { .. }
            | Self::TimestampGap { .. }
            | Self::BufferFull { .. } => None,
        }
    }
}
