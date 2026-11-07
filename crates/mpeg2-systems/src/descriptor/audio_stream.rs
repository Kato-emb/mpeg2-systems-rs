//! Audio stream descriptor, which identifies the coding version of an
//! ISO/IEC 11172-3 or ISO/IEC 13818-3 audio stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

use super::{DescriptorTag, Error, RawDescriptor, fixed_body};
#[cfg(feature = "alloc")]
use super::{field_error, write_descriptor};
use crate::field::{FieldReader, TwoBitFieldOutOfRange};

/// Bytes of the body
const BODY_LENGTH: usize = 1;

/// Layer of an ISO/IEC 11172-3 or ISO/IEC 13818-3 audio stream
///
/// `layer`, coded as in ISO/IEC 13818-3 §2.4.2.3
///
/// `'00'` is reserved.
#[doc(alias = "layer")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Layer(u8);

impl Layer {
    /// Layer III
    pub const LAYER_III: Self = Self(0b01);
    /// Layer II
    pub const LAYER_II: Self = Self(0b10);
    /// Layer I
    pub const LAYER_I: Self = Self(0b11);

    /// Returns the 2-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Layer {
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

/// Audio stream descriptor, which identifies the coding version of an
/// ISO/IEC 11172-3 or ISO/IEC 13818-3 audio stream
///
/// `audio_stream_descriptor`, ISO/IEC 13818-1 §2.6.4, Table 2-48, semantics
/// §2.6.5
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::audio_stream::{AudioStreamDescriptor, Layer};
///
/// // Layer II with the ID bit set, of a constant bit rate
/// let bytes = [0x03, 0x01, 0x67];
/// let descriptor = AudioStreamDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.layer, Layer::LAYER_II);
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "audio_stream_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioStreamDescriptor {
    /// Whether an audio frame may have `bitrate_index` `'0000'`
    pub free_format_flag: bool,
    /// Whether every audio frame has its `ID` bit set
    pub id: bool,
    /// The highest layer of any audio frame, or a higher one
    pub layer: Layer,
    /// Whether the bit rate may change between consecutive audio frames
    pub variable_rate_audio_indicator: bool,
}

impl AudioStreamDescriptor {
    /// Creates the descriptor of a stream of `layer`, every flag clear
    #[must_use]
    pub const fn new(layer: Layer) -> Self {
        Self {
            free_format_flag: false,
            id: false,
            layer,
            variable_rate_audio_indicator: false,
        }
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH.saturating_add(BODY_LENGTH)
    }

    /// Appends the descriptor to `out`, its `reserved` bits `'111'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Forbidden`](Error::Forbidden): `layer` is `'00'`, reserved.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let layer = self.layer.get();
        if layer == 0 {
            return Err(Error::Forbidden {
                field: "layer",
                value: 0,
            });
        }

        write_descriptor(out, DescriptorTag::AUDIO_STREAM, BODY_LENGTH, |writer| {
            (|| {
                writer.write_bool(self.free_format_flag)?;
                writer.write_bool(self.id)?;
                writer.write_bits::<2>(layer)?;
                writer.write_bool(self.variable_rate_audio_indicator)?;
                writer.write_bits::<3>(0b111_u8)
            })()
            // Why not an error per field: every value fits its field's width.
            .map_err(field_error("descriptor body"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for AudioStreamDescriptor {
    type Error = Error;

    /// Takes the fields of an audio stream descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x03.
    /// * [`BodyLength`](Error::BodyLength): the body is not 1 byte.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::AUDIO_STREAM)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            Ok(Self {
                free_format_flag: reader.read_bool()?,
                id: reader.read_bool()?,
                layer: Layer(reader.read_u8::<2>()?),
                variable_rate_audio_indicator: reader.read_bool()?,
            })
        })()
        .map_err(|error| Error::Field {
            field: "descriptor body",
            error,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::AudioStreamDescriptor;
    #[cfg(feature = "alloc")]
    use super::Layer;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[cfg(feature = "alloc")]
    #[test]
    fn reserved_bits_read_as_0_are_written_as_1() {
        let descriptor =
            AudioStreamDescriptor::try_from(&RawDescriptor::read(&[0x03, 0x01, 0xb8]).unwrap());
        let mut expected = AudioStreamDescriptor::new(Layer::LAYER_I);
        expected.free_format_flag = true;
        expected.variable_rate_audio_indicator = true;
        let mut written = Vec::new();
        expected.write(&mut written).unwrap();

        assert_eq!(
            (descriptor, written.as_slice()),
            (Ok(expected), [0x03, 0x01, 0xbf].as_slice())
        );
    }

    #[test]
    fn a_body_of_other_than_1_byte_is_refused() {
        assert_eq!(
            AudioStreamDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::AUDIO_STREAM,
                &[0x67, 0x00]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::AUDIO_STREAM,
                length: 2
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_reserved_layer_is_refused_before_a_byte_is_written() {
        let mut written = Vec::new();

        assert_eq!(
            AudioStreamDescriptor::new(Layer::try_from(0).unwrap()).write(&mut written),
            Err(Error::Forbidden {
                field: "layer",
                value: 0
            })
        );
        assert_eq!(written, []);
    }
}
