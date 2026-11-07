//! Descriptors, which extend the definition of a program or a program
//! element, and the loops that carry them
//!
//! A table carries its descriptors as a [`DescriptorLoop`], which is read
//! descriptor by descriptor as it is walked. Each descriptor comes out as a
//! [`RawDescriptor`]: its tag and its body, whatever the tag. A typed
//! descriptor is converted from a [`RawDescriptor`] with `TryFrom`, and a
//! crate that defines descriptors of its own, for the user private tags,
//! implements the same `TryFrom` for its types. A tag nobody converts stays a
//! [`RawDescriptor`] and is written back as read.
//!
//! A typed descriptor reads the values its specification reserves or forbids
//! and keeps them, but its `write` refuses them, so a descriptor read is not
//! always written back as it came.
//!
//! The descriptors of tags 0x02 to 0x0A have a module each: [`video_stream`],
//! [`audio_stream`], [`hierarchy`], [`registration`],
//! [`data_stream_alignment`], [`target_background_grid`], [`video_window`],
//! [`ca`] and [`iso_639_language`].
//!
//! # Examples
//!
//! ```
//! use mpeg2_systems::descriptor::{DescriptorLoop, RawDescriptor};
//!
//! /// A descriptor a crate defines for the user private tag 0x52
//! struct StreamIdentifier {
//!     component_tag: u8,
//! }
//!
//! impl TryFrom<&RawDescriptor<'_>> for StreamIdentifier {
//!     type Error = &'static str;
//!
//!     fn try_from(raw: &RawDescriptor<'_>) -> Result<Self, Self::Error> {
//!         match (raw.descriptor_tag.get(), raw.body) {
//!             (0x52, [component_tag]) => Ok(Self {
//!                 component_tag: *component_tag,
//!             }),
//!             _ => Err("not a stream identifier descriptor"),
//!         }
//!     }
//! }
//!
//! // A registration descriptor, then a stream identifier descriptor
//! let bytes = [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56, 0x52, 0x01, 0x30];
//! let mut descriptors = DescriptorLoop::from(bytes.as_slice()).iter();
//!
//! let registration = descriptors.next().unwrap()?;
//! assert_eq!(registration.body, b"HDMV");
//! let stream_identifier = StreamIdentifier::try_from(&descriptors.next().unwrap()?);
//! assert_eq!(stream_identifier.map(|descriptor| descriptor.component_tag), Ok(0x30));
//! assert!(descriptors.next().is_none());
//! # Ok::<(), mpeg2_systems::descriptor::Error>(())
//! ```

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::fmt;

#[cfg(feature = "alloc")]
use crate::field::FieldWriter;
use crate::field::{self, FieldReader};

pub mod audio_stream;
pub mod ca;
pub mod data_stream_alignment;
pub mod hierarchy;
pub mod iso_639_language;
pub mod registration;
pub mod tag;
pub mod target_background_grid;
pub mod video_stream;
pub mod video_window;

pub use tag::DescriptorTag;

/// Bytes of `descriptor_tag` and `descriptor_length`
const HEADER_LENGTH: usize = 2;

