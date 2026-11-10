//! Conditional access descriptor, which names a conditional access system and
//! the PID of its ECM or EMM stream

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

#[cfg(feature = "alloc")]
use super::write_descriptor;
use super::{DescriptorTag, Error, RawDescriptor, field_error, prefix_body};
use crate::field::FieldReader;
use crate::ts::Pid;

/// Bytes from `CA_system_ID` to `CA_PID`
const FIXED_LENGTH: usize = 4;

/// Conditional access descriptor, which names a conditional access system and
/// the PID of its ECM or EMM stream
///
/// `CA_descriptor`, ISO/IEC 13818-1 §2.6.16, Table 2-61, semantics §2.6.17
///
/// In a `TS_program_map_section` `CA_PID` carries ECMs, and in a
/// `CA_section` EMMs.
///
/// # Examples
///
/// ```
/// use mpeg2_systems::descriptor::RawDescriptor;
/// use mpeg2_systems::descriptor::ca::CaDescriptor;
///
/// // CA_system_ID 0x0005, EMMs on PID 0x0100
/// let bytes = [0x09, 0x04, 0x00, 0x05, 0xe1, 0x00];
/// let descriptor = CaDescriptor::try_from(&RawDescriptor::read(&bytes)?)?;
/// assert_eq!(
///     (descriptor.ca_system_id, descriptor.ca_pid.get()),
///     (0x0005, 0x0100)
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
#[doc(alias("CA_descriptor", "conditional_access_descriptor"))]
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CaDescriptor<'input> {
    /// Type of conditional access system, privately defined
    pub ca_system_id: u16,
    /// PID of the Transport Packets carrying the ECMs or EMMs of the system
    pub ca_pid: Pid,
    /// Bytes privately defined
    pub private_data_byte: &'input [u8],
}

impl<'input> CaDescriptor<'input> {
    /// Creates the descriptor of a system and its PID, without private data
    #[must_use]
    pub const fn new(ca_system_id: u16, ca_pid: Pid) -> Self {
        Self {
            ca_system_id,
            ca_pid,
            private_data_byte: &[],
        }
    }

    /// Returns the value with the given `ca_system_id`
    #[must_use]
    pub const fn with_ca_system_id(mut self, ca_system_id: u16) -> Self {
        self.ca_system_id = ca_system_id;
        self
    }

    /// Returns the value with the given `ca_pid`
    #[must_use]
    pub const fn with_ca_pid(mut self, ca_pid: Pid) -> Self {
        self.ca_pid = ca_pid;
        self
    }

    /// Returns the value with the given `private_data_byte`
    #[must_use]
    pub const fn with_private_data_byte(mut self, private_data_byte: &'input [u8]) -> Self {
        self.private_data_byte = private_data_byte;
        self
    }

    /// Returns the number of bytes [`write`](Self::write) appends
    #[must_use]
    pub const fn encoded_len(&self) -> usize {
        super::HEADER_LENGTH
            .saturating_add(FIXED_LENGTH)
            .saturating_add(self.private_data_byte.len())
    }

    /// Appends the descriptor to `out`, its `reserved` bits `'111'`
    ///
    /// A write that fails leaves `out` as it was.
    ///
    /// # Errors
    ///
    /// * [`Forbidden`](Error::Forbidden): `ca_pid` is other than 0x0003, the
    ///   IPMP control information table §2.6.17 names, and 0x0010 to 0x1FFE,
    ///   the PIDs Table 2-3 lets be assigned.
    /// * [`DescriptorLength`](Error::DescriptorLength): the body would be
    ///   longer than 255 bytes.
    #[cfg(feature = "alloc")]
    pub fn write(&self, out: &mut Vec<u8>) -> Result<(), Error> {
        let ca_pid = self.ca_pid.get();
        if !(self.ca_pid.is_assignable() || self.ca_pid == Pid::IPMP) {
            return Err(Error::Forbidden {
                field: "CA_PID",
                value: u32::from(ca_pid),
            });
        }

        let body_length = FIXED_LENGTH.saturating_add(self.private_data_byte.len());
        write_descriptor(out, DescriptorTag::CA, body_length, |writer| {
            writer
                .write_bits::<16>(self.ca_system_id)
                .map_err(field_error("CA_system_ID"))?;
            writer
                .write_bits::<3>(0b111_u8)
                .map_err(field_error("reserved"))?;
            writer
                .write_bits::<13>(ca_pid)
                .map_err(field_error("CA_PID"))?;
            writer
                .write_bytes(self.private_data_byte)
                .map_err(field_error("private_data_byte"))
        })
    }
}

