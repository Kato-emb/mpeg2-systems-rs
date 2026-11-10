//! Hierarchy descriptor, which places a program element among the layers of
//! a hierarchically coded stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, field_error, fixed_body};
use crate::field::{FieldReader, FourBitFieldOutOfRange};

/// Bytes of the body
const BODY_LENGTH: usize = 4;

/// Relation between a hierarchy layer and the layer it is embedded in
///
/// `hierarchy_type`, ISO/IEC 13818-1 §2.6.7, Table 2-50
///
/// 0 and 11 to 14 are reserved.
#[doc(alias = "hierarchy_type")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HierarchyType(u8);

impl HierarchyType {
    /// Spatial Scalability
    pub const SPATIAL_SCALABILITY: Self = Self(1);
    /// SNR Scalability
    pub const SNR_SCALABILITY: Self = Self(2);
    /// Temporal Scalability
    pub const TEMPORAL_SCALABILITY: Self = Self(3);
    /// Data partitioning
    pub const DATA_PARTITIONING: Self = Self(4);
    /// Extension bitstream
    pub const EXTENSION_BITSTREAM: Self = Self(5);
    /// Private Stream
    pub const PRIVATE_STREAM: Self = Self(6);
    /// Multi-view Profile
    pub const MULTI_VIEW_PROFILE: Self = Self(7);
    /// Combined Scalability or MV-HEVC sub-partition
    pub const COMBINED_SCALABILITY: Self = Self(8);
    /// MVC video sub-bitstream or MVCD video sub-bitstream
    pub const MVC_VIDEO_SUB_BITSTREAM: Self = Self(9);
    /// Auxiliary picture layer, Rec. ITU-T H.265 | ISO/IEC 23008-2 Annex F
    pub const AUXILIARY_PICTURE_LAYER: Self = Self(10);
    /// Base layer, or one of the base sub-bitstreams Table 2-50 lists
    pub const BASE_LAYER: Self = Self(15);

    /// Returns the 4-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for HierarchyType {
    type Error = FourBitFieldOutOfRange;

    /// Takes a 4-bit value
    ///
    /// # Errors
    ///
    /// [`FourBitFieldOutOfRange`]: `value` is 16 or more.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        FourBitFieldOutOfRange::check(value).map(Self)
    }
}

/// Hierarchy descriptor, which places a program element among the layers of
/// a hierarchically coded stream
///
/// `hierarchy_descriptor`, ISO/IEC 13818-1 §2.6.6, Table 2-49, semantics
/// §2.6.7
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::hierarchy::{HierarchyDescriptor, HierarchyType};
///
/// // A temporal enhancement of layer 0 as layer 1
/// let bytes = [0x04, 0x04, 0xb3, 0xc1, 0x40, 0xc1];
/// let descriptor = HierarchyDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(
///     descriptor,
///     HierarchyDescriptor::new(HierarchyType::TEMPORAL_SCALABILITY, 1, 0, 1)
///         .with_no_temporal_scalability_flag(false)
/// );
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "hierarchy_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HierarchyDescriptor {
    /// `'0'` when the element adds views to the layer it is embedded in
    pub no_view_scalability_flag: bool,
    /// `'0'` when the element raises the frame rate of that layer
    pub no_temporal_scalability_flag: bool,
    /// `'0'` when the element raises the spatial resolution of that layer
    pub no_spatial_scalability_flag: bool,
    /// `'0'` when the element raises the SNR quality of that layer
    pub no_quality_scalability_flag: bool,
    /// Relation between this layer and the layer it is embedded in
    pub hierarchy_type: HierarchyType,
    /// Index of the element among the layers of the program, 6 bits
    pub hierarchy_layer_index: u8,
    /// `'0'` when the PES packet headers of the element may carry `TREF`
    pub tref_present_flag: bool,
    /// Index of the layer decoded before this one, 6 bits
    pub hierarchy_embedded_layer_index: u8,
    /// Transmission channel the element is meant for, 6 bits
    pub hierarchy_channel: u8,
}

impl HierarchyDescriptor {
    /// Creates the descriptor of a layer, every `no_*_scalability_flag` `'1'`
    /// and `tref_present_flag` `'0'`
    #[must_use]
    pub const fn new(
        hierarchy_type: HierarchyType,
        hierarchy_layer_index: u8,
        hierarchy_embedded_layer_index: u8,
        hierarchy_channel: u8,
    ) -> Self {
        Self {
            no_view_scalability_flag: true,
            no_temporal_scalability_flag: true,
            no_spatial_scalability_flag: true,
            no_quality_scalability_flag: true,
            hierarchy_type,
            hierarchy_layer_index,
            tref_present_flag: false,
            hierarchy_embedded_layer_index,
            hierarchy_channel,
        }
    }

    /// Returns the value with the given `no_view_scalability_flag`
    #[must_use]
    pub const fn with_no_view_scalability_flag(mut self, no_view_scalability_flag: bool) -> Self {
        self.no_view_scalability_flag = no_view_scalability_flag;
        self
    }