/// Failure to read or write a descriptor or a descriptor loop
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Error {
    /// The bytes end before `descriptor_length`
    Field {
        /// The name of the field in the syntax table
        field: &'static str,
        /// How the field ran past the end
        error: field::Error,
    },
    /// The bytes after `descriptor_length` are not as many as it declares
    Length {
        /// The `descriptor_length` read
        descriptor_length: u8,
        /// The bytes given after `descriptor_length`
        available_bytes: usize,
    },
    /// The body to write is longer than the 255 bytes `descriptor_length`
    /// counts
    DescriptorLength {
        /// The bytes of body given
        length: usize,
    },
    /// The `descriptor_tag` to write is 0x01, which Table 2-45 forbids
    ForbiddenTag,
    /// The descriptor converted is of another tag than the type takes
    DescriptorTag {
        /// The tag the type takes
        expected: DescriptorTag,
        /// The tag of the descriptor
        found: DescriptorTag,
    },
    /// The body of the descriptor converted is not as long as the syntax of
    /// its tag
    BodyLength {
        /// The tag of the descriptor
        descriptor_tag: DescriptorTag,
        /// The bytes of body found
        length: usize,
    },
    /// A field to write has a value its semantics forbid or reserve
    Forbidden {
        /// The name of the field in the syntax table
        field: &'static str,
        /// The value given
        value: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field { field, .. } => {
                write!(formatter, "field {field} could not be read or written")
            }
            Self::Length {
                descriptor_length,
                available_bytes,
            } => write!(
                formatter,
                "descriptor_length {descriptor_length} does not match the {available_bytes} \
                 bytes given"
            ),
            Self::DescriptorLength { length } => write!(
                formatter,
                "descriptor body of {length} bytes exceeds the 255 descriptor_length counts"
            ),
            Self::ForbiddenTag => write!(formatter, "descriptor_tag 0x01 is forbidden"),
            Self::DescriptorTag { expected, found } => write!(
                formatter,
                "descriptor_tag {:#04x} is not {:#04x}, the tag of the descriptor type",
                found.get(),
                expected.get()
            ),
            Self::BodyLength {
                descriptor_tag,
                length,
            } => write!(
                formatter,
                "a body of {length} bytes does not fit the syntax of descriptor_tag {:#04x}",
                descriptor_tag.get()
            ),
            Self::Forbidden { field, value } => write!(formatter, "{field} {value} is forbidden"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Field { error, .. } => Some(error),
            Self::Length { .. }
            | Self::DescriptorLength { .. }
            | Self::ForbiddenTag
            | Self::DescriptorTag { .. }
            | Self::BodyLength { .. }
            | Self::Forbidden { .. } => None,
        }
    }
}

/// Returns the body of `raw`, refusing a descriptor of another tag than
/// `expected`
pub(crate) const fn body_of<'input>(
    raw: &RawDescriptor<'input>,
    expected: DescriptorTag,
) -> Result<&'input [u8], Error> {
    if raw.descriptor_tag.get() != expected.get() {
        return Err(Error::DescriptorTag {
            expected,
            found: raw.descriptor_tag,
        });
    }

    Ok(raw.body)
}

/// Returns the body of `raw`, refusing a descriptor of another tag than
/// `expected` or a body of other than `LENGTH` bytes
pub(crate) fn fixed_body<'input, const LENGTH: usize>(
    raw: &RawDescriptor<'input>,
    expected: DescriptorTag,
) -> Result<&'input [u8; LENGTH], Error> {
    let body = body_of(raw, expected)?;
    body.try_into().map_err(|_| Error::BodyLength {
        descriptor_tag: expected,
        length: body.len(),
    })
}

/// Returns the body of `raw` split after its first `LENGTH` bytes, refusing a
/// descriptor of another tag than `expected` or a body shorter than `LENGTH`
pub(crate) fn prefix_body<'input, const LENGTH: usize>(
    raw: &RawDescriptor<'input>,
    expected: DescriptorTag,
) -> Result<(&'input [u8; LENGTH], &'input [u8]), Error> {
    let body = body_of(raw, expected)?;
    body.split_first_chunk().ok_or(Error::BodyLength {
        descriptor_tag: expected,
        length: body.len(),
    })
}

/// Returns a mapping of a failure on `field` to an [`Error::Field`]
pub(crate) const fn field_error(field: &'static str) -> impl Fn(field::Error) -> Error {
    move |error| Error::Field { field, error }
}

/// Appends a descriptor of `descriptor_tag` to `out`, its body of
/// `body_length` bytes laid out by `write_body`, leaving `out` as it was if
/// the body is too long or a field is refused
#[cfg(feature = "alloc")]
pub(crate) fn write_descriptor(
    out: &mut Vec<u8>,
    descriptor_tag: DescriptorTag,
    body_length: usize,
    write_body: impl FnOnce(&mut FieldWriter<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    let descriptor_length = u8::try_from(body_length).map_err(|_| Error::DescriptorLength {
        length: body_length,
    })?;
    let start = out.len();
    out.push(descriptor_tag.get());
    out.push(descriptor_length);
    let mut writer = FieldWriter::new(out);
    let written = write_body(&mut writer)
        .and_then(|()| writer.finish().map_err(field_error("descriptor body")));
    if written.is_err() {
        out.truncate(start);
    }

    written
}

/// Descriptor whose body is kept as bytes, whatever its tag
///
/// `descriptor()`, ISO/IEC 13818-1 §2.6, semantics §2.6.1
///
/// `descriptor_length` follows from the body. A typed descriptor is
/// converted from it with `TryFrom` (see the [module](self)).
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::{DescriptorTag, Error, RawDescriptor};
///
/// // A registration descriptor of format_identifier "HDMV"
/// let bytes = [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56];
/// let descriptor = RawDescriptor::read(&bytes)?;
/// assert_eq!(descriptor.descriptor_tag, DescriptorTag::REGISTRATION);
/// assert_eq!(descriptor.body, b"HDMV");
///
/// // Written back (with the `alloc` feature), it gives the same bytes
/// # #[cfg(feature = "alloc")]
/// # {
/// let mut written = Vec::new();
/// descriptor.write(&mut written)?;
/// assert_eq!(written, bytes);
/// # }
/// # Ok::<(), Error>(())
/// ```
#[doc(alias = "descriptor")]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RawDescriptor<'input> {
    /// The syntax of the body
    pub descriptor_tag: DescriptorTag,
    /// The bytes after `descriptor_length`
    pub body: &'input [u8],
}

