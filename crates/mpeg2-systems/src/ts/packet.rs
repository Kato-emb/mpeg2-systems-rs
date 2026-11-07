//! Transport Packet, the 188-byte unit of a Transport Stream, and the values
//! its header carries

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

use super::adaptation_field::{self, AdaptationField, MAX_ADAPTATION_FIELD_LENGTH};
#[cfg(feature = "alloc")]
use crate::field::FieldWriter;
use crate::field::{self, FieldReader};

/// Bytes in a Transport Packet
const PACKET_LENGTH: usize = 188;

/// Bytes in the header of a Transport Packet, `sync_byte` to `continuity_counter`
#[cfg(feature = "alloc")]
const HEADER_LENGTH: usize = 4;

/// Value of `sync_byte`
const SYNC_BYTE: u8 = 0x47;

/// Failure to read or write a Transport Packet
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The packet is not 188 bytes long
    PacketLength {
        /// The bytes given to read, or the bytes the header, the adaptation
        /// field and the payload add up to when writing
        length: usize,
    },
    /// The first byte is not `sync_byte` 0x47
    SyncByte {
        /// The byte found
        sync_byte: u8,
    },
    /// `adaptation_field_control` is `'00'`, which is reserved
    ReservedAdaptationFieldControl,
    /// `adaptation_field_length` does not fit what `adaptation_field_control`
    /// says the packet holds: 183 with no payload, 0 to 182 with one
    AdaptationFieldLength {
        /// The `adaptation_field_control` of the packet
        adaptation_field_control: AdaptationFieldControl,
        /// The `adaptation_field_length` found
        adaptation_field_length: usize,
    },
    /// A packet to write has neither an adaptation field nor a payload
    NoAdaptationFieldOrPayload,
    /// The adaptation field could not be read or written
    AdaptationField(adaptation_field::Error),
    /// A field of the header could not be read or written
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field failed
        error: field::Error,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketLength { length } => {
                write!(
                    formatter,
                    "transport packet of {length} bytes is not 188 bytes long"
                )
            }
            Self::SyncByte { sync_byte } => {
                write!(formatter, "sync_byte {sync_byte:#04x} is not 0x47")
            }
            Self::ReservedAdaptationFieldControl => {
                write!(formatter, "adaptation_field_control '00' is reserved")
            }
            Self::AdaptationFieldLength {
                adaptation_field_control,
                adaptation_field_length,
            } => write!(
                formatter,
                "adaptation_field_length {adaptation_field_length} does not fit \
                 adaptation_field_control '{:02b}'",
                adaptation_field_control.get()
            ),
            Self::NoAdaptationFieldOrPayload => write!(
                formatter,
                "transport packet has neither an adaptation field nor a payload"
            ),
            Self::AdaptationField(error) => write!(formatter, "adaptation field: {error}"),
            Self::Field { field, error } => write!(formatter, "{field}: {error}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::AdaptationField(error) => Some(error),
            Self::Field { error, .. } => Some(error),
            Self::PacketLength { .. }
            | Self::SyncByte { .. }
            | Self::ReservedAdaptationFieldControl
            | Self::AdaptationFieldLength { .. }
            | Self::NoAdaptationFieldOrPayload => None,
        }
    }
}

impl From<adaptation_field::Error> for Error {
    fn from(error: adaptation_field::Error) -> Self {
        Self::AdaptationField(error)
    }
}

/// Packet identifier, which names the stream a Transport Packet belongs to
///
/// `PID`, ISO/IEC 13818-1 §2.4.3.3, Table 2-3
///
/// The PIDs the specification reserves are associated constants. Which PIDs a
/// program uses is stated in the PSI; the crate does not choose PIDs to
/// follow.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::ts::{Pid, packet::PidOutOfRange};
///
/// assert_eq!(Pid::try_from(0x1fff), Ok(Pid::NULL));
/// assert_eq!(Pid::try_from(0x2000).map_err(PidOutOfRange::value), Err(0x2000));
/// ```
#[doc(alias = "PID")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid(u16);

