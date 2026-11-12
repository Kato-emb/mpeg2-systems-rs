//! The settings of a [`Muxer`](super::Muxer)

use core::time::Duration;

/// Settings of a [`Muxer`](super::Muxer)
///
/// [`MuxerConfig::new`] gives the default of each setting; change the fields
/// of the value it returns.
///
/// ```
/// use core::time::Duration;
/// use mpeg2_systems::ts::mux::MuxerConfig;
///
/// let config = MuxerConfig::new()
///     .with_transport_stream_id(0x7fe0)
///     .with_pcr_interval(Duration::from_millis(30));
/// ```
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MuxerConfig {
    /// The `transport_stream_id` of the PAT (1 by default)
    pub transport_stream_id: u16,
    /// The time from one sending of the PAT and the PMT to the next
    /// (100 ms by default)
    ///
    /// Any value is taken, both ends included: 0 sends the PAT and the PMT
    /// before every PES packet, and [`Duration::MAX`] only before the first.
    /// The specification places no requirement on how often PSI sections
    /// are sent (ISO/IEC 13818-1 Annex C.5); what an operation asks of it,
    /// such as ETSI TR 101 290, is the caller's to choose.
    pub psi_interval: Duration,
    /// The time from one PCR to the next (40 ms by default)
    ///
    /// [`Muxer::new`](super::Muxer::new) takes from one tick of the 27 MHz system clock to
    /// 0.1 s (ISO/IEC 13818-1 §2.7.2, §2.4.2.3 equation 2-5).
    pub pcr_interval: Duration,
    /// Offset subtracted from a PES packet's DTS, or its PTS when it has no
    /// DTS, to obtain the desired PCR (0.7 s by default)
    ///
    /// The PCR does not go back, so a PES packet given after one of a later
    /// DTS on another stream may arrive after a later PCR.
    pub mux_delay: Duration,
    /// The most bytes the output held for [`Muxer::poll_output`](super::Muxer::poll_output) may grow to
    /// (4 MiB by default)
    ///
    /// An input that would grow the output held past it is refused, so the
    /// limit must be larger than the Transport Packets of the largest PES
    /// packet given, with the PAT, the PMT and a PCR before it.
    pub buffer_limit: usize,
}

impl MuxerConfig {
    /// Returns the default settings
    #[must_use]
    #[expect(
        clippy::new_without_default,
        reason = "the specification sets no default, which Default would claim"
    )]
    pub const fn new() -> Self {
        Self {
            transport_stream_id: 1,
            psi_interval: Duration::from_millis(100),
            pcr_interval: Duration::from_millis(40),
            mux_delay: Duration::from_millis(700),
            buffer_limit: 4 * 1024 * 1024,
        }
    }

    /// Returns the value with the given `transport_stream_id`
    #[must_use]
    pub const fn with_transport_stream_id(mut self, transport_stream_id: u16) -> Self {
        self.transport_stream_id = transport_stream_id;
        self
    }

    /// Returns the value with the given `psi_interval`
    #[must_use]
    pub const fn with_psi_interval(mut self, psi_interval: Duration) -> Self {
        self.psi_interval = psi_interval;
        self
    }

    /// Returns the value with the given `pcr_interval`
    #[must_use]
    pub const fn with_pcr_interval(mut self, pcr_interval: Duration) -> Self {
        self.pcr_interval = pcr_interval;
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
}

#[cfg(test)]
mod tests {
    use core::time::Duration;

    use super::MuxerConfig;

    #[test]
    fn a_config_built_from_new_and_every_with_method_equals_one_with_every_field_assigned() {
        let mut assigned = MuxerConfig::new();
        assigned.transport_stream_id = 0x7fe0;
        assigned.psi_interval = Duration::from_millis(50);
        assigned.pcr_interval = Duration::from_millis(30);
        assigned.mux_delay = Duration::from_millis(500);
        assigned.buffer_limit = 1024;

        let built = MuxerConfig::new()
            .with_transport_stream_id(0x7fe0)
            .with_psi_interval(Duration::from_millis(50))
            .with_pcr_interval(Duration::from_millis(30))
            .with_mux_delay(Duration::from_millis(500))
            .with_buffer_limit(1024);

        assert_eq!(built, assigned);
    }
}
