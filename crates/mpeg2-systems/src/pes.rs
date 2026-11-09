//! PES packet, the unit both a Transport Stream and a Program Stream carry
//! elementary streams in

pub mod dsm_trick_mode;
pub mod extension;
pub mod optional_header;
pub mod scrambling_control;
pub mod stream_id;

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use crate::field::{self, FieldReader};
#[cfg(feature = "alloc")]
use crate::field::{FieldWriter, write_or_truncate};
pub use optional_header::OptionalHeader;
pub use stream_id::StreamId;

/// `packet_start_code_prefix`, the 24 bits every PES packet starts with
const PACKET_START_CODE_PREFIX: u32 = 0x00_0001;

/// Bytes from `packet_start_code_prefix` to `PES_packet_length`
pub(crate) const PREFIX_LENGTH: usize = 6;

/// The most bytes of a PES packet of a specified length: its prefix and the
/// 65535 bytes `PES_packet_length` can count
#[cfg(feature = "alloc")]
pub(crate) const LONGEST_PES_PACKET: usize = PREFIX_LENGTH + 0xffff;

/// Failure to read or write a PES packet
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The packet does not start with `packet_start_code_prefix` 0x000001
    PacketStartCodePrefix {
        /// The 24 bits found
        packet_start_code_prefix: u32,
    },
    /// The bytes given are not as many as `PES_packet_length` declares
    PesPacketLength {
        /// The `PES_packet_length` read
        pes_packet_length: u16,
        /// The bytes given after `PES_packet_length`
        available_bytes: usize,
    },
    /// The optional header does not start with the bits `'10'`
    OptionalHeaderStart {
        /// The two bits found
        bits: u8,
    },
    /// `PES_header_data_length` runs past the end of the packet
    PesHeaderDataLength {
        /// The `PES_header_data_length` read
        pes_header_data_length: u8,
        /// The bytes of the packet after `PES_header_data_length`
        available_bytes: usize,
    },
    /// `PTS_DTS_flags` is `'01'`, which is forbidden, or a header to write
    /// has a DTS without a PTS
    ForbiddenPtsDtsFlags,
    /// A field runs past the end of the field that frames it, or does not
    /// fit its width when written
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// An ESCR to write has an extension of 300 or more, which the
    /// specification does not allow
    EscrExtension {
        /// The extension given
        extension: u16,
    },
    /// A run of bytes to write is longer than the field that counts it can state
    TooLong {
        /// The name of the length field in the syntax table
        field: &'static str,
        /// The length given, in bytes
        length: usize,
        /// The longest the field can state, in bytes
        limit: usize,
    },
    /// A packet to write has an optional header where its `stream_id` has
    /// none, or none where its `stream_id` has one
    OptionalHeaderPresence {
        /// The `stream_id` of the packet
        stream_id: StreamId,
    },
    /// A value to write is one the specification forbids for its field
    Forbidden {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The value given
        value: u32,
    },
    /// A packet to write would have `PES_packet_length` 0, leaving its
    /// length unspecified, though it is not of a video elementary stream
    LengthUnspecified {
        /// The `stream_id` of the packet
        stream_id: StreamId,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketStartCodePrefix {
                packet_start_code_prefix,
            } => write!(
                formatter,
                "packet_start_code_prefix {packet_start_code_prefix:#08x} is not 0x000001"
            ),
            Self::PesPacketLength {
                pes_packet_length,
                available_bytes,
            } => write!(
                formatter,
                "PES_packet_length {pes_packet_length} does not match the {available_bytes} \
                 bytes given"
            ),
            Self::OptionalHeaderStart { bits } => write!(
                formatter,
                "optional PES header starts with '{bits:02b}', not '10'"
            ),
            Self::PesHeaderDataLength {
                pes_header_data_length,
                available_bytes,
            } => write!(
                formatter,
                "PES_header_data_length {pes_header_data_length} runs past the \
                 {available_bytes} bytes left in the packet"
            ),
            Self::ForbiddenPtsDtsFlags => write!(
                formatter,
                "PTS_DTS_flags '01' is forbidden: a DTS needs a PTS"
            ),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::EscrExtension { extension } => {
                write!(formatter, "ESCR_extension {extension} is not below 300")
            }
            Self::TooLong {
                field,
                length,
                limit,
            } => write!(
                formatter,
                "{field} of {length} bytes is longer than {limit} bytes"
            ),
            Self::OptionalHeaderPresence { stream_id } => write!(
                formatter,
                "stream_id {:#04x} and the presence of an optional PES header disagree",
                stream_id.get()
            ),
            Self::Forbidden { field, value } => {
                write!(formatter, "{field} {value} is forbidden")
            }
            Self::LengthUnspecified { stream_id } => write!(
                formatter,
                "PES_packet_length 0 is for video only, not stream_id {:#04x}",
                stream_id.get()
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::PacketStartCodePrefix { .. }
            | Self::PesPacketLength { .. }
            | Self::OptionalHeaderStart { .. }
            | Self::PesHeaderDataLength { .. }
            | Self::ForbiddenPtsDtsFlags
            | Self::EscrExtension { .. }
            | Self::TooLong { .. }
            | Self::OptionalHeaderPresence { .. }
            | Self::Forbidden { .. }
            | Self::LengthUnspecified { .. } => None,
        }
    }
}