impl Pid {
    /// PID of the program association table
    pub const PAT: Self = Self(0x0000);
    /// PID of the conditional access table
    pub const CAT: Self = Self(0x0001);
    /// PID of the transport stream description table
    pub const TSDT: Self = Self(0x0002);
    /// PID of the IPMP control information table
    pub const IPMP: Self = Self(0x0003);
    /// PID of null packets
    pub const NULL: Self = Self(0x1fff);

    /// Returns the 13-bit value
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl TryFrom<u16> for Pid {
    type Error = PidOutOfRange;

    /// Takes a 13-bit value
    ///
    /// # Errors
    ///
    /// [`PidOutOfRange`]: `value` is 0x2000 or more.
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        if value <= Self::NULL.0 {
            Ok(Self(value))
        } else {
            Err(PidOutOfRange { value })
        }
    }
}

/// Failure to make a [`Pid`] of a value 13 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PidOutOfRange {
    value: u16,
}

impl PidOutOfRange {
    /// Returns the value given, 0x2000 or more
    #[must_use]
    pub const fn value(self) -> u16 {
        self.value
    }
}

impl fmt::Display for PidOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PID {:#06x} does not fit in 13 bits", self.value)
    }
}

impl core::error::Error for PidOutOfRange {}

/// Count of the packets of one PID that carry a payload, modulo 16
///
/// `continuity_counter`, ISO/IEC 13818-1 §2.4.3.3
///
/// The counter of a packet with no payload (`adaptation_field_control` `'10'`)
/// does not advance. Two consecutive packets of a PID with the same counter
/// are a duplicate, sent at most twice, not a discontinuity. The crate reads
/// and writes the counter; judging the continuity of a PID is the
/// demultiplexer's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContinuityCounter(u8);

impl ContinuityCounter {
    /// Number of distinct counter values, 16
    pub const MODULUS: u8 = 16;

    /// Returns the 4-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Returns the counter of the next packet of the PID, 15 wrapping to 0
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.wrapping_add(1).rem_euclid(Self::MODULUS))
    }
}

impl TryFrom<u8> for ContinuityCounter {
    type Error = ContinuityCounterOutOfRange;

    /// Takes a 4-bit value
    ///
    /// # Errors
    ///
    /// [`ContinuityCounterOutOfRange`]: `value` is 16 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < Self::MODULUS {
            Ok(Self(value))
        } else {
            Err(ContinuityCounterOutOfRange { value })
        }
    }
}

/// Failure to make a [`ContinuityCounter`] of a value 4 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContinuityCounterOutOfRange {
    value: u8,
}

impl ContinuityCounterOutOfRange {
    /// Returns the value given, 16 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

impl fmt::Display for ContinuityCounterOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "continuity_counter {} does not fit in 4 bits",
            self.value
        )
    }
}

impl core::error::Error for ContinuityCounterOutOfRange {}

/// Failure to make a 2-bit field of a value 2 bits cannot carry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TwoBitFieldOutOfRange {
    value: u8,
}

impl TwoBitFieldOutOfRange {
    /// Returns the value given, 4 or more
    #[must_use]
    pub const fn value(self) -> u8 {
        self.value
    }
}

impl fmt::Display for TwoBitFieldOutOfRange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "value {} does not fit in 2 bits", self.value)
    }
}

impl core::error::Error for TwoBitFieldOutOfRange {}

/// How the payload of a Transport Packet is scrambled
///
/// `transport_scrambling_control`, ISO/IEC 13818-1 §2.4.3.3, Table 2-4
///
/// Only `'00'` has a meaning the specification fixes; the other values are
/// user-defined. The header and the adaptation field are never scrambled, so
/// a scrambled packet is read whole and only its payload is opaque. The crate
/// does not descramble.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransportScramblingControl(u8);

impl TransportScramblingControl {
    /// `'00'`, the payload is not scrambled
    pub const NOT_SCRAMBLED: Self = Self(0b00);

    /// Returns the 2-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for TransportScramblingControl {
    type Error = TwoBitFieldOutOfRange;

    /// Takes a 2-bit value
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < 4 {
            Ok(Self(value))
        } else {
            Err(TwoBitFieldOutOfRange { value })
        }
    }
}

