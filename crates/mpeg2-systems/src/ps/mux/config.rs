//! The settings of a [`Muxer`](super::Muxer)

use core::time::Duration;

/// Settings of a [`Muxer`](super::Muxer)
///
/// [`MuxerConfig::new`] gives the default of each setting but the
/// multiplex rate; change the fields of the value it returns.
///
/// ```
/// use core::time::Duration;
/// use mpeg2_systems::ps::mux::MuxerConfig;
///
/// // 1 260 000 bytes a second
/// let config = MuxerConfig::new(25_200).with_mux_delay(Duration::from_millis(500));
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
    /// Offset subtracted from a PES packet's DTS, or its PTS when it has no
    /// DTS, to obtain the desired SCR (0.7 s by default)
    ///
    /// Delivery of preceding packs may require a later SCR. Any value is
    /// taken, 0 included: the SCR of the first pack is then the DTS of its
    /// PES packet.
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
    /// less `mux_delay`, packs of padding fill the time before it, each at
    /// most [`pack_size`](Self::pack_size) bytes, with `stuffing_byte`s where
    /// 7 or fewer fill it (§2.5.3.4) and a PES packet of `padding_stream`
    /// past them (§2.4.3.6). The system header names no bound for
    /// `padding_stream`.
    pub fixed_rate: bool,
}

impl MuxerConfig {
    /// Returns the default settings, at the `program_mux_rate` given
    #[must_use]
    pub const fn new(program_mux_rate: u32) -> Self {
        Self {
            program_mux_rate,
            pack_size: 2048,
            mux_delay: Duration::from_millis(700),
            buffer_limit: 4 * 1024 * 1024,
            header_interval: Duration::MAX,
            fixed_rate: false,
        }
    }

    /// Returns the value with the given `pack_size`
    #[must_use]
    pub const fn with_pack_size(mut self, pack_size: usize) -> Self {
        self.pack_size = pack_size;
        self
    }

    /// Returns the value with the given `mux_delay`
    #[must_use]
    pub const fn with_mux_delay(mut self, mux_delay: Duration) -> Self {
        self.mux_delay = mux_delay;
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
}