    /// Returns the value with the given `no_temporal_scalability_flag`
    #[must_use]
    pub const fn with_no_temporal_scalability_flag(
        mut self,
        no_temporal_scalability_flag: bool,
    ) -> Self {
        self.no_temporal_scalability_flag = no_temporal_scalability_flag;
        self
    }

    /// Returns the value with the given `no_spatial_scalability_flag`
    #[must_use]
    pub const fn with_no_spatial_scalability_flag(
        mut self,
        no_spatial_scalability_flag: bool,
    ) -> Self {
        self.no_spatial_scalability_flag = no_spatial_scalability_flag;
        self
    }

    /// Returns the value with the given `no_quality_scalability_flag`
    #[must_use]
    pub const fn with_no_quality_scalability_flag(
        mut self,
        no_quality_scalability_flag: bool,
    ) -> Self {
        self.no_quality_scalability_flag = no_quality_scalability_flag;
        self
    }

    /// Returns the value with the given `hierarchy_type`
    #[must_use]
    pub const fn with_hierarchy_type(mut self, hierarchy_type: HierarchyType) -> Self {
        self.hierarchy_type = hierarchy_type;
        self
    }

    /// Returns the value with the given `hierarchy_layer_index`
    #[must_use]
    pub const fn with_hierarchy_layer_index(mut self, hierarchy_layer_index: u8) -> Self {
        self.hierarchy_layer_index = hierarchy_layer_index;
        self
    }

    /// Returns the value with the given `tref_present_flag`
    #[must_use]
    pub const fn with_tref_present_flag(mut self, tref_present_flag: bool) -> Self {
        self.tref_present_flag = tref_present_flag;
        self
    }

    /// Returns the value with the given `hierarchy_embedded_layer_index`
    #[must_use]
    pub const fn with_hierarchy_embedded_layer_index(
        mut self,
        hierarchy_embedded_layer_index: u8,
    ) -> Self {
        self.hierarchy_embedded_layer_index = hierarchy_embedded_layer_index;
        self
    }

    /// Returns the value with the given `hierarchy_channel`
    #[must_use]
    pub const fn with_hierarchy_channel(mut self, hierarchy_channel: u8) -> Self {
        self.hierarchy_channel = hierarchy_channel;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH.saturating_add(BODY_LENGTH)
    }

    /// Appends the descriptor to `out`, its `reserved` bits all `'1'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `hierarchy_type` is 0 or 11 to 14,
    ///   reserved; it is 9, an MVC or MVCD video sub-bitstream, with a
    ///   `no_*_scalability_flag` other than `'1'`, which §2.6.7 requires; it
    ///   is other than 8, Combined Scalability, with two or more
    ///   `no_*_scalability_flag`s `'0'`, scalability in more than one
    ///   dimension, for which §2.6.7 requires 8; or `tref_present_flag` is
    ///   `'1'`, reserved.
    /// * [`Field`](Error::Field): an index or `hierarchy_channel` wider than
    ///   6 bits.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let hierarchy_type = self.hierarchy_type.get();
        if matches!(hierarchy_type, 0 | 11..=14) {
            return Err(Error::Forbidden {
                field: "hierarchy_type",
                value: u32::from(hierarchy_type),
            });
        }
        let flags = [
            ("no_view_scalability_flag", self.no_view_scalability_flag),
            (
                "no_temporal_scalability_flag",
                self.no_temporal_scalability_flag,
            ),
            (
                "no_spatial_scalability_flag",
                self.no_spatial_scalability_flag,
            ),
            (
                "no_quality_scalability_flag",
                self.no_quality_scalability_flag,
            ),
        ];
        if self.hierarchy_type == HierarchyType::MVC_VIDEO_SUB_BITSTREAM
            && let Some((field, _)) = flags.iter().find(|(_, flag)| !flag)
        {
            return Err(Error::Forbidden { field, value: 0 });
        }
        let dimensions = flags.iter().filter(|(_, flag)| !flag).count();
        if dimensions > 1 && self.hierarchy_type != HierarchyType::COMBINED_SCALABILITY {
            return Err(Error::Forbidden {
                field: "hierarchy_type",
                value: u32::from(hierarchy_type),
            });
        }
        if self.tref_present_flag {
            return Err(Error::Forbidden {
                field: "tref_present_flag",
                value: 1,
            });
        }