/// Names the field a [`field::Error`] arose in
const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

/// How long a PES packet is, as far as its first bytes tell
///
/// The value [`encoded_len`] returns.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncodedLen {
    /// Fewer than the 6 bytes up to `PES_packet_length` were given
    NeedMoreData,
    /// The packet is this many bytes long, the 6 bytes up to
    /// `PES_packet_length` included
    Bounded(usize),
    /// `PES_packet_length` is 0: the packet runs to the next packet of its
    /// PID with `payload_unit_start_indicator` set
    Unbounded,
}

/// Returns the number of bytes of the PES packet that `prefix` starts, read
/// from its first 6 bytes
///
/// `PES_packet_length`, ISO/IEC 13818-1 §2.4.3.7
///
/// The value [`Bounded`](EncodedLen::Bounded) holds is what
/// [`PesPacket::encoded_len`] gives for the packet [`PesPacket::read`] reads
/// from those bytes, or [`NeedMoreData`](EncodedLen::NeedMoreData) when fewer
/// than 6 are given. It is not the value of the field: it adds 6 to
/// `PES_packet_length`. `packet_start_code_prefix` is not checked;
/// [`PesPacket::read`] checks it. A length of 0, which the specification
/// allows only for video carried in a Transport Stream, leaves the end of the
/// packet to the next `payload_unit_start_indicator`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::{self, EncodedLen};
///
/// assert_eq!(pes::encoded_len(&[0x00, 0x00, 0x01]), EncodedLen::NeedMoreData);
/// assert_eq!(
///     pes::encoded_len(&[0x00, 0x00, 0x01, 0xc0, 0x0b, 0x75]),
///     EncodedLen::Bounded(2939)
/// );
/// assert_eq!(
///     pes::encoded_len(&[0x00, 0x00, 0x01, 0xe0, 0x00, 0x00]),
///     EncodedLen::Unbounded
/// );
/// ```
#[must_use]
pub fn encoded_len(prefix: &[u8]) -> EncodedLen {
    let Some([.., high, low]) = prefix.first_chunk::<PREFIX_LENGTH>() else {
        return EncodedLen::NeedMoreData;
    };

    match u16::from_be_bytes([*high, *low]) {
        0 => EncodedLen::Unbounded,
        length => EncodedLen::Bounded(PREFIX_LENGTH.saturating_add(usize::from(length))),
    }
}

/// PES packet, the unit an elementary stream is cut into for carriage
///
/// `PES_packet`, ISO/IEC 13818-1 §2.4.3.6
///
/// `PES_packet_length` follows from the optional header and the payload,
/// unless [`length_unspecified`](Self::length_unspecified) writes it as 0. The
/// optional header is present exactly when
/// [`StreamId::carries_optional_header`] says so. The crate does not gather a
/// PES packet from Transport Packets; the demultiplexer does.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::{Error, PesPacket, StreamId};
///
/// // A padding stream packet of three padding bytes
/// let bytes = [0x00, 0x00, 0x01, 0xbe, 0x00, 0x03, 0xff, 0xff, 0xff];
/// let packet = PesPacket::read(&bytes)?;
/// assert_eq!(packet, PesPacket::new(StreamId::PADDING_STREAM, &[0xff; 3]));
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// packet.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Error>(())
/// ```
#[doc(alias("PES_packet", "PES"))]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PesPacket<'input> {
    /// The elementary stream the packet carries
    pub stream_id: StreamId,
    /// Whether `PES_packet_length` is 0, leaving the length unspecified
    pub length_unspecified: bool,
    /// The fields between `PES_packet_length` and the payload, for the
    /// streams that have them
    pub optional_header: Option<OptionalHeader<'input>>,
    /// The `PES_packet_data_byte`s, or the `padding_byte`s of a
    /// `padding_stream`
    pub payload: &'input [u8],
}

impl<'input> PesPacket<'input> {
    /// Creates a packet of `stream_id` carrying `payload`, with no optional
    /// header and its length specified
    #[must_use]
    pub const fn new(stream_id: StreamId, payload: &'input [u8]) -> Self {
        Self {
            stream_id,
            length_unspecified: false,
            optional_header: None,
            payload,
        }
    }

    /// Returns the number of bytes after `PES_packet_length`
    fn body_length(&self) -> usize {
        self.optional_header
            .as_ref()
            .map_or(0, OptionalHeader::encoded_len)
            .saturating_add(self.payload.len())
    }

