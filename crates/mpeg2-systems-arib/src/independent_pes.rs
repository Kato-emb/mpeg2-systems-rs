//! Independent PES data, which carries the data of a data broadcast, such
//! as captions, in the payload of a PES packet
//!
//! ARIB STD-B24 Volume 3 Chapter 5 defines two kinds, each a type here:
//! [`SynchronizedPesData`], whose presentation the PTS of its PES packet
//! times, and [`AsynchronousPesData`], which has no PTS. Captions are
//! carried in the first and superimposes in the second, but neither kind
//! says which it carries. A type is converted from the [`PesPacket`] the demuxer reports
//! with `TryFrom`, and writes the `PES_packet_data_byte` of a [`PesPacket`]
//! the caller builds.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use mpeg2_systems::field::{self, FieldReader};
use mpeg2_systems::pes::{PesPacket, StreamId};

/// Largest `PES_data_packet_header_length`, a 4-bit field
#[cfg(feature = "alloc")]
const MAX_PRIVATE_DATA_LENGTH: usize = 0x0f;

/// Bytes from `data_identifier` to `PES_data_packet_header_length`
const HEADER_LENGTH: usize = 3;

/// `data_identifier` of synchronized PES data
const SYNCHRONIZED_DATA_IDENTIFIER: u8 = 0x80;

/// `data_identifier` of asynchronous PES data
const ASYNCHRONOUS_DATA_IDENTIFIER: u8 = 0x81;

/// Failure to read or write independent PES data
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The PES packet is of another `stream_id` than the kind of PES data
    /// is carried in
    StreamId {
        /// The `stream_id` of the packet
        found: StreamId,
    },
    /// The `data_identifier` is not that of the kind of PES data
    DataIdentifier {
        /// The `data_identifier` of the kind
        expected: u8,
        /// The `data_identifier` read
        found: u8,
    },
    /// A field runs past the end of the payload
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
    /// The PES data to write has more `PES_data_private_data_byte` than the
    /// 15 the 4 bits of `PES_data_packet_header_length` count
    PesDataPacketHeaderLength {
        /// The bytes of private data given
        length: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StreamId { found } => write!(
                formatter,
                "stream_id 0x{:02x} does not carry this kind of PES data",
                found.get()
            ),
            Self::DataIdentifier { expected, found } => write!(
                formatter,
                "data_identifier 0x{found:02x} is not 0x{expected:02x}"
            ),
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read")
            }
            Self::PesDataPacketHeaderLength { length } => write!(
                formatter,
                "PES_data_private_data_byte of {length} bytes is longer than the 15 \
                 PES_data_packet_header_length counts"
            ),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::StreamId { .. }
            | Self::DataIdentifier { .. }
            | Self::PesDataPacketHeaderLength { .. } => None,
        }
    }
}

/// Synchronized PES data, the payload of a PES packet whose presentation
/// its PTS times, as captions are
///
/// `Synchronized_PES_data`, ARIB STD-B24 Volume 3 §5.1, Table 5-1
///
/// It is carried in a PES packet of `stream_id` 0xBD (`private_stream_1`).
/// `data_identifier` is 0x80 for every such data, so it is not a field;
/// `private_stream_id` and the `reserved_future_use` bits are not kept, and
/// are written 0xFF and `'1111'`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::{OptionalHeader, PesPacket, StreamId};
/// use mpeg2_systems::time::Timestamp;
/// use mpeg2_systems_arib::independent_pes::SynchronizedPesData;
///
/// // Synchronized PES data of 2 bytes after 2 bytes of private data, read from
/// // the payload of a PES packet
/// let packet = PesPacket::new(
///     StreamId::PRIVATE_STREAM_1,
///     &[0x80, 0xff, 0xf2, 0xaa, 0xbb, 0x01, 0x02],
/// );
/// let data = SynchronizedPesData::try_from(&packet)?;
/// assert_eq!(
///     data,
///     SynchronizedPesData::new(&[0x01, 0x02]).with_pes_data_private_data_byte(&[0xaa, 0xbb])
/// );
///
/// // Written back (with the `alloc` feature) as the payload of a PES packet
/// // the caller builds, with its PTS
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut payload = Vec::new();
/// data.write(&mut payload)?;
/// let packet = PesPacket::new(StreamId::PRIVATE_STREAM_1, &payload)
///     .with_optional_header(OptionalHeader::new().with_pts(Timestamp::try_from(90_000)?));
/// let mut written = Vec::new();
/// packet.write(&mut written)?;
/// assert_eq!(
///     SynchronizedPesData::try_from(&PesPacket::read(&written)?)?,
///     data
/// );
/// # }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[doc(alias = "Synchronized_PES_data")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SynchronizedPesData<'input> {
    /// Private data of the PES data, which a receiver may pass over
    pub pes_data_private_data_byte: &'input [u8],
    /// The data the PES packet carries, such as data groups of captions
    pub synchronized_pes_data_byte: &'input [u8],
}

