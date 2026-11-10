//! What the caller of a [`Muxer`](super::Muxer) sets on the Transport
//! Packets that carry a PES packet

use super::Error;
use crate::time::ClockReference;
use crate::ts::adaptation_field::AdaptationField;

/// Bytes the adaptation field of a Transport Packet with a payload takes at
/// most: `adaptation_field_length` up to 182 (ISO/IEC 13818-1 §2.4.3.5), and
/// the byte of `adaptation_field_length` itself
const LONGEST_ADAPTATION_FIELD_WITH_PAYLOAD: usize = 183;

/// How a [`Muxer`](super::Muxer) carries a PES packet in Transport Packets,
/// beyond what it decides itself
///
/// `transport_priority`, ISO/IEC 13818-1 §2.4.3.3, and the fields of
/// `adaptation_field`, §2.4.3.5, of the Transport Packets of the PES packet
///
/// The muxer sets the PCR, `discontinuity_indicator` and the stuffing bytes
/// itself; nothing here overrides them. It does not read the elementary
/// stream, so whether the PES packet holds an elementary stream access
/// point, as `random_access_indicator` promises, and whether the payload of
/// its first Transport Packet meets the condition of
/// `elementary_stream_priority_indicator`, are the caller's to keep.
///
/// ```
/// use mpeg2_systems::ts::mux::PesPacketCarriage;
///
/// // An access point, its Transport Packets of the higher priority
/// let carriage = PesPacketCarriage::new()
///     .with_random_access_indicator(true)
///     .with_transport_priority(true);
/// # let _ = carriage;
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PesPacketCarriage<'a> {
    /// `random_access_indicator` of the first Transport Packet
    pub random_access_indicator: bool,
    /// `elementary_stream_priority_indicator` of the first Transport Packet
    pub elementary_stream_priority_indicator: bool,
    /// `private_data_byte`s of the first Transport Packet, without the
    /// length field before them
    pub transport_private_data: Option<&'a [u8]>,
    /// `transport_priority` of every Transport Packet of the PES packet
    pub transport_priority: bool,
}

impl<'a> PesPacketCarriage<'a> {
    /// Returns the carriage with no field set: the PES packet is carried as
    /// [`Muxer::handle_pes_packet`](super::Muxer::handle_pes_packet) carries it
    #[expect(
        clippy::new_without_default,
        reason = "the specification sets no default, which Default would claim"
    )]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            random_access_indicator: false,
            elementary_stream_priority_indicator: false,
            transport_private_data: None,
            transport_priority: false,
        }
    }

    /// Returns the value with the given `random_access_indicator`
    #[must_use]
    pub const fn with_random_access_indicator(mut self, random_access_indicator: bool) -> Self {
        self.random_access_indicator = random_access_indicator;
        self
    }

    /// Returns the value with the given `elementary_stream_priority_indicator`
    #[must_use]
    pub const fn with_elementary_stream_priority_indicator(
        mut self,
        elementary_stream_priority_indicator: bool,
    ) -> Self {
        self.elementary_stream_priority_indicator = elementary_stream_priority_indicator;
        self
    }

    /// Returns the value with `transport_private_data` present, set to the
    /// one given
    #[must_use]
    pub const fn with_transport_private_data(mut self, transport_private_data: &'a [u8]) -> Self {
        self.transport_private_data = Some(transport_private_data);
        self
    }

    /// Returns the value with the given `transport_priority`
    #[must_use]
    pub const fn with_transport_priority(mut self, transport_priority: bool) -> Self {
        self.transport_priority = transport_priority;
        self
    }

    /// Returns whether the PCR goes in the adaptation field of the first
    /// Transport Packet, on the PCR PID when `on_pcr_pid`, a PCR being due at
    /// the packet when `pcr_due`
    ///
    /// `random_access_indicator` on the PCR PID takes a PCR beside it
    /// (§2.4.3.5). A PCR due goes beside `transport_private_data` where it
    /// fits, and in a Transport Packet of its own otherwise.
    ///
    /// # Errors
    ///
    /// * [`TransportPrivateDataLength`](Error::TransportPrivateDataLength):
    ///   `transport_private_data` is longer than the adaptation field holds,
    ///   beside the PCR that random access on the PCR PID takes.
    pub(super) fn pcr_in_first_packet(
        &self,
        on_pcr_pid: bool,
        pcr_due: bool,
    ) -> Result<bool, Error> {
        let length = self.transport_private_data.map_or(0, <[u8]>::len);
        let random_access_pcr = on_pcr_pid && self.random_access_indicator;
        let limit = transport_private_data_limit(random_access_pcr);
        if length > limit {
            return Err(Error::TransportPrivateDataLength { length, limit });
        }
        Ok(random_access_pcr
            || (on_pcr_pid && pcr_due && length <= transport_private_data_limit(true)))
    }

    /// Returns the adaptation field of the first Transport Packet, with
    /// `program_clock_reference` beside the fields set, or `None` when it
    /// would carry nothing
    pub(super) fn adaptation_field(
        &self,
        program_clock_reference: Option<ClockReference>,
    ) -> Option<AdaptationField<'a>> {
        let field = AdaptationField {
            random_access_indicator: self.random_access_indicator,
            elementary_stream_priority_indicator: self.elementary_stream_priority_indicator,
            program_clock_reference,
            transport_private_data: self.transport_private_data,
            ..AdaptationField::new()
        };
        (field != AdaptationField::new()).then_some(field)
    }
}

/// Returns the most bytes of `transport_private_data` the adaptation field
/// of a Transport Packet with a payload holds, with a PCR beside them when
/// `beside_pcr`
fn transport_private_data_limit(beside_pcr: bool) -> usize {
    let field = AdaptationField {
        program_clock_reference: beside_pcr.then(|| ClockReference::from_fields(0, 0)),
        transport_private_data: Some(&[]),
        ..AdaptationField::new()
    };
    LONGEST_ADAPTATION_FIELD_WITH_PAYLOAD.saturating_sub(field.encoded_len())
}
