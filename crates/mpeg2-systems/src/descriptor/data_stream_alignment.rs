//! Data stream alignment descriptor, which names the unit a PES packet of
//! the stream starts with when `data_alignment_indicator` is set

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, fixed_body};

/// Bytes of the body
const BODY_LENGTH: usize = 1;

/// Unit a PES packet starts with when `data_alignment_indicator` is set
///
/// `alignment_type`, ISO/IEC 13818-1 §2.6.11, Tables 2-53 to 2-58
///
/// The meaning of a value depends on the stream type: each of Tables 2-53
/// to 2-58 assigns the values from 1 for one kind of stream, so the
/// constants are named after their table. 0 and 16 to 255 are reserved in
/// every table.
#[doc(alias = "alignment_type")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AlignmentType(u8);

impl AlignmentType {
    /// Slice, or video access unit (Table 2-53)
    pub const VIDEO_SLICE_OR_ACCESS_UNIT: Self = Self(1);
    /// Video access unit (Table 2-53)
    pub const VIDEO_ACCESS_UNIT: Self = Self(2);
    /// GOP, or SEQ (Table 2-53)
    pub const VIDEO_GOP_OR_SEQ: Self = Self(3);
    /// SEQ (Table 2-53)
    pub const VIDEO_SEQ: Self = Self(4);
    /// AVC slice or AVC access unit (Table 2-54)
    pub const AVC_SLICE_OR_ACCESS_UNIT: Self = Self(1);
    /// AVC access unit (Table 2-54)
    pub const AVC_ACCESS_UNIT: Self = Self(2);
    /// SVC slice or SVC dependency representation (Table 2-54)
    pub const SVC_SLICE_OR_DEPENDENCY_REPRESENTATION: Self = Self(3);
    /// SVC dependency representation (Table 2-54)
    pub const SVC_DEPENDENCY_REPRESENTATION: Self = Self(4);
    /// MVC slice or MVC view-component subset (Table 2-54)
    pub const MVC_SLICE_OR_VIEW_COMPONENT_SUBSET: Self = Self(5);
    /// MVC view-component subset (Table 2-54)
    pub const MVC_VIEW_COMPONENT_SUBSET: Self = Self(6);
    /// MVCD slice or MVCD view-component subset (Table 2-54)
    pub const MVCD_SLICE_OR_VIEW_COMPONENT_SUBSET: Self = Self(7);
    /// MVCD view-component subset (Table 2-54)
    pub const MVCD_VIEW_COMPONENT_SUBSET: Self = Self(8);
    /// HEVC access unit (Table 2-55)
    pub const HEVC_ACCESS_UNIT: Self = Self(1);
    /// HEVC slice (Table 2-55)
    pub const HEVC_SLICE: Self = Self(2);
    /// HEVC access unit or slice (Table 2-55)
    pub const HEVC_ACCESS_UNIT_OR_SLICE: Self = Self(3);
    /// HEVC tile of slices (Table 2-55)
    pub const HEVC_TILE_OF_SLICES: Self = Self(4);
    /// HEVC access unit or tile of slices (Table 2-55)
    pub const HEVC_ACCESS_UNIT_OR_TILE_OF_SLICES: Self = Self(5);
    /// HEVC slice or tile of slices (Table 2-55)
    pub const HEVC_SLICE_OR_TILE_OF_SLICES: Self = Self(6);
    /// HEVC access unit or slice or tile of slices (Table 2-55)
    pub const HEVC_ACCESS_UNIT_OR_SLICE_OR_TILE_OF_SLICES: Self = Self(7);
    /// HEVC slice segment (Table 2-55)
    pub const HEVC_SLICE_SEGMENT: Self = Self(8);
    /// HEVC slice segment or access unit (Table 2-55)
    pub const HEVC_SLICE_SEGMENT_OR_ACCESS_UNIT: Self = Self(9);
    /// HEVC slice segment or slice (Table 2-55)
    pub const HEVC_SLICE_SEGMENT_OR_SLICE: Self = Self(10);
    /// HEVC slice segment or access unit or slice (Table 2-55)
    pub const HEVC_SLICE_SEGMENT_OR_ACCESS_UNIT_OR_SLICE: Self = Self(11);
    /// HEVC slice segment or tile of slices (Table 2-55)
    pub const HEVC_SLICE_SEGMENT_OR_TILE_OF_SLICES: Self = Self(12);
    /// HEVC slice segment or access unit or tile of slices (Table 2-55)
    pub const HEVC_SLICE_SEGMENT_OR_ACCESS_UNIT_OR_TILE_OF_SLICES: Self = Self(13);
    /// HEVC slice segment or slice or tile of slices (Table 2-55)
    pub const HEVC_SLICE_SEGMENT_OR_SLICE_OR_TILE_OF_SLICES: Self = Self(14);
    /// HEVC slice segment or access unit or slice or tile of slices (Table 2-55)
    pub const HEVC_SLICE_SEGMENT_OR_ACCESS_UNIT_OR_SLICE_OR_TILE_OF_SLICES: Self = Self(15);
    /// Sync word (Table 2-56)
    pub const AUDIO_SYNC_WORD: Self = Self(1);
    /// VVC access unit (Table 2-57)
    pub const VVC_ACCESS_UNIT: Self = Self(1);
    /// VVC slice (Table 2-57)
    pub const VVC_SLICE: Self = Self(2);
    /// VVC access unit or slice (Table 2-57)
    pub const VVC_ACCESS_UNIT_OR_SLICE: Self = Self(3);
    /// VVC tile of slices (Table 2-57)
    pub const VVC_TILE_OF_SLICES: Self = Self(4);
    /// VVC access unit or tile of slices (Table 2-57)
    pub const VVC_ACCESS_UNIT_OR_TILE_OF_SLICES: Self = Self(5);
    /// VVC slice or tile of slices (Table 2-57)
    pub const VVC_SLICE_OR_TILE_OF_SLICES: Self = Self(6);
    /// VVC access unit or slice or tile of slices (Table 2-57)
    pub const VVC_ACCESS_UNIT_OR_SLICE_OR_TILE_OF_SLICES: Self = Self(7);
    /// VVC subpicture (Table 2-57)
    pub const VVC_SUBPICTURE: Self = Self(8);
    /// VVC access unit or subpicture (Table 2-57)
    pub const VVC_ACCESS_UNIT_OR_SUBPICTURE: Self = Self(9);
    /// VVC slice or subpicture (Table 2-57)
    pub const VVC_SLICE_OR_SUBPICTURE: Self = Self(10);
    /// VVC subpicture or tile of slices (Table 2-57)
    pub const VVC_SUBPICTURE_OR_TILE_OF_SLICES: Self = Self(11);
    /// VVC access unit or slice or subpicture (Table 2-57)
    pub const VVC_ACCESS_UNIT_OR_SLICE_OR_SUBPICTURE: Self = Self(12);
    /// VVC access unit or subpicture or tile of slices (Table 2-57)
    pub const VVC_ACCESS_UNIT_OR_SUBPICTURE_OR_TILE_OF_SLICES: Self = Self(13);
    /// VVC slice or subpicture or tile of slices (Table 2-57)
    pub const VVC_SLICE_OR_SUBPICTURE_OR_TILE_OF_SLICES: Self = Self(14);
    /// VVC access unit or slice or subpicture or tile of slices (Table 2-57)
    pub const VVC_ACCESS_UNIT_OR_SLICE_OR_SUBPICTURE_OR_TILE_OF_SLICES: Self = Self(15);
    /// EVC access unit (Table 2-58)
    pub const EVC_ACCESS_UNIT: Self = Self(1);
    /// EVC slice (Table 2-58)
    pub const EVC_SLICE: Self = Self(2);
    /// EVC access unit or slice (Table 2-58)
    pub const EVC_ACCESS_UNIT_OR_SLICE: Self = Self(3);