impl<'input> SynchronizedPesData<'input> {
    /// Creates the PES data of `synchronized_pes_data_byte`, without private
    /// data
    #[must_use]
    pub const fn new(synchronized_pes_data_byte: &'input [u8]) -> Self {
        Self {
            pes_data_private_data_byte: &[],
            synchronized_pes_data_byte,
        }
    }

    /// Returns the value with the given `pes_data_private_data_byte`
    #[must_use]
    pub const fn with_pes_data_private_data_byte(
        mut self,
        pes_data_private_data_byte: &'input [u8],
    ) -> Self {
        self.pes_data_private_data_byte = pes_data_private_data_byte;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        encoded_len(
            self.pes_data_private_data_byte,
            self.synchronized_pes_data_byte,
        )
    }

    /// Appends the PES data to `out`, as the `PES_packet_data_byte` of a PES
    /// packet
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`PesDataPacketHeaderLength`](Error::PesDataPacketHeaderLength): the
    /// private data is longer than 15 bytes.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write(
            out,
            SYNCHRONIZED_DATA_IDENTIFIER,
            self.pes_data_private_data_byte,
            self.synchronized_pes_data_byte,
        )
    }
}

impl<'input> TryFrom<&PesPacket<'input>> for SynchronizedPesData<'input> {
    type Error = Error;

    /// Takes the PES data from the payload of a PES packet
    ///
    /// # Errors
    ///
    /// * [`StreamId`](Error::StreamId): the `stream_id` is not 0xBD.
    /// * [`DataIdentifier`](Error::DataIdentifier): the `data_identifier`
    ///   is not 0x80.
    /// * [`Field`](Error::Field): the payload ends inside the PES data
    ///   packet header or its private data.
    fn try_from(packet: &PesPacket<'input>) -> Result<Self, Self::Error> {
        let (pes_data_private_data_byte, synchronized_pes_data_byte) = read(
            packet,
            StreamId::PRIVATE_STREAM_1,
            SYNCHRONIZED_DATA_IDENTIFIER,
        )?;

        Ok(Self {
            pes_data_private_data_byte,
            synchronized_pes_data_byte,
        })
    }
}

/// Asynchronous PES data, the payload of a PES packet presented as it
/// arrives, as superimposes are
///
/// `Asynchronous_PES_data`, ARIB STD-B24 Volume 3 §5.2, Table 5-2
///
/// It is carried in a PES packet of `stream_id` 0xBF (`private_stream_2`),
/// which has no PTS. `data_identifier` is 0x81 for every such data, so it is
/// not a field; `private_stream_id` and the `reserved_future_use` bits are
/// not kept, and are written 0xFF and `'1111'`.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::pes::{PesPacket, StreamId};
/// use mpeg2_systems_arib::independent_pes::AsynchronousPesData;
///
/// // Asynchronous PES data of 1 byte after 1 byte of private data
/// let packet = PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0x81, 0xff, 0xf1, 0xaa, 0x01]);
/// let data = AsynchronousPesData::try_from(&packet)?;
/// assert_eq!(
///     data,
///     AsynchronousPesData::new(&[0x01]).with_pes_data_private_data_byte(&[0xaa])
/// );
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut payload = Vec::new();
/// data.write(&mut payload)?;
/// assert_eq!(payload, packet.payload);
/// # }
/// # Ok::<(), mpeg2_systems_arib::independent_pes::Error>(())
/// ```
#[doc(alias = "Asynchronous_PES_data")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AsynchronousPesData<'input> {
    /// Private data of the PES data, which a receiver may pass over
    pub pes_data_private_data_byte: &'input [u8],
    /// The data the PES packet carries, such as data groups of superimposes
    pub asynchronous_pes_data_byte: &'input [u8],
}