/// Whether a Transport Packet holds an adaptation field, a payload, or both
///
/// `adaptation_field_control`, ISO/IEC 13818-1 §2.4.3.3, Table 2-5
///
/// A [`TransportPacket`] states it by which of its adaptation field and
/// payload are present, and reports it with
/// [`adaptation_field_control`](TransportPacket::adaptation_field_control).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AdaptationFieldControl(u8);

impl AdaptationFieldControl {
    /// `'00'`, reserved: a decoder discards the packet
    pub const RESERVED: Self = Self(0b00);
    /// `'01'`, no adaptation field, payload only
    pub const PAYLOAD_ONLY: Self = Self(0b01);
    /// `'10'`, adaptation field only, no payload
    pub const ADAPTATION_FIELD_ONLY: Self = Self(0b10);
    /// `'11'`, adaptation field followed by payload
    pub const ADAPTATION_FIELD_AND_PAYLOAD: Self = Self(0b11);

    /// Returns the 2-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for AdaptationFieldControl {
    type Error = TwoBitFieldOutOfRange;

    /// Takes a 2-bit value
    ///
    /// # Errors
    ///
    /// [`TwoBitFieldOutOfRange`]: `value` is 4 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < 4 {
            Ok(Self(value))
        } else {
            Err(TwoBitFieldOutOfRange { value })
        }
    }
}

/// Transport Packet, the 188-byte unit a Transport Stream is cut into
///
/// `transport_packet`, ISO/IEC 13818-1 §2.4.3.2
///
/// The fields are those of the syntax table; `adaptation_field_control`
/// follows from which of [`adaptation_field`](Self::adaptation_field) and
/// [`payload`](Self::payload) are present. Both borrow the bytes the packet
/// was read from. A packet with `transport_scrambling_control` other than
/// `'00'` is read whole: only its payload is scrambled. The crate does not
/// find packet boundaries in a stream; that is the framer's.
///
/// A packet to write is made with [`new`](Self::new) and its fields set; the
/// adaptation field, with its `stuffing_bytes`, and the payload must add up to
/// the 184 bytes after the header.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::ts::packet::{ContinuityCounter, Error, TransportPacket};
/// use mpeg2_systems::ts::Pid;
///
/// // A null packet
/// let mut bytes = vec![0x47, 0x1f, 0xff, 0x10];
/// bytes.resize(188, 0xff);
/// let packet = TransportPacket::read(&bytes)?;
/// assert_eq!(packet.pid, Pid::NULL);
///
/// // The same packet, made and written (with the `alloc` feature)
/// let mut made = TransportPacket::new(Pid::NULL, ContinuityCounter::try_from(0).unwrap());
/// made.payload = Some(&bytes[4..]);
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// made.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Error>(())
/// ```
#[doc(alias("transport_packet", "TS packet"))]
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TransportPacket<'input> {
    /// Whether the packet holds at least one uncorrectable bit error
    pub transport_error_indicator: bool,
    /// Whether the payload starts a PES packet or a section
    pub payload_unit_start_indicator: bool,
    /// Whether the packet has priority over packets of its PID without the bit
    pub transport_priority: bool,
    /// The stream the packet belongs to
    pub pid: Pid,
    /// How the payload is scrambled
    pub transport_scrambling_control: TransportScramblingControl,
    /// Count of the packets of the PID that carry a payload, modulo 16
    pub continuity_counter: ContinuityCounter,
    /// The adaptation field, when the packet has one
    pub adaptation_field: Option<AdaptationField<'input>>,
    /// The `data_byte`s after the header and the adaptation field, when the
    /// packet has a payload
    pub payload: Option<&'input [u8]>,
}

impl<'input> TransportPacket<'input> {
    /// Creates a packet of `pid` with no flag set, not scrambled, and with
    /// neither adaptation field nor payload yet
    #[must_use]
    pub const fn new(pid: Pid, continuity_counter: ContinuityCounter) -> Self {
        Self {
            transport_error_indicator: false,
            payload_unit_start_indicator: false,
            transport_priority: false,
            pid,
            transport_scrambling_control: TransportScramblingControl::NOT_SCRAMBLED,
            continuity_counter,
            adaptation_field: None,
            payload: None,
        }
    }

