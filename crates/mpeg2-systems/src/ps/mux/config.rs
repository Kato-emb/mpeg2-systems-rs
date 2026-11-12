//! The settings of a [`Muxer`](super::Muxer)

use core::time::Duration;

/// Settings of a [`Muxer`](super::Muxer)
///
/// [`MuxerConfig::new`] takes the multiplex rate and `mux_delay`, which the
/// specification leaves to the caller, and gives the default of each other
/// setting; change the fields of the value it returns.
///
/// ```
/// use core::time::Duration;
/// use mpeg2_systems::ps::mux::MuxerConfig;
///
/// // 1 260 000 bytes a second, each PES packet at most 0.5 s before its time
/// let config = MuxerConfig::new(25_200, Duration::from_millis(500)).with_pack_size(2_324);
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MuxerConfig {
    /// `program_mux_rate` of every pack, and `rate_bound` of the system
    /// header, in units of 50 bytes per second
    ///
    /// [`Muxer::new`](super::Muxer::new) takes from 1 to 2^22 − 1 (ISO/IEC 13818-1 §2.5.3.4).
    pub program_mux_rate: u32,
    /// The most bytes of a pack (2048 by default)
    ///
    /// [`Muxer::new`](super::Muxer::new) takes any value from the bytes of a pack header, the
    /// shortest PES packet header and one byte of payload upward. A pack is
    /// never longer than the bytes that arrive at `program_mux_rate`, at the
    /// rate ISO/IEC 13818-1 §2.5.2.2 equation 2-21 gives, within the 0.7 s
    /// §2.7.1 allows from one SCR to the next, whatever `pack_size`.
    pub pack_size: usize,
    /// The longest time the first pack of a PES packet arrives before its
    /// DTS, or its PTS when it has no DTS
    ///
    /// [`Muxer::new`](super::Muxer::new) takes up to 1 s, the longest a byte
    /// may wait in the P-STD before it is decoded (ISO/IEC 13818-1 §2.5.2.3);
    /// the longer waits that section allows still picture video and ISO/IEC
    /// 14496 streams are not taken.
    ///
    /// The SCR of the pack is that time less `mux_delay`, or later where the
    /// packs before it are still arriving or the input buffer of its stream
    /// in the P-STD has no room for it yet. A PES packet that would not
    /// arrive in full within it is refused, as every PES packet with a
    /// payload is at 0.
    pub mux_delay: Duration,
    /// The most bytes the output held for
    /// [`Muxer::poll_output`](super::Muxer::poll_output) may grow to (4 MiB by
    /// default)
    ///
    /// Any value is taken: at 0 every PES packet is refused, and at
    /// [`usize::MAX`] none for its length. An input that would grow the
    /// output held past it is refused, so the limit must be larger than the
    /// packs of the largest PES packet given, with the system header and the
    /// program stream map they carry.
    pub buffer_limit: usize,
    /// The time from one sending of the system header and the program stream
    /// map to the next ([`Duration::MAX`] by default)
    ///
    /// They go in the first pack of a PES packet whose SCR is
    /// `header_interval` or more after that of the last pack that carried
    /// them. Any value is taken, both ends included: 0 sends them in the pack
    /// of every PES packet, and [`Duration::MAX`] only in the first pack.
    /// ISO/IEC 13818-1 places no requirement on how often they are sent
    /// (§2.7.8, §2.5.4).
    pub header_interval: Duration,
    /// Whether the stream has a fixed bitrate, `fixed_flag` `'1'` (`false`
    /// by default)
    ///
    /// The SCRs then follow ISO/IEC 13818-1 §2.5.3.6 equations 2-22 and
    /// 2-23, c1 the time a byte takes to arrive at `program_mux_rate`
    /// (§2.5.2.2 equation 2-21): each pack follows the one before without a
    /// gap. Where the SCR of a PES packet's pack would be before its time
    /// less `mux_delay`, packs of padding fill the bytes up to the first SCR
    /// on the line at or after that time. Where fewer than the bytes of a
    /// pack header would be left over, they are not filled, and the SCR is
    /// earlier than that time by no more than those fewer than 14 bytes take
    /// to arrive: the SCR of a PES packet's pack is at its time less
    /// `mux_delay`, or up to 13 bytes earlier, unless the packs before it
    /// end later.
    ///
    /// A pack of padding is at most the most bytes of a pack
    /// ([`pack_size`](Self::pack_size), or fewer where fewer arrive within
    /// 0.7 s at `program_mux_rate`), with `stuffing_byte`s where 7 or fewer
    /// fill it (§2.5.3.4) and PES packets of `padding_stream` past them
    /// (§2.4.3.6), as many as the 65 535 bytes of `PES_packet_length` need.
    /// The system header names no bound for `padding_stream`.
    pub fixed_rate: bool,
    /// Whether the stream is a constrained system parameter stream (CSPS),
    /// `CSPS_flag` `'1'` (`false` by default)
    ///
    /// The stream then keeps to the bounds of ISO/IEC 13818-1 §2.7.9, and
    /// the system header has `packet_rate_restriction_flag` `'0'`:
    ///
    /// - The packets of each pack, its PES packets, system header, program
    ///   stream map and PES packets of `padding_stream`, arrive no faster
    ///   than equation 2-28 allows, `rate_bound` the `program_mux_rate`. At a
    ///   variable rate the SCR of the pack after it is that much later, and
    ///   a pack header alone goes before `MPEG_program_end_code` where the
    ///   end code would come sooner. At a [`fixed_rate`](Self::fixed_rate)
    ///   each pack is filled to the bytes that take that long, the system
    ///   header and the program stream map go in packs of their own before
    ///   the PES packet they are sent with, and a pack of padding is no
    ///   shorter than one PES packet of `padding_stream` asks for, or carries
    ///   stuffing alone, the bytes it cannot take left out as above.
    /// - The P-STD buffer size written for each stream is within the bound
    ///   of §2.7.9, which
    ///   [`Muxer::set_decoder_buffer_bound`](super::Muxer::set_decoder_buffer_bound)
    ///   declares where the `stream_id` and the `stream_type` of the stream
    ///   do not say it.
    pub csps: bool,
}

impl MuxerConfig {
    /// Returns the settings of `program_mux_rate` and `mux_delay`, and the
    /// default of each other setting
    #[must_use]
    pub const fn new(program_mux_rate: u32, mux_delay: Duration) -> Self {
        Self {
            program_mux_rate,
            pack_size: 2048,
            mux_delay,
            buffer_limit: 4 * 1024 * 1024,
            header_interval: Duration::MAX,
            fixed_rate: false,
            csps: false,
        }
    }

    /// Returns the value with the given `pack_size`
    #[must_use]
    pub const fn with_pack_size(mut self, pack_size: usize) -> Self {
        self.pack_size = pack_size;
        self
    }

    /// Returns the value with the given `buffer_limit`
    #[must_use]
    pub const fn with_buffer_limit(mut self, buffer_limit: usize) -> Self {
        self.buffer_limit = buffer_limit;
        self
    }

    /// Returns the value with the given `header_interval`
    #[must_use]
    pub const fn with_header_interval(mut self, header_interval: Duration) -> Self {
        self.header_interval = header_interval;
        self
    }

    /// Returns the value with the given `fixed_rate`
    #[must_use]
    pub const fn with_fixed_rate(mut self, fixed_rate: bool) -> Self {
        self.fixed_rate = fixed_rate;
        self
    }

    /// Returns the value with the given `csps`
    #[must_use]
    pub const fn with_csps(mut self, csps: bool) -> Self {
        self.csps = csps;
        self
    }
}
