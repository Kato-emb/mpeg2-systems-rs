//! Program Stream: the pack header and the system header that open a pack,
//! the program stream map, and the code that ends the stream
//!
//! Each type reads one syntax element from bytes that start with its start
//! code and hold nothing else. Finding where an element starts and ends in a
//! stream is the work of the demuxer in `demux`, which takes the `alloc`
//! feature.

#[cfg(feature = "alloc")]
pub mod demux;
#[cfg(feature = "alloc")]
pub mod mux;
pub mod pack_header;
pub mod psm;
mod start_code;
pub mod system_header;

pub use pack_header::PackHeader;
pub use psm::ProgramStreamMap;
pub use start_code::{
    MPEG_PROGRAM_END_CODE, PACK_START_CODE, PROGRAM_STREAM_MAP_START_CODE,
    SYSTEM_HEADER_START_CODE, is_mpeg_program_end_code,
};
pub use system_header::{StreamBound, StreamBoundLoop, SystemHeader};