    /// Reads a PES packet from `bytes`, which hold it whole and nothing more
    ///
    /// With `PES_packet_length` 0, the packet is all of `bytes`.
    ///
    /// # Errors
    ///
    /// * [`Field`](Error::Field): `bytes` end before `PES_packet_length`, or
    ///   a field of the optional header runs past it.
    /// * [`PacketStartCodePrefix`](Error::PacketStartCodePrefix): `bytes` do
    ///   not start with 0x000001.
    /// * [`PesPacketLength`](Error::PesPacketLength): `PES_packet_length`
    ///   does not match `bytes`.
    /// * [`OptionalHeaderStart`](Error::OptionalHeaderStart),
    ///   [`PesHeaderDataLength`](Error::PesHeaderDataLength),
    ///   [`ForbiddenPtsDtsFlags`](Error::ForbiddenPtsDtsFlags): the optional
    ///   header is malformed.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let mut reader = FieldReader::new(bytes);
        // Why not an error per field: with 6 bytes there, none of these reads
        // can fail, and with fewer the length field is the one cut short.
        let prefix = (|| {
            Ok((
                reader.read_u32::<24>()?,
                StreamId::from(reader.read_u8::<8>()?),
                reader.read_u16::<16>()?,
            ))
        })();
        let (packet_start_code_prefix, stream_id, pes_packet_length) =
            prefix.map_err(field_error("PES_packet_length"))?;
        if packet_start_code_prefix != PACKET_START_CODE_PREFIX {
            return Err(Error::PacketStartCodePrefix {
                packet_start_code_prefix,
            });
        }
        let body = bytes.get(PREFIX_LENGTH..).unwrap_or_default();
        if pes_packet_length != 0 && body.len() != usize::from(pes_packet_length) {
            return Err(Error::PesPacketLength {
                pes_packet_length,
                available_bytes: body.len(),
            });
        }

        let (optional_header, payload) = if stream_id.carries_optional_header() {
            let pes_header_data_length = body
                .get(optional_header::FIXED_LENGTH.wrapping_sub(1))
                .copied()
                .unwrap_or(0);
            let (header, payload) = body
                .split_at_checked(
                    usize::from(pes_header_data_length)
                        .saturating_add(optional_header::FIXED_LENGTH),
                )
                .ok_or(Error::PesHeaderDataLength {
                    pes_header_data_length,
                    available_bytes: body.len().saturating_sub(optional_header::FIXED_LENGTH),
                })?;
            (Some(OptionalHeader::read(header)?), payload)
        } else {
            (None, body)
        };

