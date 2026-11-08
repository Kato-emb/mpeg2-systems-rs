//! Program Stream: the pack header and the system header that open a pack,
//! the program stream map, and the code that ends the stream
//!
//! Each type reads one syntax element from bytes that start with its start
//! code and hold nothing else. Finding where an element starts and ends in a
//! stream is the work of a framer, which this module does not do.

pub mod pack_header;
pub mod psm;
pub mod system_header;

pub use pack_header::PackHeader;
pub use psm::ProgramStreamMap;
pub use system_header::{StreamBound, StreamBoundLoop, SystemHeader};

/// `pack_start_code`, the 32 bits a pack header starts with, ISO/IEC 13818-1
/// §2.5.3.4
pub const PACK_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xba];

/// `system_header_start_code`, the 32 bits a system header starts with,
/// ISO/IEC 13818-1 §2.5.3.6
pub const SYSTEM_HEADER_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xbb];

/// `packet_start_code_prefix` and `map_stream_id`, the 32 bits a program
/// stream map starts with, ISO/IEC 13818-1 §2.5.4.2
pub const PROGRAM_STREAM_MAP_START_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xbc];

/// `MPEG_program_end_code`, the 32 bits that end a Program Stream, ISO/IEC
/// 13818-1 §2.5.3.2
pub const MPEG_PROGRAM_END_CODE: [u8; 4] = [0x00, 0x00, 0x01, 0xb9];

/// Returns whether `bytes` are the `MPEG_program_end_code` and nothing else
///
/// # Examples
///
/// ```
/// use mpeg2_systems::ps::{self, MPEG_PROGRAM_END_CODE};
///
/// assert!(ps::is_mpeg_program_end_code(&[0x00, 0x00, 0x01, 0xb9]));
/// assert!(!ps::is_mpeg_program_end_code(&[0x00, 0x00, 0x01, 0xba]));
/// assert!(!ps::is_mpeg_program_end_code(&MPEG_PROGRAM_END_CODE[..3]));
/// ```
#[must_use]
pub fn is_mpeg_program_end_code(bytes: &[u8]) -> bool {
    bytes == MPEG_PROGRAM_END_CODE
}
