//! ARIB extensions to mpeg2-systems: the ARIB STD-B10 descriptors that lead
//! from a Program Map Table to ARIB STD-B24 captions, and the PES data that
//! carries them
//!
//! The crate builds on the public API of [`mpeg2_systems`] alone. Its
//! descriptors are converted from a [`RawDescriptor`] with `TryFrom`, as the
//! descriptors of ISO/IEC 13818-1 are, and report through the same
//! [`mpeg2_systems::descriptor::Error`], so one loop handles both. Its PES
//! data is converted from the [`PesPacket`] the demuxer reports, the same
//! way.
//!
//! [`RawDescriptor`]: mpeg2_systems::descriptor::RawDescriptor
//! [`PesPacket`]: mpeg2_systems::pes::PesPacket
//!
//! # Finding the captions of a program
//!
//! A caption elementary stream is a PES private data stream whose data
//! component descriptor names the caption coding. This section is the PMT
//! TSDuck's `tstabcomp` wrote for one such stream on PID 0x0130:
//!
//! ```
//! use mpeg2_systems::descriptor::Error;
//! use mpeg2_systems::psi::pmt::TsProgramMapSection;
//! use mpeg2_systems::psi::section::Section;
//! use mpeg2_systems::stream_type::StreamType;
//! use mpeg2_systems_arib::descriptor::data_component::{
//!     AdditionalAribCaptionInfo, DataComponentDescriptor, DataComponentId, Timing,
//! };
//!
//! let bytes = [
//!     0x02, 0xb0, 0x1a, 0x00, 0x01, 0xc1, 0x00, 0x00, 0xff, 0xff, 0xf0, 0x00, 0x06, 0xe1, 0x30,
//!     0xf0, 0x08, 0x52, 0x01, 0x30, 0xfd, 0x03, 0x00, 0x08, 0xad, 0xc6, 0x54, 0xbe, 0xdc,
//! ];
//! let section = Section::read(&bytes)?;
//! let pmt = TsProgramMapSection::try_from(&section)?;
//! let mut captions = Vec::new();
//! for stream in pmt.elementary_streams.iter() {
//!     let stream = stream?;
//!     if stream.stream_type != StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA {
//!         continue;
//!     }
//!     for raw in stream.es_info.iter() {
//!         let descriptor = match DataComponentDescriptor::try_from(&raw?) {
//!             Ok(descriptor) => descriptor,
//!             // A descriptor of another tag
//!             Err(Error::DescriptorTag { .. }) => continue,
//!             Err(error) => return Err(error.into()),
//!         };
//!         // The caption info is read only for the caption coding
//!         if descriptor.data_component_id == DataComponentId::CAPTION_AND_SUPERIMPOSE {
//!             let info = AdditionalAribCaptionInfo::read(descriptor.additional_data_component_info)?;
//!             captions.push((stream.elementary_pid.get(), info.dmf.get(), info.timing));
//!         }
//!     }
//! }
//! assert_eq!(captions, [(0x0130, 0b1010, Timing::PROGRAM_SYNCHRONOUS)]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Reading the data groups of a caption PES packet
//!
//! A PES packet of the caption elementary stream, such as the `packet` of
//! the demuxer's `Event::PesPacket` on its PID, carries synchronized PES
//! data, whose data is a data group. This packet, of caption management
//! data, was written independently of this crate and read by FFmpeg's ARIB
//! caption decoder:
//!
//! ```
//! use mpeg2_systems::pes::PesPacket;
//! use mpeg2_systems_arib::caption::data_group::{DataGroup, DataGroupId};
//! use mpeg2_systems_arib::independent_pes::SynchronizedPesData;
//!
//! let bytes = [
//!     0x00, 0x00, 0x01, 0xbd, 0x00, 0x1c, 0x80, 0x80, 0x05, 0x21, 0x00, 0x05, 0xbf, 0x21, 0x80,
//!     0xff, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x0a, 0x3f, 0x01, 0x1a, 0x6a, 0x70, 0x6e, 0x80, 0x00,
//!     0x00, 0x00, 0xb0, 0x78,
//! ];
//! let packet = PesPacket::read(&bytes)?;
//! let data = SynchronizedPesData::try_from(&packet)?;
//! let group = DataGroup::read(data.synchronized_pes_data_byte)?;
//! // Data group 0 carries caption management data
//! assert_eq!(group.data_group_id, DataGroupId::try_from(0)?);
//! assert_eq!(group.data_group_data_byte.len(), 10);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Reading the text of a caption
//!
//! A data group of a caption statement carries caption data, whose data
//! units hold the text in the 8-unit code, which the caller decodes. This
//! data group, of the statement あ, comes from the same independent source:
//!
//! ```
//! use mpeg2_systems_arib::caption::data_group::DataGroup;
//! use mpeg2_systems_arib::caption::data_unit::DataUnitParameter;
//! use mpeg2_systems_arib::caption::statement::CaptionData;
//!
//! let bytes = [
//!     0x04, 0x00, 0x00, 0x00, 0x0b, 0x3f, 0x00, 0x00, 0x07, 0x1f, 0x20, 0x00, 0x00, 0x02, 0x24,
//!     0x22, 0x05, 0x71,
//! ];
//! let group = DataGroup::read(&bytes)?;
//! let data = CaptionData::try_from(&group)?;
//! let mut text = Vec::new();
//! for unit in data.data_units.iter() {
//!     let unit = unit?;
//!     // Other data units carry DRCS patterns, bitmaps and the like
//!     if unit.data_unit_parameter == DataUnitParameter::STATEMENT_BODY {
//!         text.extend_from_slice(unit.data_unit_data_byte);
//!     }
//! }
//! assert_eq!(text, [0x24, 0x22]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What the crate does not do
//!
//! - It does not read the SI tables of ARIB STD-B10, such as the NIT, the
//!   SDT or the EIT.
//! - It does not decode the 8-unit code characters of ARIB STD-B24.
//! - It does not check the operational rules of ARIB TR-B14 and TR-B15.
//!
//! # Modules
//!
//! - [`descriptor`]: the ARIB STD-B10 descriptors
//! - [`independent_pes`]: the PES data of ARIB STD-B24 Volume 3
//! - [`caption`]: the syntax of ARIB STD-B24 captions
//!
//! # Features
//!
//! - `alloc` (default): the writers of the syntax types, and `alloc` of
//!   `mpeg2-systems`. Without it, the syntax types read with no allocator.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod caption;
pub mod descriptor;
pub mod independent_pes;