        write_descriptor(out, DescriptorTag::HIERARCHY, BODY_LENGTH, |writer| {
            (|| {
                for (_, flag) in flags {
                    writer.write_bool(flag)?;
                }
                writer.write_bits::<4>(hierarchy_type)?;
                writer.write_bits::<2>(0b11_u8)
            })()
            // Why not an error per field: the flags, the type and the
            // reserved bits fit their fields' widths.
            .map_err(field_error("descriptor body"))?;
            writer
                .write_bits::<6>(self.hierarchy_layer_index)
                .map_err(field_error("hierarchy_layer_index"))?;
            writer
                .write_bool(self.tref_present_flag)
                .and_then(|()| writer.write_bool(true))
                .map_err(field_error("tref_present_flag"))?;
            writer
                .write_bits::<6>(self.hierarchy_embedded_layer_index)
                .map_err(field_error("hierarchy_embedded_layer_index"))?;
            writer
                .write_bits::<2>(0b11_u8)
                .map_err(field_error("reserved"))?;
            writer
                .write_bits::<6>(self.hierarchy_channel)
                .map_err(field_error("hierarchy_channel"))
        })
    }
}

impl TryFrom<&RawDescriptor<'_>> for HierarchyDescriptor {
    type Error = Error;

    /// Takes the fields of a hierarchy descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x04.
    /// * [`BodyLength`](Error::BodyLength): the body is not 4 bytes.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let body = fixed_body::<BODY_LENGTH>(raw, DescriptorTag::HIERARCHY)?;
        let mut reader = FieldReader::new(body);
        // Why not an error per field: the body is as long as the fields.
        (|| {
            let no_view_scalability_flag = reader.read_bool()?;
            let no_temporal_scalability_flag = reader.read_bool()?;
            let no_spatial_scalability_flag = reader.read_bool()?;
            let no_quality_scalability_flag = reader.read_bool()?;
            let hierarchy_type = HierarchyType(reader.read_u8::<4>()?);
            reader.read_bits::<2>()?;
            let hierarchy_layer_index = reader.read_u8::<6>()?;
            let tref_present_flag = reader.read_bool()?;
            reader.read_bool()?;
            let hierarchy_embedded_layer_index = reader.read_u8::<6>()?;
            reader.read_bits::<2>()?;
            Ok(Self {
                no_view_scalability_flag,
                no_temporal_scalability_flag,
                no_spatial_scalability_flag,
                no_quality_scalability_flag,
                hierarchy_type,
                hierarchy_layer_index,
                tref_present_flag,
                hierarchy_embedded_layer_index,
                hierarchy_channel: reader.read_u8::<6>()?,
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::{HierarchyDescriptor, HierarchyType};
    #[cfg(feature = "alloc")]
    use crate::descriptor::Error;
    use crate::descriptor::RawDescriptor;
    #[cfg(feature = "alloc")]
    use crate::field;

    /// An SNR enhancement of layer 4 as layer 5, on channel 63, every
    /// `reserved` bit `'1'`
    const SNR: [u8; 6] = [0x04, 0x04, 0xd2, 0xc5, 0x44, 0xff];

    fn snr() -> HierarchyDescriptor {
        let mut descriptor = HierarchyDescriptor::new(HierarchyType::SNR_SCALABILITY, 5, 4, 63);
        descriptor.no_spatial_scalability_flag = false;
        descriptor
    }

    #[test]
    fn the_fields_are_read_whatever_the_reserved_bits() {
        let reserved_0 = [0x04, 0x04, 0xd2, 0x05, 0x04, 0x3f];

        assert_eq!(
            [SNR, reserved_0]
                .map(|bytes| HierarchyDescriptor::try_from(&RawDescriptor::read(&bytes).unwrap())),
            [Ok(snr()), Ok(snr())]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_is_written_as_laid_out_by_hand() {
        let mut written = Vec::new();
        snr().write(&mut written).unwrap();

        assert_eq!(written, SNR);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn descriptors_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let reserved = |value| {
            let mut descriptor = snr();
            descriptor.hierarchy_type = HierarchyType::try_from(value).unwrap();
            descriptor
        };
        let mut mvc = snr();
        mvc.hierarchy_type = HierarchyType::MVC_VIDEO_SUB_BITSTREAM;
        let combined = HierarchyDescriptor::try_from(
            &RawDescriptor::read(&[0x04, 0x04, 0x03, 0xc1, 0x40, 0xc1]).unwrap(),
        )
        .unwrap();
        let mut tref = snr();
        tref.tref_present_flag = true;
        let mut wide_index = snr();
        wide_index.hierarchy_layer_index = 64;
        let mut written = Vec::new();

        assert_eq!(
            [reserved(0), reserved(11), mvc, combined, tref, wide_index]
                .map(|descriptor| descriptor.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "hierarchy_type",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "hierarchy_type",
                    value: 11
                }),
                Err(Error::Forbidden {
                    field: "no_spatial_scalability_flag",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "hierarchy_type",
                    value: 3
                }),
                Err(Error::Forbidden {
                    field: "tref_present_flag",
                    value: 1
                }),
                Err(Error::Field {
                    field: "hierarchy_layer_index",
                    error: field::Error::ValueTooWide {
                        width: 6,
                        value: 64
                    }
                }),
            ]
        );
        assert_eq!(written, []);
    }
}