impl<'input> RawDescriptor<'input> {
    /// Creates a descriptor of `descriptor_tag` carrying `body`
    #[must_use]
    pub const fn new(descriptor_tag: DescriptorTag, body: &'input [u8]) -> Self {
        Self {
            descriptor_tag,
            body,
        }
    }

    /// Takes the descriptor at the front of `bytes`, and returns it with the
    /// bytes after it
    fn frame(bytes: &'input [u8]) -> Result<(Self, &'input [u8]), Error> {
        let mut reader = FieldReader::new(bytes);
        let (descriptor_tag, descriptor_length) = reader
            .read_u8::<8>()
            .and_then(|tag| Ok((DescriptorTag::from(tag), reader.read_u8::<8>()?)))
            .map_err(|error| Error::Field {
                field: "descriptor_length",
                error,
            })?;
        let available_bytes = reader.remaining_bytes();
        let body = reader
            .read_bytes(usize::from(descriptor_length))
            .map_err(|_| Error::Length {
                descriptor_length,
                available_bytes,
            })?;
        // Why not an error: the body ends on a byte boundary.
        let rest = reader
            .read_bytes(reader.remaining_bytes())
            .unwrap_or_default();

        Ok((Self::new(descriptor_tag, body), rest))
    }

    /// Reads a descriptor from `bytes`, which hold it whole and nothing more
    ///
    /// # Errors
    ///
    /// * [`Field`](Error::Field): `bytes` end before `descriptor_length`.
    /// * [`Length`](Error::Length): `descriptor_length` does not match the
    ///   bytes after it.
    pub fn read(bytes: &'input [u8]) -> Result<Self, Error> {
        let (descriptor, rest) = Self::frame(bytes)?;
        if !rest.is_empty() {
            return Err(Error::Length {
                descriptor_length: u8::try_from(descriptor.body.len()).unwrap_or(u8::MAX),
                available_bytes: bytes.len().saturating_sub(HEADER_LENGTH),
            });
        }

        Ok(descriptor)
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        HEADER_LENGTH.saturating_add(self.body.len())
    }

    /// Appends the descriptor to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`ForbiddenTag`](Error::ForbiddenTag): `descriptor_tag` is 0x01.
    /// * [`DescriptorLength`](Error::DescriptorLength): `body` is longer than
    ///   255 bytes.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        if self.descriptor_tag == DescriptorTag::FORBIDDEN {
            return Err(Error::ForbiddenTag);
        }
        let descriptor_length =
            u8::try_from(self.body.len()).map_err(|_| Error::DescriptorLength {
                length: self.body.len(),
            })?;

        out.push(self.descriptor_tag.get());
        out.push(descriptor_length);
        out.extend_from_slice(self.body);

        Ok(())
    }
}

/// A loop of descriptors, borrowed
///
/// The bytes are framed descriptor by descriptor as [`iter`](Self::iter)
/// walks them, so a view of any bytes can be made; a descriptor the bytes end
/// inside is an error of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescriptorLoop<'input>(pub(crate) &'input [u8]);

