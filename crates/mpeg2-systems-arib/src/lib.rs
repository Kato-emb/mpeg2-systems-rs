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
//! # Reading the captions of a stream
//!
//! This reads the fixture `cargo xtask fixtures --arib` made: TSDuck's PAT
//! and PMT for a caption elementary stream on PID 0x0130, and its caption
//! management data and two caption statements. It finds the caption stream
//! in the PMT, reads each of its PES packets down to its data units, and
//! collects the characters of the statements, which it leaves undecoded.
//!
//! ```
//! # #[cfg(feature = "alloc")]
//! # {
//! use mpeg2_systems::descriptor::Error;
//! use mpeg2_systems::stream_type::StreamType;
//! use mpeg2_systems::ts::demux::{Demuxer, DemuxerConfig, Event};
//! use mpeg2_systems_arib::caption::Dmf;
//! use mpeg2_systems_arib::caption::data_group::DataGroup;
//! use mpeg2_systems_arib::caption::data_unit::DataUnitParameter;
//! use mpeg2_systems_arib::caption::management::CaptionManagementData;
//! use mpeg2_systems_arib::caption::statement::CaptionData;
//! use mpeg2_systems_arib::descriptor::data_component::{
//!     AdditionalAribCaptionInfo, DataComponentDescriptor, DataComponentId, Timing,
//! };
//! use mpeg2_systems_arib::independent_pes::SynchronizedPesData;
//!
//! let stream = std::fs::read("tests/fixtures/caption.ts")?;
//! let mut demuxer = Demuxer::new(DemuxerConfig::default());
//! let (mut captions, mut languages, mut texts) = (Vec::new(), Vec::new(), Vec::new());
//! for packet in stream.chunks(188) {
//!     demuxer.handle_input(packet);
//!     let mut subscribe = Vec::new();
//!     while let Some(event) = demuxer.poll_event() {
//!         // A caption stream: PES private data whose data component is captions
//!         if let Event::TsProgramMapSection { section, .. } = &event {
//!             for stream in section.elementary_streams.iter() {
//!                 let stream = stream?;
//!                 if stream.stream_type != StreamType::ISO_IEC_13818_1_PES_PRIVATE_DATA {
//!                     continue;
//!                 }
//!                 for raw in stream.es_info.iter() {
//!                     let descriptor = match DataComponentDescriptor::try_from(&raw?) {
//!                         Ok(descriptor) => descriptor,
//!                         Err(Error::DescriptorTag { .. }) => continue,
//!                         Err(error) => return Err(error.into()),
//!                     };
//!                     if descriptor.data_component_id == DataComponentId::CAPTION_AND_SUPERIMPOSE {
//!                         let info = AdditionalAribCaptionInfo::read(descriptor.additional_data_component_info)?;
//!                         captions.push((stream.elementary_pid.get(), info));
//!                         subscribe.push(stream.elementary_pid);
//!                     }
//!                 }
//!             }
//!         }
//!         // Its PES packets: a data group each, of management or of a statement
//!         if let Event::PesPacket { packet, .. } = &event {
//!             let data = SynchronizedPesData::try_from(packet)?;
//!             let group = DataGroup::read(data.synchronized_pes_data_byte)?;
//!             if matches!(group.data_group_id.get(), 0x00 | 0x20) {
//!                 for language in CaptionManagementData::try_from(&group)?.languages.iter() {
//!                     languages.push(language?.iso_639_language_code);
//!                 }
//!             } else {
//!                 for unit in CaptionData::try_from(&group)?.data_units.iter() {
//!                     let unit = unit?;
//!                     // The 8-unit code characters, which the caller decodes
//!                     if unit.data_unit_parameter == DataUnitParameter::STATEMENT_BODY {
//!                         texts.push(unit.data_unit_data_byte.to_vec());
//!                     }
//!                 }
//!             }
//!         }
//!     }
//!     // Subscribe once the events, which borrow the demuxer, are taken
//!     for pid in subscribe {
//!         demuxer.subscribe_pes(pid);
//!     }
//! }
//! demuxer.finish();
//! while demuxer.poll_event().is_some() {}
//! let info = AdditionalAribCaptionInfo::new(Dmf::try_from(0b0011)?, Timing::PROGRAM_SYNCHRONOUS);
//! assert_eq!(captions, [(0x0130, info)]);
//! assert_eq!(languages, [*b"jpn"]);
//! assert_eq!(texts, [[0x24, 0x22], [0x24, 0x24]]);
//! # }
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
//! - [`descriptor`]: the ARIB STD-B10 descriptors,
//!   [`stream_identifier`](descriptor::stream_identifier) and
//!   [`data_component`](descriptor::data_component), the latter with the
//!   caption information it carries
//! - [`independent_pes`]: the synchronized and asynchronous PES data of ARIB
//!   STD-B24 Volume 3
//! - [`caption`]: the syntax of ARIB STD-B24 captions: the
//!   [`data_group`](caption::data_group), the caption
//!   [`management`](caption::management) and [`statement`](caption::statement)
//!   data, their [`data_unit`](caption::data_unit)s, and the display mode and
//!   time control they share
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