        Ok(Self {
            stream_id,
            length_unspecified: pes_packet_length == 0,
            optional_header,
            payload,
        })
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        PREFIX_LENGTH.saturating_add(self.body_length())
    }

    /// Appends the packet to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`OptionalHeaderPresence`](Error::OptionalHeaderPresence): the
    ///   optional header is present or absent against
    ///   [`StreamId::carries_optional_header`].
    /// * [`LengthUnspecified`](Error::LengthUnspecified): `PES_packet_length`
    ///   would be 0, left unspecified or with nothing after it, for a stream
    ///   that is not video.
    /// * [`TooLong`](Error::TooLong): the packet after `PES_packet_length` is
    ///   longer than 65535 bytes and its length is specified.
    /// * Any error of [`OptionalHeader::write`].
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.optional_header.is_some() != self.stream_id.carries_optional_header() {
            return Err(Error::OptionalHeaderPresence {
                stream_id: self.stream_id,
            });
        }
        let pes_packet_length = if self.length_unspecified {
            0
        } else {
            let length = self.body_length();
            u16::try_from(length).map_err(|_| Error::TooLong {
                field: "PES_packet_length",
                length,
                limit: usize::from(u16::MAX),
            })?
        };
        // Why not only when `length_unspecified` is set: an empty packet with
        // no optional header comes to a length of 0 too, which reads back as
        // unspecified.
        if pes_packet_length == 0 && !self.stream_id.is_video() {
            return Err(Error::LengthUnspecified {
                stream_id: self.stream_id,
            });
        }

        write_or_truncate(out, |out| {
            let mut writer = FieldWriter::new(out);
            // Why not an error per field: each value fits its field's width.
            (|| {
                writer.write_bits::<24>(PACKET_START_CODE_PREFIX)?;
                writer.write_bits::<8>(self.stream_id.get())?;
                writer.write_bits::<16>(pes_packet_length)?;
                writer.finish()
            })()
            .map_err(field_error("PES_packet_length"))?;
            if let Some(header) = &self.optional_header {
                header.write(out)?;
            }
            out.extend_from_slice(self.payload);

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{EncodedLen, Error, OptionalHeader, PesPacket, StreamId};
    use crate::pes;

    #[test]
    fn a_stream_without_an_optional_header_has_every_byte_as_payload() {
        let bytes = [0x00, 0x00, 0x01, 0xbf, 0x00, 0x04, 0x80, 0x80, 0x05, 0x21];

        assert_eq!(
            PesPacket::read(&bytes),
            Ok(PesPacket::new(
                StreamId::PRIVATE_STREAM_2,
                &[0x80, 0x80, 0x05, 0x21]
            ))
        );
    }

    #[test]
    fn a_packet_of_unspecified_length_runs_to_the_end_of_the_bytes() {
        let bytes = [
            0x00, 0x00, 0x01, 0xe0, 0x00, 0x00, 0x80, 0x00, 0x00, 0xaa, 0xbb,
        ];
        let mut expected = PesPacket::new(StreamId::from(0xe0), &[0xaa, 0xbb]);
        expected.length_unspecified = true;
        expected.optional_header = Some(OptionalHeader::new());

        assert_eq!(PesPacket::read(&bytes), Ok(expected));
    }

    #[test]
    fn a_packet_that_does_not_start_with_the_start_code_prefix_is_refused() {
        assert_eq!(
            PesPacket::read(&[0x00, 0x00, 0x02, 0xbe, 0x00, 0x00]),
            Err(Error::PacketStartCodePrefix {
                packet_start_code_prefix: 0x00_0002
            })
        );
    }

    #[test]
    fn bytes_that_disagree_with_the_pes_packet_length_are_refused() {
        assert_eq!(
            PesPacket::read(&[0x00, 0x00, 0x01, 0xbe, 0x00, 0x03, 0xff, 0xff]),
            Err(Error::PesPacketLength {
                pes_packet_length: 3,
                available_bytes: 2
            })
        );
    }

    #[test]
    fn a_header_data_length_past_the_end_of_the_packet_is_refused() {
        assert_eq!(
            PesPacket::read(&[0x00, 0x00, 0x01, 0xc0, 0x00, 0x04, 0x80, 0x80, 0x05, 0x21]),
            Err(Error::PesHeaderDataLength {
                pes_header_data_length: 5,
                available_bytes: 1
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn an_empty_packet_of_a_stream_other_than_video_is_refused_its_length_of_zero() {
        let mut written = Vec::new();

        assert_eq!(
            PesPacket::new(StreamId::PADDING_STREAM, &[]).write(&mut written),
            Err(Error::LengthUnspecified {
                stream_id: StreamId::PADDING_STREAM
            })
        );
        assert_eq!(written, []);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn packets_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let long_payload = [0; 65_536];
        let too_long = PesPacket::new(StreamId::PADDING_STREAM, &long_payload);
        let mut unspecified_audio = PesPacket::new(StreamId::from(0xc0), &[]);
        unspecified_audio.optional_header = Some(OptionalHeader::new());
        unspecified_audio.length_unspecified = true;
        let missing_header = PesPacket::new(StreamId::from(0xe0), &[]);
        let mut extra_header = PesPacket::new(StreamId::PADDING_STREAM, &[]);
        extra_header.optional_header = Some(OptionalHeader::new());
        let mut written = Vec::new();

        assert_eq!(
            [
                too_long.write(&mut written),
                unspecified_audio.write(&mut written),
                missing_header.write(&mut written),
                extra_header.write(&mut written),
            ],
            [
                Err(Error::TooLong {
                    field: "PES_packet_length",
                    length: 65_536,
                    limit: 65_535
                }),
                Err(Error::LengthUnspecified {
                    stream_id: StreamId::from(0xc0)
                }),
                Err(Error::OptionalHeaderPresence {
                    stream_id: StreamId::from(0xe0)
                }),
                Err(Error::OptionalHeaderPresence {
                    stream_id: StreamId::PADDING_STREAM
                }),
            ]
        );
        assert_eq!(written, []);
    }

    #[test]
    fn fewer_than_6_bytes_have_no_encoded_length() {
        let prefix = [0x00, 0x00, 0x01, 0xc0, 0x00];

        for length in 0..=prefix.len() {
            assert_eq!(
                pes::encoded_len(prefix.get(..length).unwrap()),
                EncodedLen::NeedMoreData
            );
        }
    }

    #[test]
    fn the_encoded_length_counts_the_6_bytes_up_to_pes_packet_length() {
        assert_eq!(
            pes::encoded_len(&[0x00, 0x00, 0x01, 0xbe, 0x00, 0x03]),
            EncodedLen::Bounded(9)
        );
    }

    #[test]
    fn the_largest_pes_packet_length_gives_the_largest_encoded_length() {
        assert_eq!(
            pes::encoded_len(&[0x00, 0x00, 0x01, 0xc0, 0xff, 0xff]),
            EncodedLen::Bounded(65541)
        );
    }
}