impl<'input> DescriptorLoop<'input> {
    /// Returns the bytes of the loop
    #[must_use]
    pub const fn as_bytes(self) -> &'input [u8] {
        self.0
    }

    /// Returns whether the loop has no bytes
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Returns an iterator over the descriptors, each framed as it is reached
    #[must_use]
    pub const fn iter(self) -> DescriptorIter<'input> {
        DescriptorIter(self.0)
    }

    /// Appends the bytes of a loop of `descriptors` to `out`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// The errors of [`RawDescriptor::write`].
    ///
    /// # Examples
    ///
    /// ```
    /// # #[cfg(feature = "alloc")]
    /// # {
    /// use mpeg2_systems::descriptor::{DescriptorLoop, DescriptorTag, RawDescriptor};
    ///
    /// let mut bytes = Vec::new();
    /// DescriptorLoop::write_from(
    ///     [RawDescriptor::new(DescriptorTag::REGISTRATION, b"HDMV")],
    ///     &mut bytes,
    /// )?;
    /// assert_eq!(bytes, [0x05, 0x04, 0x48, 0x44, 0x4d, 0x56]);
    /// # }
    /// # Ok::<(), mpeg2_systems::descriptor::Error>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn write_from<'descriptor>(
        descriptors: impl IntoIterator<Item = RawDescriptor<'descriptor>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        Self::write_entries(descriptors.into_iter().map(Ok), out)
    }

    /// Appends the bytes of `entries` to `out`, leaving `out` as it was if an
    /// entry is an error or is refused
    #[cfg(feature = "alloc")]
    pub(crate) fn write_entries<'descriptor>(
        entries: impl IntoIterator<Item = Result<RawDescriptor<'descriptor>, Error>>,
        out: &mut Vec<u8>,
    ) -> Result<(), Error> {
        let start = out.len();
        for descriptor in entries {
            if let Err(error) = descriptor.and_then(|descriptor| descriptor.write(out)) {
                out.truncate(start);
                return Err(error);
            }
        }

        Ok(())
    }
}

impl<'input> From<&'input [u8]> for DescriptorLoop<'input> {
    fn from(bytes: &'input [u8]) -> Self {
        Self(bytes)
    }
}

impl<'input> IntoIterator for DescriptorLoop<'input> {
    type Item = Result<RawDescriptor<'input>, Error>;
    type IntoIter = DescriptorIter<'input>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the descriptors of a [`DescriptorLoop`]
///
/// A descriptor the bytes end inside is an error, and the last item: nothing
/// after a header that cannot be framed can be located.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DescriptorIter<'input>(&'input [u8]);

impl<'input> Iterator for DescriptorIter<'input> {
    type Item = Result<RawDescriptor<'input>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }

        // Why end the walk on an error: the bytes after a descriptor that
        // cannot be framed have no known position, so resynchronizing would
        // read bytes whose meaning is unknown.
        let (item, rest) = match RawDescriptor::frame(self.0) {
            Ok((descriptor, rest)) => (Ok(descriptor), rest),
            Err(error) => (Err(error), &[][..]),
        };
        self.0 = rest;

        Some(item)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;
    use core::array;

    use super::ca::CaDescriptor;
    use super::registration::RegistrationDescriptor;
    use super::video_window::VideoWindowDescriptor;
    use super::{DescriptorLoop, DescriptorTag, Error, RawDescriptor};
    use crate::field;

    /// A `registration_descriptor` of "HDMV", an `ISO_639_language_descriptor`
    /// of "jpn" and a user private descriptor of tag 0x52, laid out by hand
    const THREE_DESCRIPTORS: [u8; 15] = [
        0x05, 0x04, 0x48, 0x44, 0x4d, 0x56, 0x0a, 0x04, 0x6a, 0x70, 0x6e, 0x00, 0x52, 0x01, 0x30,
    ];

    fn three_descriptors() -> [RawDescriptor<'static>; 3] {
        [
            RawDescriptor::new(DescriptorTag::REGISTRATION, b"HDMV"),
            RawDescriptor::new(DescriptorTag::ISO_639_LANGUAGE, b"jpn\0"),
            RawDescriptor::new(DescriptorTag::from(0x52), &[0x30]),
        ]
    }

    #[test]
    fn a_loop_yields_each_descriptor_with_its_tag_and_body() {
        let mut descriptors = DescriptorLoop::from(THREE_DESCRIPTORS.as_slice()).iter();
        let [registration, language, private] = three_descriptors();

        assert_eq!(
            array::from_fn(|_| descriptors.next()),
            [
                Some(Ok(registration)),
                Some(Ok(language)),
                Some(Ok(private)),
                None
            ]
        );
    }