    /// Returns the 8-bit value
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl From<u8> for AlignmentType {
    fn from(value: u8) -> Self {
        Self(value)
    }
}

/// Data stream alignment descriptor, which names the unit a PES packet of
/// the stream starts with when `data_alignment_indicator` is set
///
/// `data_stream_alignment_descriptor`, ISO/IEC 13818-1 §2.6.10, Table 2-52,
/// semantics §2.6.11
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::data_stream_alignment::{
///     AlignmentType, DataStreamAlignmentDescriptor,
/// };
///
/// let bytes = [0x06, 0x01, 0x02];
/// let descriptor = DataStreamAlignmentDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(descriptor.alignment_type, AlignmentType::VIDEO_ACCESS_UNIT);
///
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), mpeg2_systems::descriptor::Error>(())
/// ```
#[doc(alias = "data_stream_alignment_descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DataStreamAlignmentDescriptor {
    /// Unit a PES packet of the stream starts with
    pub alignment_type: AlignmentType,
}

impl DataStreamAlignmentDescriptor {
    /// Creates the descriptor of `alignment_type`
    #[must_use]
    pub const fn new(alignment_type: AlignmentType) -> Self {
        Self { alignment_type }
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

    /// Appends the descriptor to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// [`Forbidden`](Error::Forbidden): `alignment_type` is 0 or 16 to 255,
    /// reserved in every one of Tables 2-53 to 2-58.
    #[cfg(feature = "alloc")]
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "every syntax element writes from a reference"
    )]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let alignment_type = self.alignment_type.get();
        if !matches!(alignment_type, 1..=15) {
            return Err(Error::Forbidden {
                field: "alignment_type",
                value: u32::from(alignment_type),
            });
        }

        write_descriptor(
            out,
            DescriptorTag::DATA_STREAM_ALIGNMENT,
            BODY_LENGTH,
            |writer| {
                writer
                    .write_bits::<8>(alignment_type)
                    .map_err(super::field_error("alignment_type"))
            },
        )
    }
}

impl TryFrom<&RawDescriptor<'_>> for DataStreamAlignmentDescriptor {
    type Error = Error;

    /// Takes the fields of a data stream alignment descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x06.
    /// * [`BodyLength`](Error::BodyLength): the body is not 1 byte.
    fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
        let [alignment_type] =
            *fixed_body::<BODY_LENGTH>(raw, DescriptorTag::DATA_STREAM_ALIGNMENT)?;

        Ok(Self::new(AlignmentType(alignment_type)))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    #[cfg(feature = "alloc")]
    use super::AlignmentType;
    use super::DataStreamAlignmentDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};

    #[test]
    fn a_body_of_other_than_1_byte_is_refused() {
        assert_eq!(
            DataStreamAlignmentDescriptor::try_from(&RawDescriptor::new(
                DescriptorTag::DATA_STREAM_ALIGNMENT,
                &[]
            )),
            Err(Error::BodyLength {
                descriptor_tag: DescriptorTag::DATA_STREAM_ALIGNMENT,
                length: 0
            })
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn alignment_types_reserved_in_every_table_are_refused_before_a_byte_is_written() {
        let mut written = Vec::new();

        assert_eq!(
            [0, 16, 15].map(|value| {
                DataStreamAlignmentDescriptor::new(AlignmentType::from(value)).write(&mut written)
            }),
            [
                Err(Error::Forbidden {
                    field: "alignment_type",
                    value: 0
                }),
                Err(Error::Forbidden {
                    field: "alignment_type",
                    value: 16
                }),
                Ok(())
            ]
        );
        assert_eq!(written, [0x06, 0x01, 0x0f]);
    }
}
