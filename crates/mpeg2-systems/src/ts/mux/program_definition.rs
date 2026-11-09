//! The program registered with a [`Muxer`](super::Muxer), its streams, and
//! the PAT and the PMT made of them

use alloc::vec::Vec;
use core::num::NonZeroU16;

use super::Error;
use crate::psi::VersionNumber;
use crate::psi::pat::{Program, ProgramAssociationSection, ProgramLoop};
use crate::psi::pmt::{ElementaryStream, ElementaryStreamLoop, TsProgramMapSection};
use crate::stream_type::StreamType;
use crate::ts::Pid;

/// The program registered, and the streams registered with it
///
/// Program definition, ISO/IEC 13818-1 §2.4.4.8
#[derive(Debug, Clone)]
pub(super) struct ProgramDefinition {
    pub(super) program_number: NonZeroU16,
    pub(super) program_map_pid: Pid,
    /// The `pcr_pid` registered, `None` for the muxer to choose it
    // Why not the PCR PID chosen: it is chosen from the streams, which are
    // registered after the program.
    pub(super) pcr_pid: Option<Pid>,
    pub(super) program_info: Vec<u8>,
    pub(super) streams: Vec<Stream>,
}

/// A stream registered with the program
#[derive(Debug, Clone)]
pub(super) struct Stream {
    pub(super) pid: Pid,
    pub(super) stream_type: StreamType,
    pub(super) es_info: Vec<u8>,
}

impl ProgramDefinition {
    /// Returns the stream registered on `pid`
    pub(super) fn stream(&self, pid: Pid) -> Option<&Stream> {
        self.streams.iter().find(|stream| stream.pid == pid)
    }

    /// Returns the PID the PCR goes on: the `pcr_pid` registered, or else
    /// the first video stream, or else the first stream
    pub(super) fn pcr_pid(&self) -> Option<Pid> {
        self.pcr_pid.or_else(|| {
            self.streams
                .iter()
                .find(|stream| is_video(stream.stream_type))
                .or_else(|| self.streams.first())
                .map(|stream| stream.pid)
        })
    }

    /// Appends the program loop of the PAT and the elementary stream loop
    /// of the PMT to `scratch`, and returns the length of the program loop
    pub(super) fn write_loops(&self, scratch: &mut Vec<u8>) -> Result<usize, Error> {
        let start = scratch.len();
        ProgramLoop::write_from(
            [Program::ProgramMap {
                program_number: self.program_number,
                program_map_pid: self.program_map_pid,
            }],
            scratch,
        )
        .map_err(|error| Error::ProgramAssociationSection { error })?;
        let pat_loop_length = scratch.len().saturating_sub(start);
        ElementaryStreamLoop::write_from(
            self.streams.iter().map(|stream| {
                let mut entry = ElementaryStream::new(stream.stream_type, stream.pid);
                entry.es_info = stream.es_info.as_slice().into();
                entry
            }),
            scratch,
        )
        .map_err(|error| Error::TsProgramMapSection { error })?;
        Ok(pat_loop_length)
    }

    /// Returns the PAT of `transport_stream_id` and the PMT, `version_number`
    /// 0, their loops `pat_loop` and `pmt_loop` as
    /// [`write_loops`](Self::write_loops) writes them
    pub(super) fn sections<'loops>(
        &'loops self,
        transport_stream_id: u16,
        pat_loop: &'loops [u8],
        pmt_loop: &'loops [u8],
    ) -> (
        ProgramAssociationSection<'loops>,
        TsProgramMapSection<'loops>,
    ) {
        let version_number = VersionNumber(0);
        let mut pat = ProgramAssociationSection::new(transport_stream_id, version_number);
        pat.programs = ProgramLoop::from(pat_loop);
        let mut pmt = TsProgramMapSection::new(self.program_number.get(), version_number);
        pmt.pcr_pid = self.pcr_pid();
        pmt.program_info = self.program_info.as_slice().into();
        pmt.elementary_streams = ElementaryStreamLoop::from(pmt_loop);
        (pat, pmt)
    }

    /// Writes the PAT and the PMT, so that what their `write` refuses is
    /// refused on registration rather than on input
    pub(super) fn check(&self, transport_stream_id: u16) -> Result<(), Error> {
        let mut loops = Vec::new();
        let pat_loop_length = self.write_loops(&mut loops)?;
        let (pat_loop, pmt_loop) = loops.split_at(pat_loop_length);
        let (pat, pmt) = self.sections(transport_stream_id, pat_loop, pmt_loop);
        let mut out = Vec::new();
        pat.write(&mut out)
            .map_err(|error| Error::ProgramAssociationSection { error })?;
        pmt.write(&mut out)
            .map_err(|error| Error::TsProgramMapSection { error })
    }
}

/// Returns whether `stream_type` is the base video of a coding
const fn is_video(stream_type: StreamType) -> bool {
    matches!(
        stream_type,
        StreamType::ISO_IEC_11172_2_VIDEO
            | StreamType::ISO_IEC_13818_2_VIDEO
            | StreamType::ISO_IEC_14496_2_VISUAL
            | StreamType::AVC_VIDEO
            | StreamType::ISO_IEC_15444_1_VIDEO
            | StreamType::HEVC_VIDEO
            | StreamType::JPEG_XS_VIDEO
            | StreamType::VVC_VIDEO
            | StreamType::EVC_VIDEO
    )
}