impl<'input> AsynchronousPesData<'input> {
    /// Creates the PES data of `asynchronous_pes_data_byte`, without private
    /// data
    #[must_use]
    pub const fn new(asynchronous_pes_data_byte: &'input [u8]) -> Self {
        Self {
            pes_data_private_data_byte: &[],
            asynchronous_pes_data_byte,
        }
    }

    /// Returns the value with the given `pes_data_private_data_byte`
    #[must_use]
    pub const fn with_pes_data_private_data_byte(
        mut self,
        pes_data_private_data_byte: &'input [u8],
    ) -> Self {
        self.pes_data_private_data_byte = pes_data_private_data_byte;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        encoded_len(
            self.pes_data_private_data_byte,
            self.asynchronous_pes_data_byte,
        )
    }

    /// Appends the PES data to `out`, as the `PES_packet_data_byte` of a PES
    /// packet
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`PesDataPacketHeaderLength`](Error::PesDataPacketHeaderLength): the
    /// private data is longer than 15 bytes.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        write(
            out,
            ASYNCHRONOUS_DATA_IDENTIFIER,
            self.pes_data_private_data_byte,
            self.asynchronous_pes_data_byte,
        )
    }
}

impl<'input> TryFrom<&PesPacket<'input>> for AsynchronousPesData<'input> {
    type Error = Error;

    /// Takes the PES data from the payload of a PES packet
    ///
    /// # Errors
    ///
    /// * [`StreamId`](Error::StreamId): the `stream_id` is not 0xBF.
    /// * [`DataIdentifier`](Error::DataIdentifier): the `data_identifier`
    ///   is not 0x81.
    /// * [`Field`](Error::Field): the payload ends inside the PES data
    ///   packet header or its private data.
    fn try_from(packet: &PesPacket<'input>) -> Result<Self, Self::Error> {
        let (pes_data_private_data_byte, asynchronous_pes_data_byte) = read(
            packet,
            StreamId::PRIVATE_STREAM_2,
            ASYNCHRONOUS_DATA_IDENTIFIER,
        )?;

        Ok(Self {
            pes_data_private_data_byte,
            asynchronous_pes_data_byte,
        })
    }
}

/// Splits the payload of `packet` into its private data and its data, after
/// checking that `stream_id` and `data_identifier` are those of the kind
fn read<'input>(
    packet: &PesPacket<'input>,
    stream_id: StreamId,
    data_identifier: u8,
) -> Result<(&'input [u8], &'input [u8]), Error> {
    if packet.stream_id != stream_id {
        return Err(Error::StreamId {
            found: packet.stream_id,
        });
    }
    let mut reader = FieldReader::new(packet.payload);
    let field = |field| move |error| Error::Field { field, error };
    let found = reader.read_u8::<8>().map_err(field("data_identifier"))?;
    if found != data_identifier {
        return Err(Error::DataIdentifier {
            expected: data_identifier,
            found,
        });
    }
    // Why not keep private_stream_id and reserved_future_use: they are
    // "Unused (0xFF)" and reserved, and are written as such.
    reader.read_u8::<8>().map_err(field("private_stream_id"))?;
    reader
        .read_u8::<4>()
        .map_err(field("reserved_future_use"))?;
    let private_data_length = reader
        .read_u8::<4>()
        .map_err(field("PES_data_packet_header_length"))?;
    let private_data = reader
        .read_bytes(usize::from(private_data_length))
        .map_err(field("PES_data_private_data_byte"))?;
    let data = reader
        .read_bytes(reader.remaining_bytes())
        .unwrap_or_default();

    Ok((private_data, data))
}

/// Returns the bytes [`write`] appends
const fn encoded_len(private_data: &[u8], data: &[u8]) -> usize {
    HEADER_LENGTH
        .saturating_add(private_data.len())
        .saturating_add(data.len())
}