    /// Returns the `adaptation_field_control` that the adaptation field and
    /// the payload present add up to
    #[must_use]
    pub const fn adaptation_field_control(&self) -> AdaptationFieldControl {
        match (self.adaptation_field.is_some(), self.payload.is_some()) {
            (false, false) => AdaptationFieldControl::RESERVED,
            (false, true) => AdaptationFieldControl::PAYLOAD_ONLY,
            (true, false) => AdaptationFieldControl::ADAPTATION_FIELD_ONLY,
            (true, true) => AdaptationFieldControl::ADAPTATION_FIELD_AND_PAYLOAD,
        }
    }

    /// Reads a Transport Packet from `bytes`, which hold it whole and nothing more
    ///
    /// # Errors
    ///
    /// * [`PacketLength`](Error::PacketLength): `bytes` are not 188 bytes long.
    /// * [`SyncByte`](Error::SyncByte): the first byte is not 0x47.
    /// * [`ReservedAdaptationFieldControl`](Error::ReservedAdaptationFieldControl):
    ///   `adaptation_field_control` is `'00'`.
    /// * [`AdaptationFieldLength`](Error::AdaptationFieldLength):
    ///   `adaptation_field_length` does not fit `adaptation_field_control`.
    /// * [`AdaptationField`](Error::AdaptationField): the adaptation field
    ///   could not be read.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        if bytes.len() != PACKET_LENGTH {
            return Err(Error::PacketLength {
                length: bytes.len(),
            });
        }

        let mut reader = FieldReader::new(bytes);
        let field = |name: &'static str, error| Error::Field { field: name, error };
        let sync_byte = reader
            .read_u8::<8>()
            .map_err(|error| field("sync_byte", error))?;
        if sync_byte != SYNC_BYTE {
            return Err(Error::SyncByte { sync_byte });
        }
        let transport_error_indicator = reader
            .read_bool()
            .map_err(|error| field("transport_error_indicator", error))?;
        let payload_unit_start_indicator = reader
            .read_bool()
            .map_err(|error| field("payload_unit_start_indicator", error))?;
        let transport_priority = reader
            .read_bool()
            .map_err(|error| field("transport_priority", error))?;
        let pid = Pid(reader
            .read_u16::<13>()
            .map_err(|error| field("PID", error))?);
        let transport_scrambling_control = TransportScramblingControl(
            reader
                .read_u8::<2>()
                .map_err(|error| field("transport_scrambling_control", error))?,
        );
        let adaptation_field_control = AdaptationFieldControl(
            reader
                .read_u8::<2>()
                .map_err(|error| field("adaptation_field_control", error))?,
        );
        let continuity_counter = ContinuityCounter(
            reader
                .read_u8::<4>()
                .map_err(|error| field("continuity_counter", error))?,
        );
        let rest = reader
            .read_bytes(reader.remaining_bytes())
            .map_err(|error| field("data_byte", error))?;

        let (adaptation_field, payload) = match adaptation_field_control {
            AdaptationFieldControl::PAYLOAD_ONLY => (None, Some(rest)),
            AdaptationFieldControl::ADAPTATION_FIELD_ONLY
            | AdaptationFieldControl::ADAPTATION_FIELD_AND_PAYLOAD => {
                let has_payload = adaptation_field_control
                    == AdaptationFieldControl::ADAPTATION_FIELD_AND_PAYLOAD;
                let adaptation_field_length = rest.first().map_or(0, |length| usize::from(*length));
                let fits = if has_payload {
                    adaptation_field_length < MAX_ADAPTATION_FIELD_LENGTH
                } else {
                    adaptation_field_length == MAX_ADAPTATION_FIELD_LENGTH
                };
                let length_error = Error::AdaptationFieldLength {
                    adaptation_field_control,
                    adaptation_field_length,
                };
                if !fits {
                    return Err(length_error);
                }
                let (adaptation_field, payload) = rest
                    .split_at_checked(adaptation_field_length.saturating_add(1))
                    .ok_or(length_error)?;
                (
                    Some(AdaptationField::read(adaptation_field)?),
                    has_payload.then_some(payload),
                )
            }
            _ => return Err(Error::ReservedAdaptationFieldControl),
        };

        Ok(Self {
            transport_error_indicator,
            payload_unit_start_indicator,
            transport_priority,
            pid,
            transport_scrambling_control,
            continuity_counter,
            adaptation_field,
            payload,
        })
    }

    /// Returns the number of bytes [`write`](Self::write) appends, always 188
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        PACKET_LENGTH
    }

    /// Appends the packet to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`NoAdaptationFieldOrPayload`](Error::NoAdaptationFieldOrPayload):
    ///   the packet has neither.
    /// * [`PacketLength`](Error::PacketLength): the header, the adaptation
    ///   field and the payload do not add up to 188 bytes.
    /// * [`AdaptationFieldLength`](Error::AdaptationFieldLength): the
    ///   adaptation field fills the packet, leaving the payload present but
    ///   empty.
    /// * [`AdaptationField`](Error::AdaptationField): the adaptation field
    ///   could not be written.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let adaptation_field_control = self.adaptation_field_control();
        if adaptation_field_control == AdaptationFieldControl::RESERVED {
            return Err(Error::NoAdaptationFieldOrPayload);
        }
        let adaptation_field_length = self
            .adaptation_field
            .as_ref()
            .map_or(0, AdaptationField::encoded_len);
        let payload_length = self.payload.map_or(0, <[u8]>::len);
        let length = HEADER_LENGTH
            .saturating_add(adaptation_field_length)
            .saturating_add(payload_length);
        if length != PACKET_LENGTH {
            return Err(Error::PacketLength { length });
        }
        if adaptation_field_control == AdaptationFieldControl::ADAPTATION_FIELD_AND_PAYLOAD
            && payload_length == 0
        {
            return Err(Error::AdaptationFieldLength {
                adaptation_field_control,
                adaptation_field_length: MAX_ADAPTATION_FIELD_LENGTH,
            });
        }

        let start = out.len();
        let written = self.write_fields(out, adaptation_field_control);
        if written.is_err() {
            out.truncate(start);
        }

        written
    }

    /// Appends the header, the adaptation field and the payload, leaving
    /// whatever it appended on failure
    #[cfg(feature = "alloc")]
    fn write_fields(
        &self,
        out: &mut Vec<u8>,
        adaptation_field_control: AdaptationFieldControl,
    ) -> Result<(), Error> {
        let mut writer = FieldWriter::new(out);
        // Why not an error naming each field: every value is a newtype of its
        // field's width, so none of these writes can fail.
        let header = (|| {
            writer.write_bits::<8>(SYNC_BYTE)?;
            writer.write_bool(self.transport_error_indicator)?;
            writer.write_bool(self.payload_unit_start_indicator)?;
            writer.write_bool(self.transport_priority)?;
            writer.write_bits::<13>(self.pid.get())?;
            writer.write_bits::<2>(self.transport_scrambling_control.get())?;
            writer.write_bits::<2>(adaptation_field_control.get())?;
            writer.write_bits::<4>(self.continuity_counter.get())?;
            writer.finish()
        })();
        header.map_err(|error| Error::Field {
            field: "transport_packet header",
            error,
        })?;

        if let Some(adaptation_field) = &self.adaptation_field {
            adaptation_field.write(out)?;
        }
        if let Some(payload) = self.payload {
            out.extend_from_slice(payload);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{
        AdaptationFieldControl, ContinuityCounter, ContinuityCounterOutOfRange, Error, Pid,
        PidOutOfRange, TransportPacket, TransportScramblingControl,
    };
    use crate::time::ClockReference;
    use crate::ts::adaptation_field::AdaptationField;

    /// Returns a 188-byte packet that starts with `head` and is 0xFF after it
    fn packet(head: &[u8]) -> [u8; 188] {
        let mut bytes = [0xff; 188];
        for (byte, value) in bytes.iter_mut().zip(head) {
            *byte = *value;
        }
        bytes
    }

    #[test]
    fn a_packet_that_does_not_start_with_the_sync_byte_is_refused() {
        assert_eq!(
            TransportPacket::read(&packet(&[0x48, 0x1f, 0xff, 0x10])),
            Err(Error::SyncByte { sync_byte: 0x48 })
        );
    }

    #[test]
    fn bytes_other_than_188_are_not_a_packet() {
        assert_eq!(
            TransportPacket::read(&[0x47; 187]),
            Err(Error::PacketLength { length: 187 })
        );
    }

    #[test]
    fn a_packet_with_the_reserved_adaptation_field_control_is_refused() {
        assert_eq!(
            TransportPacket::read(&packet(&[0x47, 0x1f, 0xff, 0x00])),
            Err(Error::ReservedAdaptationFieldControl)
        );
    }

    #[test]
    fn an_adaptation_field_length_that_does_not_fit_the_control_is_refused() {
        assert_eq!(
            [
                TransportPacket::read(&packet(&[0x47, 0x01, 0x00, 0x20, 182])),
                TransportPacket::read(&packet(&[0x47, 0x01, 0x00, 0x30, 183])),
            ],
            [
                Err(Error::AdaptationFieldLength {
                    adaptation_field_control: AdaptationFieldControl::ADAPTATION_FIELD_ONLY,
                    adaptation_field_length: 182
                }),
                Err(Error::AdaptationFieldLength {
                    adaptation_field_control: AdaptationFieldControl::ADAPTATION_FIELD_AND_PAYLOAD,
                    adaptation_field_length: 183
                }),
            ]
        );
    }

    #[test]
    fn a_scrambled_packet_has_its_header_and_adaptation_field_read() {
        let bytes = packet(&[
            0x47, 0x41, 0x00, 0xb7, 0x07, 0x10, 0x00, 0x00, 0x00, 0x01, 0x7e, 0x64,
        ]);
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.program_clock_reference = Some(ClockReference::new(2, 100).unwrap());

        assert_eq!(
            TransportPacket::read(&bytes),
            Ok(TransportPacket {
                transport_error_indicator: false,
                payload_unit_start_indicator: true,
                transport_priority: false,
                pid: Pid::try_from(0x0100).unwrap(),
                transport_scrambling_control: TransportScramblingControl::try_from(0b10).unwrap(),
                continuity_counter: ContinuityCounter::try_from(7).unwrap(),
                adaptation_field: Some(adaptation_field),
                payload: bytes.get(12..),
            })
        );
    }

    #[test]
    fn a_pid_takes_any_13_bit_value_and_refuses_a_wider_one() {
        assert_eq!(
            [Pid::try_from(0x1fff), Pid::try_from(0x2000)],
            [Ok(Pid::NULL), Err(PidOutOfRange { value: 0x2000 })]
        );
    }

    #[test]
    fn a_continuity_counter_takes_any_4_bit_value_and_wraps_from_15_to_0() {
        let counter = |value| ContinuityCounter::try_from(value).unwrap();

        assert_eq!(
            (
                ContinuityCounter::try_from(16),
                counter(3).next(),
                counter(15).next()
            ),
            (
                Err(ContinuityCounterOutOfRange { value: 16 }),
                counter(4),
                counter(0)
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_packet_that_does_not_add_up_to_188_bytes_is_refused_before_a_byte_is_written() {
        let payload = [0; 183];
        let mut short = TransportPacket::new(Pid::NULL, ContinuityCounter::try_from(0).unwrap());
        short.payload = Some(&payload);
        let empty = TransportPacket::new(Pid::NULL, ContinuityCounter::try_from(0).unwrap());
        let mut written = Vec::new();

        assert_eq!(
            [short.write(&mut written), empty.write(&mut written)],
            [
                Err(Error::PacketLength { length: 187 }),
                Err(Error::NoAdaptationFieldOrPayload)
            ]
        );
        assert_eq!(written, []);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn an_adaptation_field_that_leaves_an_empty_payload_is_refused() {
        let mut adaptation_field = AdaptationField::new();
        adaptation_field.stuffing_bytes = 183;
        let mut packet = TransportPacket::new(Pid::NULL, ContinuityCounter::try_from(0).unwrap());
        packet.adaptation_field = Some(adaptation_field);
        packet.payload = Some(&[]);

        assert_eq!(
            packet.write(&mut Vec::new()),
            Err(Error::AdaptationFieldLength {
                adaptation_field_control: AdaptationFieldControl::ADAPTATION_FIELD_AND_PAYLOAD,
                adaptation_field_length: 183
            })
        );
    }
}