impl<'input> TryFrom<&RawDescriptor<'input>> for CaDescriptor<'input> {
    type Error = Error;

    /// Takes the fields of a CA descriptor
    ///
    /// # Errors
    ///
    /// * [`DescriptorTag`](Error::DescriptorTag): the tag is not 0x09.
    /// * [`BodyLength`](Error::BodyLength): the body is shorter than
    ///   `CA_system_ID` and `CA_PID`.
    fn try_from(raw: &RawDescriptor<'input>) -> Result<Self, Self::Error> {
        let (fixed, private_data_byte) = prefix_body::<FIXED_LENGTH>(raw, DescriptorTag::CA)?;
        let mut reader = FieldReader::new(fixed);
        // Why not an error per field: the fixed part is as long as the fields.
        (|| {
            let ca_system_id = reader.read_u16::<16>()?;
            reader.read_bits::<3>()?;
            Ok(Self {
                ca_system_id,
                ca_pid: Pid(reader.read_u16::<13>()?),
                private_data_byte,
            })
        })()
        .map_err(field_error("descriptor body"))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "alloc")]
    use alloc::vec::Vec;

    use super::CaDescriptor;
    use crate::descriptor::{DescriptorTag, Error, RawDescriptor};
    use crate::ts::Pid;

    /// `CA_system_ID` 0x0B00, ECMs on PID 0x0123, two bytes of private data
    const WITH_PRIVATE_DATA: [u8; 8] = [0x09, 0x06, 0x0b, 0x00, 0xe1, 0x23, 0xaa, 0xbb];

    fn with_private_data() -> CaDescriptor<'static> {
        let mut descriptor = CaDescriptor::new(0x0b00, Pid::try_from(0x0123).unwrap());
        descriptor.private_data_byte = &[0xaa, 0xbb];
        descriptor
    }

    #[test]
    fn the_bytes_after_ca_pid_are_the_private_data() {
        assert_eq!(
            [
                RawDescriptor::read(&WITH_PRIVATE_DATA).unwrap(),
                RawDescriptor::new(DescriptorTag::CA, &[0x0b, 0x00, 0xe1]),
            ]
            .map(|raw| CaDescriptor::try_from(&raw)),
            [
                Ok(with_private_data()),
                Err(Error::BodyLength {
                    descriptor_tag: DescriptorTag::CA,
                    length: 3
                })
            ]
        );
    }

    #[cfg(feature = "alloc")]
    #[test]
    fn a_descriptor_is_written_as_laid_out_by_hand_unless_its_pid_is_reserved() {
        let with_pid = |pid| {
            let mut descriptor = with_private_data();
            descriptor.ca_pid = Pid::try_from(pid).unwrap();
            descriptor
        };
        let mut written = Vec::new();

        assert_eq!(
            [with_pid(0x0002), with_pid(0x1fff), with_private_data()]
                .map(|descriptor| descriptor.write(&mut written)),
            [
                Err(Error::Forbidden {
                    field: "CA_PID",
                    value: 0x0002
                }),
                Err(Error::Forbidden {
                    field: "CA_PID",
                    value: 0x1fff
                }),
                Ok(())
            ]
        );
        assert_eq!(written, WITH_PRIVATE_DATA);
    }
}