/// Appends the PES data of `data_identifier` to `out`
#[cfg(feature = "alloc")]
fn write(
    out: &mut Vec<u8>,
    data_identifier: u8,
    private_data: &[u8],
    data: &[u8],
) -> Result<(), Error> {
    let length = private_data.len();
    let header_length = u8::try_from(length)
        .ok()
        .filter(|&length| usize::from(length) <= MAX_PRIVATE_DATA_LENGTH)
        .ok_or(Error::PesDataPacketHeaderLength { length })?;
    out.push(data_identifier);
    out.push(0xff);
    out.push(0xf0 | header_length);
    out.extend_from_slice(private_data);
    out.extend_from_slice(data);

    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use mpeg2_systems::field;
    use mpeg2_systems::pes::{PesPacket, StreamId};

    use super::{AsynchronousPesData, Error, SynchronizedPesData};

    /// The PES packet of caption management data (PTS 1 s) on PID 0x0130 of
    /// the Task 26 spike's `arib-caption2.ts`, which ffprobe's
    /// libaribcaption read as an ARIB caption packet
    const MANAGEMENT: [u8; 34] = [
        0x00, 0x00, 0x01, 0xbd, 0x00, 0x1c, 0x80, 0x80, 0x05, 0x21, 0x00, 0x05, 0xbf, 0x21, 0x80,
        0xff, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00,
        0x00, 0x00, 0xb0, 0x78,
    ];

    #[test]
    fn the_bytes_after_the_pes_data_packet_header_are_the_synchronized_pes_data() {
        let packet = PesPacket::read(&MANAGEMENT).unwrap();
        let mut unspecified = packet.clone();
        unspecified.length_unspecified = true;

        assert_eq!(
            [&packet, &unspecified].map(SynchronizedPesData::try_from),
            [
                Ok(SynchronizedPesData::new(MANAGEMENT.get(17..).unwrap())),
                Ok(SynchronizedPesData::new(MANAGEMENT.get(17..).unwrap()))
            ]
        );
    }

    #[test]
    fn a_packet_of_another_stream_id_or_data_identifier_is_refused() {
        let synchronized = PesPacket::new(StreamId::PRIVATE_STREAM_1, &[0x80, 0xff, 0xf0]);
        let asynchronous = PesPacket::new(StreamId::PRIVATE_STREAM_2, &[0x80, 0xff, 0xf0]);

        assert_eq!(
            [
                SynchronizedPesData::try_from(&asynchronous).err(),
                AsynchronousPesData::try_from(&synchronized).err(),
                AsynchronousPesData::try_from(&asynchronous).err(),
            ],
            [
                Some(Error::StreamId {
                    found: StreamId::PRIVATE_STREAM_2
                }),
                Some(Error::StreamId {
                    found: StreamId::PRIVATE_STREAM_1
                }),
                Some(Error::DataIdentifier {
                    expected: 0x81,
                    found: 0x80
                }),
            ]
        );
    }

    #[test]
    fn a_payload_that_ends_inside_the_pes_data_packet_header_is_refused() {
        assert_eq!(
            [&[0x80][..], &[0x80, 0xff, 0xf2, 0x01]].map(|payload| {
                SynchronizedPesData::try_from(&PesPacket::new(StreamId::PRIVATE_STREAM_1, payload))
            }),
            [
                Err(Error::Field {
                    field: "private_stream_id",
                    error: field::Error::Truncated {
                        required_bits: 8,
                        remaining_bits: 0
                    }
                }),
                Err(Error::Field {
                    field: "PES_data_private_data_byte",
                    error: field::Error::Truncated {
                        required_bits: 16,
                        remaining_bits: 8
                    }
                }),
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn pes_data_is_written_back_as_read() {
        let mut asynchronous = AsynchronousPesData::new(&[0x03]);
        asynchronous.pes_data_private_data_byte = &[0x01, 0x02];
        let mut synchronized_bytes = Vec::new();
        let mut asynchronous_bytes = Vec::new();

        assert_eq!(
            [
                SynchronizedPesData::new(MANAGEMENT.get(17..).unwrap())
                    .write(&mut synchronized_bytes),
                asynchronous.write(&mut asynchronous_bytes)
            ],
            [Ok(()), Ok(())]
        );
        assert_eq!(synchronized_bytes, MANAGEMENT.get(14..).unwrap());
        assert_eq!(asynchronous_bytes, [0x81, 0xff, 0xf2, 0x01, 0x02, 0x03]);
        assert_eq!(
            AsynchronousPesData::try_from(&PesPacket::new(
                StreamId::PRIVATE_STREAM_2,
                &asynchronous_bytes
            )),
            Ok(asynchronous)
        );
        assert_eq!(asynchronous.encoded_len(), asynchronous_bytes.len());
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn private_data_past_15_bytes_is_refused_and_leaves_the_output_as_it_was() {
        let mut data = SynchronizedPesData::new(&[]);
        data.pes_data_private_data_byte = &[0; 16];
        let mut written = Vec::from([0x47]);

        assert_eq!(
            data.write(&mut written),
            Err(Error::PesDataPacketHeaderLength { length: 16 })
        );
        assert_eq!(written, [0x47]);
    }
}
