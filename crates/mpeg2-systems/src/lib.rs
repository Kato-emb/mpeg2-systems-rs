//! Sans-IO reading and writing of MPEG-2 Systems streams (ISO/IEC 13818-1),
//! the transport stream and the program stream. No type does I/O: the caller
//! hands in the bytes it has read and writes out the bytes it is given, so the
//! crate is `no_std` and runs wherever the caller's I/O does. The crate is
//! under construction and holds nothing yet.

#![no_std]

#[cfg(feature = "alloc")]
#[expect(
    unused_extern_crates,
    reason = "nothing allocates yet; the first item that does lifts this"
)]
extern crate alloc;
