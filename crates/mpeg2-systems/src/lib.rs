//! Sans-IO reading and writing of MPEG-2 Systems streams (ISO/IEC 13818-1),
//! the transport stream and the program stream. No type does I/O: the caller
//! hands in the bytes it has read and writes out the bytes it is given, so the
//! crate is `no_std` and runs wherever the caller's I/O does. The crate is
//! under construction.
//!
//! With the `alloc` feature, `ts::demux` and `ps::demux` read a stream, and
//! the doc of `ps::mux` shows the order to write a Program Stream in.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod crc;
pub mod descriptor;
pub mod field;
#[cfg(feature = "alloc")]
mod input_buffer;
pub mod pes;
pub mod ps;
pub mod psi;
pub mod stream_type;
pub mod time;
pub mod ts;
