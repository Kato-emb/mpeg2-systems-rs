//! Packet identifier, which names the stream each Transport Packet belongs to

use core::fmt;

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
/// use mpeg2_systems::ts::{Pid, pid::PidOutOfRange};
///
/// assert_eq!(Pid::try_from(0x1fff), Ok(Pid::NULL));
/// assert_eq!(Pid::try_from(0x2000).map_err(PidOutOfRange::value), Err(0x2000));
/// ```
#[doc(alias = "PID")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Pid(pub(super) u16);

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

#[cfg(test)]
mod tests {
    use super::{Pid, PidOutOfRange};

    #[test]
    fn a_pid_takes_any_13_bit_value_and_refuses_a_wider_one() {
        assert_eq!(
            [Pid::try_from(0x1fff), Pid::try_from(0x2000)],
            [Ok(Pid::NULL), Err(PidOutOfRange { value: 0x2000 })]
        );
    }
}