    #[test]
    fn a_descriptor_running_past_the_loop_ends_it_after_the_descriptors_before() {
        let bytes = [
            0x05, 0x04, 0x48, 0x44, 0x4d, 0x56, 0x52, 0x01, 0x30, 0x0a, 0x04, 0x6a, 0x70,
        ];
        let mut descriptors = DescriptorLoop::from(bytes.as_slice()).iter();
        let [registration, _, private] = three_descriptors();

        assert_eq!(
            array::from_fn(|_| descriptors.next()),
            [
                Some(Ok(registration)),
                Some(Ok(private)),
                Some(Err(Error::Length {
                    descriptor_length: 4,
                    available_bytes: 2
                })),
                None
            ]
        );
    }

    #[test]
    fn a_loop_ending_inside_a_header_yields_an_error_then_ends() {
        let mut descriptors = DescriptorLoop::from([0x52].as_slice()).iter();

        assert_eq!(
            array::from_fn(|_| descriptors.next()),
            [
                Some(Err(Error::Field {
                    field: "descriptor_length",
                    error: field::Error::Truncated {
                        required_bits: 8,
                        remaining_bits: 0
                    }
                })),
                None
            ]
        );
    }

    #[test]
    fn an_empty_loop_yields_nothing() {
        let empty = DescriptorLoop::from([].as_slice());

        assert_eq!((empty.is_empty(), empty.iter().next()), (true, None));
    }

    #[test]
    fn a_descriptor_is_read_only_from_bytes_that_hold_it_exactly() {
        assert_eq!(
            [
                RawDescriptor::read(&[0x52, 0x01, 0x30]),
                RawDescriptor::read(&[0x52, 0x01, 0x30, 0xff]),
                RawDescriptor::read(&[0x52, 0x02, 0x30]),
            ],
            [
                Ok(RawDescriptor::new(DescriptorTag::from(0x52), &[0x30])),
                Err(Error::Length {
                    descriptor_length: 1,
                    available_bytes: 2
                }),
                Err(Error::Length {
                    descriptor_length: 2,
                    available_bytes: 1
                }),
            ]
        );
    }

    #[test]
    fn a_descriptor_whose_body_does_not_fit_its_tag_is_refused_and_the_loop_goes_on() {
        let bytes = [
            0x08, 0x03, 0x00, 0x40, 0x00, 0x05, 0x04, 0x48, 0x44, 0x4d, 0x56,
        ];
        let mut descriptors = DescriptorLoop::from(bytes.as_slice()).iter();
        let window = descriptors
            .next()
            .unwrap()
            .and_then(|raw| VideoWindowDescriptor::try_from(&raw));
        let registration = descriptors
            .next()
            .unwrap()
            .and_then(|raw| RegistrationDescriptor::try_from(&raw));
        let as_ca =
            CaDescriptor::try_from(&RawDescriptor::new(DescriptorTag::REGISTRATION, b"HDMV"));

        assert_eq!(
            (window, registration, as_ca, descriptors.next()),
            (
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::VIDEO_WINDOW,
                    length: 3
                }),
                Ok(RegistrationDescriptor::new(0x4844_4d56)),
                Err(Error::DescriptorTag {
                    expected: DescriptorTag::CA,
                    found: DescriptorTag::REGISTRATION
                }),
                None
            )
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_loop_built_from_its_descriptors_gives_the_bytes_laid_out_by_hand() {
        let mut written = Vec::new();
        DescriptorLoop::write_from(three_descriptors(), &mut written).unwrap();

        assert_eq!(written, THREE_DESCRIPTORS);
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn descriptors_the_specification_does_not_allow_are_refused_before_a_byte_is_written() {
        let long_body = [0xff; 256];
        let [registration, ..] = three_descriptors();
        let mut written = Vec::new();

        assert_eq!(
            [
                RawDescriptor::new(DescriptorTag::from(0x52), &long_body).write(&mut written),
                RawDescriptor::new(DescriptorTag::FORBIDDEN, &[]).write(&mut written),
                DescriptorLoop::write_from(
                    [
                        registration,
                        RawDescriptor::new(DescriptorTag::FORBIDDEN, &[])
                    ],
                    &mut written
                ),
            ],
            [
                Err(Error::DescriptorLength { length: 256 }),
                Err(Error::ForbiddenTag),
                Err(Error::ForbiddenTag),
            ]
        );
        assert_eq!(written, []);
    }
}
