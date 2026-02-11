//! VGM streaming utilities.
//!
//! This module implements a low-memory, iterator-based parser that consumes VGM
//! bytes (or a pre-parsed document) and yields VGM commands one at a time.
//!
//! High-level responsibilities:
//! - parsing VGM commands incrementally from a byte stream
//! - handling DAC Stream Control (SetupStreamControl / Start/Stop / FastCall)
//! - expanding Wait commands with interleaved stream-generated chip writes
//! - storing and decompressing data blocks used by DAC streams
//!
//! See the `VgmStream` type below for usage examples and more detailed docs.
//!
use crate::VgmDocument;
use crate::binutil::ParseError;
use crate::vgm::command::{DacStreamChipType, DataBlock, VgmCommand, WaitSamples};
use crate::vgm::detail::{
    BitPackingSubType, CompressedStream, CompressedStreamData, DataBlockType, DecompressionTable,
    UncompressedStream, parse_data_block,
};
use crate::vgm::header::VgmHeader;
use crate::vgm::parser::parse_vgm_command;
use std::collections::HashMap;

/// Internal source of VGM commands for the stream processor.
///
/// The stream processor can work with either raw byte streams that need parsing,
/// or pre-parsed command streams from a VgmDocument.
#[derive(Debug)]
enum VgmStreamSource {
    /// Raw byte stream that needs to be parsed into commands.
    Bytes {
        /// Buffer containing incomplete or unparsed VGM data.
        buffer: Vec<u8>,
        /// VGM header (parsed upfront to know loop offset, etc.)
        header: Option<Box<VgmHeader>>,
    },
    /// Pre-parsed commands from a VgmDocument.
    Commands {
        /// Reference to the original document (boxed to avoid large enum variant size)
        document: Box<VgmDocument>,
        /// Current command index
        current_index: usize,
        /// Loop point command index (None if no loop)
        loop_index: Option<usize>,
    },
}

/// DAC stream chip type mapping (see `DacStreamChipType` in `vgm::command`).
///
/// The `DacStreamChipType` enum (defined in `crate::vgm::command`) defines the
/// mapping between `chip_type` values used in `SetupStreamControl` commands
/// (0x90) and the actual chip hardware. When serialized in VGM files, the byte
/// format is:
/// - Bit 7: Instance flag (0 = Primary, 1 = Secondary)
/// - Bits 6..0: Chip ID (values defined by `DacStreamChipType`)
///
/// Note: These values correspond to the order of chips in the VGM header and
/// are distinct from the VGM command opcodes used for chip write commands.
/// For example, the stream chip ID for the YM2151 is 0x03, while YM2151 write
/// commands in the VGM command stream use opcode 0x54.
///
/// State of a DAC stream for stream control commands.
///
/// This structure tracks the complete state of a VGM DAC stream, which converts
/// PCM data from data blocks into chip register writes at specific sample rates.
/// The stream state machine is controlled by VGM stream control commands (0x90-0x95).
#[derive(Debug, Clone)]
struct StreamState {
    /// Stream ID (kept for debugging, prefixed with _ to avoid unused warning)
    _stream_id: u8,
    /// Chip type to write to (determines which chip write command to generate)
    chip_type: u8,
    /// Port/register to write to (pp in VGM spec)
    write_port: u8,
    /// Write command/register (cc in VGM spec)
    write_command: u8,
    /// Data bank ID (references a data block by its data_type)
    data_bank_id: u8,
    /// Step size for reading data (bytes to advance after each read)
    step_size: u8,
    /// Step base (initial offset adjustment added to start_offset)
    step_base: u8,
    /// Stream frequency in Hz (determines write rate)
    frequency_hz: Option<u32>,
    /// Start offset in the data block
    start_offset: Option<i32>,
    /// Length mode (0=ignore, 1=count commands, 2=milliseconds, 3=play until end)
    length_mode: u8,
    /// Data length (interpretation depends on length_mode)
    data_length: u32,
    /// End position for the current block (used with FastCall and length_mode 3)
    block_end_pos: Option<usize>,
    /// Whether the stream is currently active/playing
    active: bool,
    /// Current read position in the data block (byte offset)
    current_data_pos: usize,
    /// Next sample number when a write should occur (at 44100 Hz sample rate)
    next_write_sample: u64,
    /// Fractional accumulator for sample interval (tracks fractional part to avoid rounding errors)
    sample_fraction: f32,
    /// Remaining commands to write (used when length_mode == 1)
    remaining_commands: Option<u32>,
}

/// Result type for stream parsing operations.
/// Default maximum size for accumulated data blocks (32 MiB).
const DEFAULT_MAX_DATA_BLOCK_SIZE: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum StreamResult {
    /// A complete command was parsed successfully.
    Command(VgmCommand),
    /// More data is needed to complete the current command.
    NeedsMoreData,
    /// The VGM stream has ended.
    EndOfStream,
}

/// Memory-efficient streaming VGM parser.
///
/// `VgmStream` is the primary entry point for incremental processing of VGM data.
/// It accepts either raw VGM bytes (fed via `push_data`) or a pre-parsed
/// `VgmDocument` (via `from_document`) and yields `VgmCommand` values through
/// its iterator interface.
///
/// Key behaviors:
/// - Produces parsed commands as soon as they are available, minimizing buffering.
/// - Automatically handles DAC stream control: it configures streams, reads
///   stream data blocks, schedules writes according to stream frequency, and
///   interleaves generated chip writes with parsed commands during Wait periods.
///
/// ## Data block storage and limits
///
/// The stream parser stores and accumulates data blocks (for example, PCM or
/// DAC stream data) while parsing. To avoid unbounded memory growth there is
/// a configurable maximum total size for accumulated data blocks. By default
/// this limit is 32 MiB. You can change the limit at runtime via
/// `VgmStream::set_max_data_block_size(max_bytes)`. When adding a data block
/// would cause the accumulated total to exceed the configured limit, the
/// parser will return `ParseError::DataBlockSizeExceeded` from the iterator.
/// Use `max_data_block_size()` and `total_data_block_size()` to query the
/// configured limit and the current accumulated size respectively.
///
/// # Examples
///
/// Basic usage:
/// ```
/// use soundlog::vgm::VgmStream;
/// use soundlog::vgm::command::WaitSamples;
/// use soundlog::vgm::stream::StreamResult;
///
/// let mut parser = VgmStream::new();
/// parser.set_loop_count(Some(2));
///
/// // Feed VGM data (could be incoming chunks)
/// let vgm_data = vec![0x62, 0x63, 0x66];
/// parser.push_chunk(&vgm_data);
///
/// for result in &mut parser {
///     match result {
///         Ok(StreamResult::Command(cmd)) => {
///             // handle the command
///         }
///         Ok(StreamResult::NeedsMoreData) => break,
///         Ok(StreamResult::EndOfStream) => break,
///         Err(e) => break,
///     }
/// }
/// ```
///
/// From a parsed `VgmDocument` (including stream control commands):
/// ```
/// use soundlog::{VgmBuilder, vgm::stream::VgmStream};
/// use soundlog::vgm::stream::StreamResult;
/// use soundlog::vgm::command::{
///     WaitSamples, DataBlock, SetupStreamControl, SetStreamData, SetStreamFrequency, StartStream,
/// };
///
/// // Build a VGM document that contains:
/// // - a DataBlock with an uncompressed PCM stream (data_type 0x00)
/// // - stream control commands to route that bank to a YM2612 stream (chip id 0x02)
/// // - a StartStream and a Wait to allow generated writes to occur
/// let mut builder = VgmBuilder::new();
///
/// // Simple uncompressed stream data block (type 0x00 = YM2612 PCM)
/// let block = DataBlock {
///     marker: 0x66,
///     chip_instance: 0,
///     data_type: 0x00, // data bank id
///     size: 4,
///     data: vec![0x10, 0x20, 0x30, 0x40],
/// };
/// builder.add_vgm_command(block);
///
/// // Configure stream 0 to write to YM2612 (chip id 0x02), register 0x2A
/// builder.add_vgm_command(SetupStreamControl {
///     stream_id: 0,
///     chip_type: 0x02, // YM2612 (DacStreamChipType::Ym2612)
///     write_port: 0,
///     write_command: 0x2A,
/// });
///
/// // Point stream 0 at data bank 0x00 with step size 1
/// builder.add_vgm_command(SetStreamData {
///     stream_id: 0,
///     data_bank_id: 0x00,
///     step_size: 1,
///     step_base: 0,
/// });
///
/// // Set a stream frequency so writes will be generated
/// builder.add_vgm_command(SetStreamFrequency {
///     stream_id: 0,
///     frequency: 22050, // Hz
/// });
///
/// // Start the stream (length_mode 3 = play until end of block)
/// builder.add_vgm_command(StartStream {
///     stream_id: 0,
///     data_start_offset: 0,
///     length_mode: 3,
///     data_length: 0,
/// });
///
/// // Wait long enough for the stream to generate writes
/// builder.add_vgm_command(WaitSamples(100));
///
/// let doc = builder.finalize();
///
/// // Create a stream processor from the parsed document and iterate results.
/// // The iterator will yield the original commands as well as automatically
/// // generated chip write commands produced by the active stream during Waits.
/// let mut stream = VgmStream::from_document(doc);
/// for item in &mut stream {
///     match item {
///         Ok(StreamResult::Command(cmd)) => {
///             // `cmd` may be a parsed command (DataBlock/Setup/Start/Wait) or a
///             // generated chip write (e.g. `VgmCommand::Ym2612Write`).
///             println!("command: {:?}", cmd);
///         }
///         Ok(StreamResult::NeedsMoreData) => break,
///         Ok(StreamResult::EndOfStream) => break,
///         Err(e) => { eprintln!("error: {}", e); break; }
///     }
/// }
/// ```
///
/// Streaming from multiple chunks:
/// ```
/// use soundlog::vgm::VgmStream;
/// use soundlog::vgm::stream::StreamResult;
///
/// let mut parser = VgmStream::new();
/// let chunks = vec![vec![0x61, 0x44], vec![0x01], vec![0x62, 0x63]];
///
/// for chunk in chunks {
///     parser.push_chunk(&chunk);
///     for result in &mut parser {
///         match result {
///             Ok(StreamResult::Command(_)) => {},
///             Ok(StreamResult::NeedsMoreData) => break,
///             Ok(StreamResult::EndOfStream) => break,
///             Err(_) => break,
///         }
///     }
/// }
/// ```
#[derive(Debug)]
pub struct VgmStream {
    /// Internal source of VGM commands (either bytes or pre-parsed commands)
    source: VgmStreamSource,
    /// Uncompressed streams stored by data type
    uncompressed_streams: HashMap<u8, UncompressedStream>,
    /// Block ID to (data_type, offset within that type's concatenated stream, block_size) mapping
    /// Maps global DataBlock sequence number to its location and size in the concatenated stream
    block_id_map: Vec<(u8, usize, usize)>,
    /// Cumulative size of DataBlocks by data_type (for offset calculation)
    /// Used to track offsets when blocks are not stored (e.g., RomRamDump types)
    block_sizes: HashMap<u8, usize>,
    /// Decompression tables stored by data type
    decompression_tables: HashMap<u8, DecompressionTable>,
    /// Loop count limit (None = infinite)
    loop_count: Option<u32>,
    /// Current loop iteration
    current_loops: u32,
    /// Whether we've encountered the end of data command
    encountered_end: bool,
    /// Byte offset where loop should occur (for Bytes source)
    loop_byte_offset: Option<usize>,
    /// Pending data block to return to iterator (for non-stream/table blocks)
    pending_data_block: Option<DataBlock>,
    /// DAC stream states indexed by stream ID
    stream_states: HashMap<u8, StreamState>,
    /// Current sample position (at 44100 Hz)
    current_sample: u64,
    /// Pending stream write commands to emit
    pending_stream_writes: Vec<VgmCommand>,
    /// Pending wait time that hasn't been emitted yet (in samples)
    pending_wait: Option<u16>,
    /// Fadeout grace period in samples after loop end (None = no fadeout)
    fadeout_samples: Option<u64>,
    /// Sample position when the loop ended (for fadeout tracking)
    loop_end_sample: Option<u64>,
    /// Current read offset in PCM data bank (type 0x00) for 0x8n commands
    pcm_data_offset: usize,
    /// Maximum allowed total size for accumulated data blocks
    max_data_block_size: usize,
    /// Current total size of accumulated data blocks
    total_data_block_size: usize,
}

impl VgmStream {
    /// Creates a new VGM stream parser.
    ///
    /// This constructor creates a parser that expects raw VGM bytes to be fed
    /// incrementally. When using a `VgmStream` created with `new()`, you must
    /// supply the VGM data by calling `push_chunk(&[u8])` to add raw VGM bytes
    /// (for example, as file or network chunks arrive). If you already have a
    /// parsed `VgmDocument`, prefer `VgmStream::from_document(...)` which is
    /// more efficient and avoids re-serializing/parsing the document.
    ///
    /// # Examples
    /// ```
    /// use soundlog::vgm::VgmStream;
    /// use soundlog::vgm::stream::StreamResult;
    ///
    /// // Create a parser that accepts raw bytes and feed it a small chunk.
    /// let mut parser = VgmStream::new();
    /// parser.set_loop_count(Some(1));
    ///
    /// // Push raw VGM bytes (header + a few commands); in real usage these
    /// // would typically come from a file or network in chunks.
    /// let chunk: &[u8] = &[0x56, 0x67, 0x6D, 0x20]; // partial/example bytes
    /// parser.push_chunk(chunk);
    ///
    /// // Iterate parsed commands as they become available.
    /// for item in &mut parser {
    ///     match item {
    ///         Ok(StreamResult::Command(cmd)) => {
    ///             // handle parsed or generated command
    ///         }
    ///         Ok(StreamResult::NeedsMoreData) => break,
    ///         Ok(StreamResult::EndOfStream) => break,
    ///         Err(_) => break,
    ///     }
    /// }
    /// ```
    pub fn new() -> Self {
        Self {
            source: VgmStreamSource::Bytes {
                buffer: Vec::with_capacity(16),
                header: None,
            },
            uncompressed_streams: HashMap::new(),
            block_id_map: Vec::new(),
            block_sizes: HashMap::new(),
            decompression_tables: HashMap::new(),
            loop_count: None,
            current_loops: 0,
            encountered_end: false,
            loop_byte_offset: None,
            pending_data_block: None,
            stream_states: HashMap::new(),
            current_sample: 0,
            pending_stream_writes: Vec::new(),
            pending_wait: None,
            fadeout_samples: None,
            loop_end_sample: None,
            pcm_data_offset: 0,
            max_data_block_size: DEFAULT_MAX_DATA_BLOCK_SIZE,
            total_data_block_size: 0,
        }
    }

    /// Creates a new VGM stream processor from a parsed VgmDocument.
    ///
    /// This is more efficient than serializing and re-parsing when you already
    /// have a parsed document.
    ///
    /// # Arguments
    /// * `doc` - A parsed VGM document
    ///
    /// # Examples
    /// ```
    /// use soundlog::{VgmBuilder, vgm::stream::VgmStream};
    /// use soundlog::vgm::command::WaitSamples;
    ///
    /// let mut builder = VgmBuilder::new();
    /// builder.add_vgm_command(WaitSamples(100));
    /// let doc = builder.finalize();
    ///
    /// let stream = VgmStream::from_document(doc);
    /// ```
    /// Creates a new VGM stream parser from an existing document.
    pub fn from_document(document: VgmDocument) -> Self {
        let loop_index = Self::calculate_loop_index(&document);
        Self {
            source: VgmStreamSource::Commands {
                document: Box::new(document),
                current_index: 0,
                loop_index,
            },
            uncompressed_streams: HashMap::new(),
            block_id_map: Vec::new(),
            block_sizes: HashMap::new(),
            decompression_tables: HashMap::new(),
            loop_count: None,
            current_loops: 0,
            encountered_end: false,
            loop_byte_offset: None,
            pending_data_block: None,
            stream_states: HashMap::new(),
            current_sample: 0,
            pending_stream_writes: Vec::new(),
            pending_wait: None,
            fadeout_samples: None,
            loop_end_sample: None,
            pcm_data_offset: 0,
            max_data_block_size: DEFAULT_MAX_DATA_BLOCK_SIZE,
            total_data_block_size: 0,
        }
    }

    /// Calculates the command index corresponding to loop_offset in the header.
    fn calculate_loop_index(doc: &VgmDocument) -> Option<usize> {
        if doc.header.loop_offset == 0 {
            return None;
        }

        let data_offset = if doc.header.data_offset == 0 {
            use crate::vgm::header::VgmHeader;
            (VgmHeader::fallback_header_size_for_version(doc.header.version) - 0x34) as u32
        } else {
            doc.header.data_offset
        };

        let mut header_len = doc.header.to_bytes(0, data_offset).len() as u32;
        if let Some(ref extra) = doc.extra_header {
            header_len += extra.to_bytes().len() as u32;
        }
        let loop_abs_offset = 0x1C_u32.wrapping_add(doc.header.loop_offset);
        let loop_command_offset = loop_abs_offset.wrapping_sub(header_len);

        let offsets = doc.command_offsets_and_lengths();
        for (index, (cmd_offset, _len)) in offsets.iter().enumerate() {
            if *cmd_offset as u32 == loop_command_offset {
                return Some(index);
            }
        }
        None
    }

    /// Adds new data to the internal buffer for parsing.
    ///
    /// # Arguments
    /// * `chunk` - Raw VGM bytes to add to the parsing buffer
    pub fn push_chunk(&mut self, chunk: &[u8]) {
        match &mut self.source {
            VgmStreamSource::Bytes { buffer, header } => {
                buffer.extend_from_slice(chunk);
                if header.is_none()
                    && buffer.len() >= 0x40
                    && let Ok((parsed_header, _size)) = crate::vgm::parser::parse_vgm_header(buffer)
                {
                    *header = Some(Box::new(parsed_header));
                }
            }
            VgmStreamSource::Commands { .. } => {
                panic!("push_data() cannot be called on a VgmStream created from a document");
            }
        }
    }

    /// Attempts to parse the next complete command from the buffer.
    ///
    /// Returns `StreamResult::Command` if a complete command was parsed,
    /// `StreamResult::NeedsMoreData` if more bytes are required, or
    /// `StreamResult::EndOfStream` if the stream has ended.
    fn next_command(&mut self) -> Result<StreamResult, ParseError> {
        if !self.pending_stream_writes.is_empty() {
            let cmd = self.pending_stream_writes.remove(0);
            return Ok(StreamResult::Command(cmd));
        }

        if let Some(wait_samples) = self.pending_wait.take() {
            return self.process_wait_with_streams(wait_samples as u64);
        }

        if let Some(block) = self.pending_data_block.take() {
            return Ok(StreamResult::Command(VgmCommand::DataBlock(block)));
        }

        if self.encountered_end {
            if let (Some(fadeout_samples), Some(loop_end_sample)) =
                (self.fadeout_samples, self.loop_end_sample)
            {
                if self.current_sample >= loop_end_sample + fadeout_samples {
                    return Ok(StreamResult::EndOfStream);
                }
                let command = match self.get_next_raw_command()? {
                    Some(cmd) => cmd,
                    None => {
                        // No more data, generate a wait to advance to end of fadeout
                        let remaining = (loop_end_sample + fadeout_samples) - self.current_sample;
                        let wait_amount = remaining.min(u16::MAX as u64) as u16;
                        VgmCommand::WaitSamples(WaitSamples(wait_amount))
                    }
                };
                return self.process_command(command);
            } else {
                return Ok(StreamResult::EndOfStream);
            }
        }

        let command = match self.get_next_raw_command()? {
            Some(cmd) => cmd,
            None => return Ok(StreamResult::NeedsMoreData),
        };

        self.process_command(command)
    }

    /// Gets the next raw command from the internal source.
    fn get_next_raw_command(&mut self) -> Result<Option<VgmCommand>, ParseError> {
        match &mut self.source {
            VgmStreamSource::Bytes { buffer, .. } => {
                // If buffer is empty, we need more data
                if buffer.is_empty() {
                    return Ok(None);
                }

                let parse_result = parse_vgm_command(buffer, 0);

                match parse_result {
                    Ok((command, consumed)) => {
                        // Remove consumed bytes from buffer
                        buffer.drain(..consumed);
                        self.shrink_buffer_if_needed();
                        Ok(Some(command))
                    }
                    Err(ParseError::UnexpectedEof) | Err(ParseError::OffsetOutOfRange { .. }) => {
                        // Not enough data to complete the command
                        Ok(None)
                    }
                    Err(e) => Err(e),
                }
            }
            VgmStreamSource::Commands {
                document,
                current_index,
                ..
            } => {
                let doc_ref: &VgmDocument = document.as_ref();
                if *current_index < doc_ref.commands.len() {
                    let cmd = doc_ref.commands[*current_index].clone();
                    *current_index += 1;
                    Ok(Some(cmd))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Processes a single VGM command, handling special cases and generating stream writes.
    fn process_command(&mut self, command: VgmCommand) -> Result<StreamResult, ParseError> {
        match &command {
            VgmCommand::EndOfData(_) => {
                self.handle_end_of_data();
                return self.next_command();
            }
            VgmCommand::DataBlock(block) => {
                return self.handle_data_block(block.clone());
            }
            VgmCommand::SetupStreamControl(setup) => {
                self.handle_setup_stream_control(setup);
                return self.next_command();
            }
            VgmCommand::SetStreamData(data) => {
                self.handle_set_stream_data(data);
                return self.next_command();
            }
            VgmCommand::SetStreamFrequency(freq) => {
                self.handle_set_stream_frequency(freq);
                return self.next_command();
            }
            VgmCommand::StartStream(start) => {
                self.handle_start_stream(start)?;
                return self.next_command();
            }
            VgmCommand::StopStream(stop) => {
                self.handle_stop_stream(stop);
                return self.next_command();
            }
            VgmCommand::StartStreamFastCall(fast) => {
                self.handle_start_stream_fast_call(fast)?;
                return self.next_command();
            }
            VgmCommand::WaitSamples(w) => {
                return self.process_wait_with_streams(w.0 as u64);
            }
            VgmCommand::Wait735Samples(_) => {
                return self.process_wait_with_streams(735);
            }
            VgmCommand::Wait882Samples(_) => {
                return self.process_wait_with_streams(882);
            }
            VgmCommand::WaitNSample(w) => {
                let samples = w.0 as u64;
                return self.process_wait_with_streams(samples);
            }
            VgmCommand::YM2612Port0Address2AWriteAndWaitN(cmd) => {
                return self.handle_ym2612_port0_address_2a_write_and_wait_n(cmd);
            }
            VgmCommand::SeekOffset(seek_offset) => {
                self.pcm_data_offset = seek_offset.0 as usize;
                return self.next_command();
            }
            _ => {}
        }

        Ok(StreamResult::Command(command))
    }

    fn handle_ym2612_port0_address_2a_write_and_wait_n(
        &mut self,
        cmd: &crate::vgm::command::Ym2612Port0Address2AWriteAndWaitN,
    ) -> Result<StreamResult, ParseError> {
        let wait_samples = cmd.0 as u64;

        if let Some(data_byte) = self.read_pcm_data_bank_byte()? {
            let dac_write = VgmCommand::Ym2612Write(
                crate::vgm::command::Instance::Primary,
                crate::chip::Ym2612Spec {
                    port: 0,
                    register: 0x2A,
                    value: data_byte,
                },
            );

            self.pcm_data_offset += 1;

            if wait_samples > 0 {
                // Emit the DAC write first
                self.pending_stream_writes.insert(0, dac_write);
                self.process_wait_with_streams(wait_samples)
            } else {
                Ok(StreamResult::Command(dac_write))
            }
        } else if wait_samples > 0 {
            self.process_wait_with_streams(wait_samples)
        } else {
            self.next_command()
        }
    }

    /// Sets the loop count limit.
    ///
    /// # Arguments
    /// * `count` - Maximum number of loops to process (None for infinite)
    pub fn set_loop_count(&mut self, count: Option<u32>) {
        self.loop_count = count;
    }

    /// Gets the current loop iteration count.
    pub fn current_loop_count(&self) -> u32 {
        self.current_loops
    }

    /// Sets the fadeout grace period in samples after loop end.
    ///
    /// When set, the stream will continue processing commands for the specified
    /// number of samples after reaching the loop end, allowing for fadeout effects.
    /// This is measured at 44100 Hz sample rate.
    ///
    /// # Arguments
    /// * `samples` - Number of samples to continue after loop end (None to disable)
    ///
    /// # Examples
    /// ```
    /// use soundlog::vgm::stream::VgmStream;
    ///
    /// let mut stream = VgmStream::new();
    /// stream.set_loop_count(Some(2));
    /// stream.set_fadeout_samples(Some(44100)); // 1 second fadeout at 44.1kHz
    /// ```
    /// Sets the fadeout grace period in samples (at 44.1 kHz).
    pub fn set_fadeout_samples(&mut self, samples: Option<u64>) {
        self.fadeout_samples = samples;
    }

    /// Gets the current fadeout grace period.
    pub fn fadeout_samples(&self) -> Option<u64> {
        self.fadeout_samples
    }

    /// Sets the maximum allowed size for accumulated data blocks.
    ///
    /// When data blocks are added that would exceed this limit, a
    /// `ParseError::DataBlockSizeExceeded` error will be returned.
    ///
    /// # Arguments
    ///
    /// * `max_size` - Maximum size in bytes (default is 32 MiB)
    pub fn set_max_data_block_size(&mut self, max_size: usize) {
        self.max_data_block_size = max_size;
    }

    /// Gets the maximum allowed size for accumulated data blocks.
    pub fn max_data_block_size(&self) -> usize {
        self.max_data_block_size
    }

    /// Gets the current total size of accumulated data blocks.
    pub fn total_data_block_size(&self) -> usize {
        self.total_data_block_size
    }

    /// Shrinks the buffer if it has grown too large relative to its usage.
    fn shrink_buffer_if_needed(&mut self) {
        if let VgmStreamSource::Bytes { buffer, .. } = &mut self.source
            && buffer.capacity() > 1024
            && buffer.len() < buffer.capacity() / 4
        {
            buffer.shrink_to_fit();
        }
    }

    /// Returns the current size of the internal buffer.
    #[doc(hidden)]
    pub fn buffer_size(&self) -> usize {
        match &self.source {
            VgmStreamSource::Bytes { buffer, .. } => buffer.len(),
            VgmStreamSource::Commands { .. } => 0,
        }
    }

    /// Optimizes memory usage by cleaning up unused resources.
    #[doc(hidden)]
    pub fn optimize_memory(&mut self) {
        self.cleanup_unused_data_blocks();
        if let VgmStreamSource::Bytes { buffer, .. } = &mut self.source {
            buffer.shrink_to_fit();
        }
    }

    /// Resets the parser state, clearing all buffers and data blocks.
    /// Resets the stream parser to its initial state.
    pub fn reset(&mut self) {
        match &mut self.source {
            VgmStreamSource::Bytes { buffer, header } => {
                buffer.clear();
                *header = None;
            }
            VgmStreamSource::Commands {
                current_index,
                loop_index: _,
                document: _,
            } => {
                *current_index = 0;
            }
        }
        self.uncompressed_streams.clear();
        self.block_id_map.clear();
        self.block_sizes.clear();
        self.decompression_tables.clear();
        self.current_loops = 0;
        self.encountered_end = false;
        self.loop_byte_offset = None;
        self.pending_data_block = None;
        self.stream_states.clear();
        self.current_sample = 0;
        self.pending_stream_writes.clear();
        self.pending_wait = None;
        self.loop_end_sample = None;
        self.pcm_data_offset = 0;
        self.total_data_block_size = 0;
    }

    /// Handles end of data command, potentially starting a new loop.
    fn handle_end_of_data(&mut self) {
        self.current_loops += 1;

        if let Some(max_loops) = self.loop_count {
            if self.current_loops >= max_loops {
                self.encountered_end = true;
                if self.fadeout_samples.is_some() {
                    self.loop_end_sample = Some(self.current_sample);
                }
            } else {
                self.jump_to_loop_point();
                self.reset_loop_state();
                if self.current_loops + 1 == max_loops && self.fadeout_samples.is_some() {
                    self.loop_end_sample = Some(self.current_sample);
                }
            }
        } else {
            self.encountered_end = true;
        }
    }

    /// Jumps to the loop point in the command stream.
    fn jump_to_loop_point(&mut self) {
        match &mut self.source {
            VgmStreamSource::Commands {
                current_index,
                loop_index,
                ..
            } => {
                if let Some(idx) = loop_index {
                    *current_index = *idx;
                } else {
                    *current_index = 0;
                }
            }
            VgmStreamSource::Bytes { .. } => {
                // For byte stream, looping is handled by re-pushing data
                // We just mark that we're ready to loop
            }
        }
    }

    /// Resets loop-specific state when starting a new loop iteration.
    fn reset_loop_state(&mut self) {
        self.pcm_data_offset = 0;

        for state in self.stream_states.values_mut() {
            state.active = false;
            state.current_data_pos = 0;
            state.next_write_sample = self.current_sample;
            state.remaining_commands = None;
        }

        self.pending_stream_writes.clear();
        self.pending_wait = None;
    }

    /// Handles a data block command by parsing it and storing or returning it.
    fn handle_data_block(&mut self, block: DataBlock) -> Result<StreamResult, ParseError> {
        let block_size = block.size as usize;
        let block_data_type = block.data_type;
        let data_type = block.data_type;
        let data_len = block.data.len();
        let marker = block.marker;
        let chip_instance = block.chip_instance;

        // Check if adding this block would exceed the size limit
        let new_total = self.total_data_block_size.saturating_add(data_len);
        if new_total > self.max_data_block_size {
            return Err(ParseError::DataBlockSizeExceeded {
                current_size: self.total_data_block_size,
                limit: self.max_data_block_size,
                attempted_size: data_len,
            });
        }

        match parse_data_block(block) {
            Ok(parsed) => {
                match parsed {
                    DataBlockType::UncompressedStream(stream) => {
                        let current_offset = self
                            .uncompressed_streams
                            .get(&data_type)
                            .map(|s| s.data.len())
                            .unwrap_or(0);
                        self.block_id_map
                            .push((data_type, current_offset, stream.data.len()));
                        self.total_data_block_size += data_len;
                        self.uncompressed_streams
                            .entry(data_type)
                            .and_modify(|existing| {
                                existing.data.extend_from_slice(&stream.data);
                            })
                            .or_insert(stream);
                        self.next_command()
                    }
                    DataBlockType::CompressedStream(stream) => {
                        self.total_data_block_size += data_len;
                        self.process_compressed_stream(data_type, stream)?;
                        self.next_command()
                    }
                    DataBlockType::DecompressionTable(table) => {
                        self.total_data_block_size += data_len;
                        self.decompression_tables.insert(data_type, table);
                        self.next_command()
                    }
                    DataBlockType::RomRamDump(dump) => {
                        // For RomRamDump, reconstruct DataBlock and return
                        let current_offset = *self.block_sizes.get(&data_type).unwrap_or(&0);
                        self.block_id_map
                            .push((data_type, current_offset, data_len));
                        *self.block_sizes.entry(data_type).or_insert(0) += data_len;
                        self.total_data_block_size += data_len;

                        let block = DataBlock {
                            marker,
                            chip_instance,
                            data_type,
                            size: dump.data.len() as u32,
                            data: dump.data,
                        };
                        Ok(StreamResult::Command(VgmCommand::DataBlock(block)))
                    }
                    DataBlockType::RamWrite16(write) => {
                        // For RamWrite16, reconstruct DataBlock and return
                        let current_offset = *self.block_sizes.get(&data_type).unwrap_or(&0);
                        self.block_id_map
                            .push((data_type, current_offset, data_len));
                        *self.block_sizes.entry(data_type).or_insert(0) += data_len;
                        self.total_data_block_size += data_len;

                        let block = DataBlock {
                            marker,
                            chip_instance,
                            data_type,
                            size: write.data.len() as u32,
                            data: write.data,
                        };
                        Ok(StreamResult::Command(VgmCommand::DataBlock(block)))
                    }
                    DataBlockType::RamWrite32(write) => {
                        // For RamWrite32, reconstruct DataBlock and return
                        let current_offset = *self.block_sizes.get(&data_type).unwrap_or(&0);
                        self.block_id_map
                            .push((data_type, current_offset, data_len));
                        *self.block_sizes.entry(data_type).or_insert(0) += data_len;
                        self.total_data_block_size += data_len;

                        let block = DataBlock {
                            marker,
                            chip_instance,
                            data_type,
                            size: write.data.len() as u32,
                            data: write.data,
                        };
                        Ok(StreamResult::Command(VgmCommand::DataBlock(block)))
                    }
                }
            }
            Err((original_block, _err)) => {
                // If parsing fails, return the raw block without storing
                let current_offset = *self.block_sizes.get(&data_type).unwrap_or(&0);
                self.block_id_map
                    .push((block_data_type, current_offset, block_size));
                *self.block_sizes.entry(data_type).or_insert(0) += data_len;
                self.total_data_block_size += data_len;
                Ok(StreamResult::Command(VgmCommand::DataBlock(original_block)))
            }
        }
    }

    /// Process a compressed stream: perform decompression using available
    /// decompression tables and store the result as an UncompressedStream.
    fn process_compressed_stream(
        &mut self,
        data_type: u8,
        mut stream: CompressedStream,
    ) -> Result<(), ParseError> {
        let decompressed_data = match &mut stream.compression {
            CompressedStreamData::BitPacking(bp) => {
                let table = if matches!(bp.sub_type, BitPackingSubType::UseTable) {
                    Some(self.decompression_tables.get(&data_type).ok_or_else(|| {
                        ParseError::DataInconsistency(format!(
                            "DecompressionTable not found for data_type {}",
                            data_type
                        ))
                    })?)
                } else {
                    None
                };
                bp.decompress(table)?;
                bp.data.clone()
            }
            CompressedStreamData::Dpcm(dpcm) => {
                let table = self.decompression_tables.get(&data_type).ok_or_else(|| {
                    ParseError::DataInconsistency(format!(
                        "DecompressionTable not found for data_type {}",
                        data_type
                    ))
                })?;
                dpcm.decompress(table)?;
                dpcm.data.clone()
            }
            CompressedStreamData::Unknown { .. } => {
                return Err(ParseError::Other(format!(
                    "Unknown compression type for data_type {}",
                    data_type
                )));
            }
        };

        // Record block position and size in block_id_map
        let current_offset = self
            .uncompressed_streams
            .get(&data_type)
            .map(|s| s.data.len())
            .unwrap_or(0);
        self.block_id_map
            .push((data_type, current_offset, decompressed_data.len()));

        // Append decompressed data to existing stream or create new one
        self.uncompressed_streams
            .entry(data_type)
            .and_modify(|existing| {
                existing.data.extend_from_slice(&decompressed_data);
            })
            .or_insert_with(|| UncompressedStream {
                chip_type: stream.chip_type,
                data: decompressed_data,
            });
        Ok(())
    }

    /// Removes data blocks that are no longer referenced externally.
    fn cleanup_unused_data_blocks(&mut self) {
        // No-op: data_blocks are stored by value; keep entries until reset.
    }

    /// Gets a reference to an uncompressed stream by data type.
    pub fn get_uncompressed_stream(&self, data_type: u8) -> Option<&UncompressedStream> {
        self.uncompressed_streams.get(&data_type)
    }

    /// Gets a reference to a decompression table by data type.
    pub fn get_decompression_table(&self, data_type: u8) -> Option<&DecompressionTable> {
        self.decompression_tables.get(&data_type)
    }

    /// Handles SetupStreamControl command (0x90).
    fn handle_setup_stream_control(&mut self, setup: &crate::vgm::command::SetupStreamControl) {
        let state = self
            .stream_states
            .entry(setup.stream_id)
            .or_insert_with(|| StreamState {
                _stream_id: setup.stream_id,
                chip_type: 0,
                write_port: 0,
                write_command: 0,
                data_bank_id: 0,
                step_size: 1,
                step_base: 0,
                frequency_hz: None,
                start_offset: None,
                length_mode: 0,
                data_length: 0,
                block_end_pos: None,
                active: false,
                current_data_pos: 0,
                next_write_sample: 0,
                sample_fraction: 0.0,
                remaining_commands: None,
            });

        state.chip_type = setup.chip_type;
        state.write_port = setup.write_port;
        state.write_command = setup.write_command;
    }

    /// Handles SetStreamData command (0x91).
    fn handle_set_stream_data(&mut self, data: &crate::vgm::command::SetStreamData) {
        let state = self
            .stream_states
            .entry(data.stream_id)
            .or_insert_with(|| StreamState {
                _stream_id: data.stream_id,
                chip_type: 0,
                write_port: 0,
                write_command: 0,
                data_bank_id: 0,
                step_size: 0,
                step_base: 0,
                frequency_hz: None,
                start_offset: None,
                length_mode: 0,
                data_length: 0,
                block_end_pos: None,
                active: false,
                current_data_pos: 0,
                next_write_sample: 0,
                sample_fraction: 0.0,
                remaining_commands: None,
            });

        state.data_bank_id = data.data_bank_id;
        state.step_size = data.step_size;
        state.step_base = data.step_base;
    }

    /// Handles SetStreamFrequency command (0x92).
    fn handle_set_stream_frequency(&mut self, freq: &crate::vgm::command::SetStreamFrequency) {
        if let Some(state) = self.stream_states.get_mut(&freq.stream_id) {
            state.frequency_hz = Some(freq.frequency);
        }
    }

    /// Handles StartStream command (0x93).
    fn handle_start_stream(
        &mut self,
        start: &crate::vgm::command::StartStream,
    ) -> Result<(), ParseError> {
        if let Some(state) = self.stream_states.get_mut(&start.stream_id) {
            state.start_offset = Some(start.data_start_offset);
            state.length_mode = start.length_mode;
            state.data_length = start.data_length;
            state.block_end_pos = None; // StartStream doesn't set block boundaries
            state.active = true;

            // Calculate initial data position
            let base_offset = if start.data_start_offset >= 0 {
                start.data_start_offset as usize
            } else {
                0
            };
            state.current_data_pos = base_offset + state.step_base as usize;

            state.next_write_sample = self.current_sample;
            state.sample_fraction = 0.0;

            if state.length_mode == 1 {
                state.remaining_commands = Some(start.data_length);
            } else {
                state.remaining_commands = None;
            }
        }
        Ok(())
    }

    /// Handles StopStream command (0x94).
    fn handle_stop_stream(&mut self, stop: &crate::vgm::command::StopStream) {
        if stop.stream_id == 0xFF {
            for state in self.stream_states.values_mut() {
                state.active = false;
            }
        } else if let Some(state) = self.stream_states.get_mut(&stop.stream_id) {
            state.active = false;
        }
    }

    /// Handles StartStreamFastCall command (0x95).
    fn handle_start_stream_fast_call(
        &mut self,
        fast: &crate::vgm::command::StartStreamFastCall,
    ) -> Result<(), ParseError> {
        let (data_bank_id, step_base) = if let Some(state) = self.stream_states.get(&fast.stream_id)
        {
            (state.data_bank_id, state.step_base)
        } else {
            return Ok(());
        };

        let (block_offset, block_size) =
            self.get_block_offset_and_size(data_bank_id, fast.block_id)?;
        if let Some(state) = self.stream_states.get_mut(&fast.stream_id) {
            state.active = true;
            state.current_data_pos = block_offset + step_base as usize;
            state.block_end_pos = Some(block_offset + block_size);
            state.next_write_sample = self.current_sample;
            state.sample_fraction = 0.0;
            // Length mode 3 = play until end of block
            state.length_mode = 3;
            state.remaining_commands = None;
        }
        Ok(())
    }

    /// Gets the offset and size for a specific block_id within a data bank.
    ///
    /// The block_id is the global sequence number (order of appearance) of DataBlocks.
    /// Returns the byte offset and size within the concatenated stream of the specified data_bank_id.
    fn get_block_offset_and_size(
        &self,
        data_bank_id: u8,
        block_id: u16,
    ) -> Result<(usize, usize), ParseError> {
        if let Some(&(mapped_data_type, offset, size)) = self.block_id_map.get(block_id as usize) {
            if mapped_data_type == data_bank_id {
                return Ok((offset, size));
            } else {
                return Err(ParseError::DataInconsistency(format!(
                    "Block ID {} refers to data type 0x{:02x}, but stream is configured for data bank 0x{:02x}",
                    block_id, mapped_data_type, data_bank_id
                )));
            }
        }

        Err(ParseError::DataInconsistency(format!(
            "Block ID {} not found (only {} blocks defined)",
            block_id,
            self.block_id_map.len()
        )))
    }

    /// Generates stream write commands for all active streams that are due.
    ///
    /// This method is called after processing Wait commands. It checks all active streams
    /// and generates chip write commands for any streams whose next write time has been
    /// reached based on the current sample position.
    ///
    /// The generated commands are stored in `pending_stream_writes` and will be returned
    /// by the iterator before the next parsed command.
    fn generate_stream_writes(&mut self) -> Result<(), ParseError> {
        let mut writes = Vec::new();

        let stream_ids: Vec<u8> = self.stream_states.keys().copied().collect();
        for stream_id in stream_ids {
            let (
                _freq,
                sample_interval,
                mut active,
                mut next_write_sample,
                mut sample_fraction,
                mut current_data_pos,
            ) = {
                let state = match self.stream_states.get(&stream_id) {
                    Some(s) => s,
                    None => continue,
                };
                if !state.active {
                    continue;
                }
                let freq = match state.frequency_hz {
                    Some(f) if f > 0 => f,
                    _ => continue, // Skip streams without valid frequency
                };
                let sample_interval = 44100.0 / freq as f32;
                (
                    freq,
                    sample_interval,
                    state.active,
                    state.next_write_sample,
                    state.sample_fraction,
                    state.current_data_pos,
                )
            };

            if next_write_sample <= self.current_sample && active {
                let (
                    data_bank_id,
                    chip_type,
                    write_port,
                    write_command,
                    step_size,
                    length_mode,
                    remaining_commands,
                    block_end_pos,
                ) = {
                    let state = self.stream_states.get(&stream_id).unwrap();
                    (
                        state.data_bank_id,
                        state.chip_type,
                        state.write_port,
                        state.write_command,
                        state.step_size,
                        state.length_mode,
                        state.remaining_commands,
                        state.block_end_pos,
                    )
                };

                if length_mode == 1
                    && let Some(remaining) = remaining_commands
                {
                    if remaining == 0 {
                        // Stop the stream - no more commands to generate
                        if let Some(state) = self.stream_states.get_mut(&stream_id) {
                            state.active = false;
                        }
                        continue;
                    }
                    if let Some(state) = self.stream_states.get_mut(&stream_id) {
                        state.remaining_commands = Some(remaining - 1);
                    }
                }

                // Check if we've reached the block end (for FastCall with length_mode 3)
                if length_mode == 3
                    && block_end_pos.is_some()
                    && current_data_pos >= block_end_pos.unwrap()
                {
                    // Reached end of block
                    if let Some(state) = self.stream_states.get_mut(&stream_id) {
                        state.active = false;
                    }
                    continue;
                }

                let data_byte = self.read_stream_byte_at(data_bank_id, current_data_pos)?;

                if let Some(data) = data_byte {
                    if let Some(cmd) = Self::create_stream_write_command_static(
                        chip_type,
                        write_port,
                        write_command,
                        data,
                    ) {
                        writes.push(cmd);
                    }

                    current_data_pos += step_size as usize;
                    next_write_sample += sample_interval as u64;
                    sample_fraction += sample_interval.fract();
                    if sample_fraction >= 1.0 {
                        next_write_sample += 1;
                        sample_fraction -= 1.0;
                    }
                } else {
                    active = false;
                }
            }

            if let Some(state) = self.stream_states.get_mut(&stream_id) {
                state.active = active;
                state.next_write_sample = next_write_sample;
                state.sample_fraction = sample_fraction;
                state.current_data_pos = current_data_pos;
            }
        }
        self.pending_stream_writes.append(&mut writes);

        Ok(())
    }

    /// Processes a wait command, generating stream writes and splitting the wait as needed.
    ///
    /// This method handles large wait periods by:
    /// 1. Finding the next stream write time within the wait period
    /// 2. Emitting a partial wait up to that point
    /// 3. Generating the stream write
    /// 4. Saving remaining wait time for later emission
    fn process_wait_with_streams(&mut self, wait_samples: u64) -> Result<StreamResult, ParseError> {
        let target_sample = self.current_sample + wait_samples;

        let next_stream_write_sample = self.find_next_stream_write_sample(target_sample);

        if let Some(next_write_sample) = next_stream_write_sample
            && next_write_sample <= target_sample
            && next_write_sample >= self.current_sample
        {
            let wait_until_write = next_write_sample.saturating_sub(self.current_sample);
            self.current_sample = next_write_sample;
            self.generate_stream_writes()?;

            let remaining_wait = target_sample.saturating_sub(next_write_sample);
            if remaining_wait > 0 {
                self.pending_wait = Some(remaining_wait.min(u16::MAX as u64) as u16);
            }

            if wait_until_write > 0 {
                return Ok(StreamResult::Command(VgmCommand::WaitSamples(WaitSamples(
                    wait_until_write.min(u16::MAX as u64) as u16,
                ))));
            } else if !self.pending_stream_writes.is_empty() {
                let cmd = self.pending_stream_writes.remove(0);
                return Ok(StreamResult::Command(cmd));
            }
        }
        self.current_sample = target_sample;
        self.pending_wait = None;

        Ok(StreamResult::Command(VgmCommand::WaitSamples(WaitSamples(
            wait_samples.min(u16::MAX as u64) as u16,
        ))))
    }

    /// Finds the next stream write sample position that is after current_sample and at or before target_sample.
    fn find_next_stream_write_sample(&self, target_sample: u64) -> Option<u64> {
        let mut earliest: Option<u64> = None;

        for state in self.stream_states.values() {
            if !state.active {
                continue;
            }

            if state.frequency_hz.is_none() || state.frequency_hz == Some(0) {
                continue;
            }

            if state.next_write_sample >= self.current_sample
                && state.next_write_sample <= target_sample
            {
                match earliest {
                    None => earliest = Some(state.next_write_sample),
                    Some(e) if state.next_write_sample < e => {
                        earliest = Some(state.next_write_sample);
                    }
                    _ => {}
                }
            }
        }

        earliest
    }

    /// Reads a byte from the stream's data block at the specified position.
    ///
    /// Reads from uncompressed streams (which have been decompressed from compressed
    /// data blocks). Non-stream blocks are not stored in VgmStream.
    fn read_stream_byte_at(&self, data_bank_id: u8, pos: usize) -> Result<Option<u8>, ParseError> {
        if let Some(stream) = self
            .uncompressed_streams
            .get(&data_bank_id)
            .filter(|stream| pos < stream.data.len())
        {
            return Ok(Some(stream.data[pos]));
        }

        Ok(None)
    }

    /// Reads a byte from the PCM data bank (type 0x00) at the current offset.
    ///
    /// This is used by the 0x8n commands to read YM2612 DAC data.
    fn read_pcm_data_bank_byte(&self) -> Result<Option<u8>, ParseError> {
        if let Some(stream) = self
            .uncompressed_streams
            .get(&0x00)
            .filter(|stream| self.pcm_data_offset < stream.data.len())
        {
            return Ok(Some(stream.data[self.pcm_data_offset]));
        }

        Ok(None)
    }

    /// Creates a VgmCommand for writing a stream byte to the appropriate chip.
    ///
    /// Maps the stream's chip type to the appropriate VGM chip write command variant.
    /// The chip_type follows the VGM header clock order (see VGM specification).
    /// Bit 7 (0x80) indicates the second chip instance.
    ///
    /// This is a static method to avoid borrowing conflicts during stream generation.
    fn create_stream_write_command_static(
        chip_type: u8,
        write_port: u8,
        write_command: u8,
        data: u8,
    ) -> Option<VgmCommand> {
        use crate::chip;
        use crate::vgm::command::Instance;

        let instance = if chip_type & 0x80 != 0 {
            Instance::Secondary
        } else {
            Instance::Primary
        };
        let chip_id = chip_type & 0x7F;

        DacStreamChipType::from_u8(chip_id).map(|chip_type| match chip_type {
            DacStreamChipType::Sn76489 => {
                VgmCommand::Sn76489Write(instance, chip::PsgSpec { value: data })
            }
            DacStreamChipType::Ym2413 => VgmCommand::Ym2413Write(
                instance,
                chip::Ym2413Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ym2612 => VgmCommand::Ym2612Write(
                instance,
                chip::Ym2612Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ym2151 => VgmCommand::Ym2151Write(
                instance,
                chip::Ym2151Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::SegaPcm => VgmCommand::SegaPcmWrite(
                instance,
                chip::SegaPcmSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::Rf5c68 => VgmCommand::Rf5c68U8Write(
                instance,
                chip::Rf5c68U8Spec {
                    offset: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ym2203 => VgmCommand::Ym2203Write(
                instance,
                chip::Ym2203Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ym2608 => VgmCommand::Ym2608Write(
                instance,
                chip::Ym2608Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ym2610 => VgmCommand::Ym2610bWrite(
                instance,
                chip::Ym2610Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ym3812 => VgmCommand::Ym3812Write(
                instance,
                chip::Ym3812Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ym3526 => VgmCommand::Ym3526Write(
                instance,
                chip::Ym3526Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Y8950 => VgmCommand::Y8950Write(
                instance,
                chip::Y8950Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ymf262 => VgmCommand::Ymf262Write(
                instance,
                chip::Ymf262Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ymf278b => VgmCommand::Ymf278bWrite(
                instance,
                chip::Ymf278bSpec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ymf271 => VgmCommand::Ymf271Write(
                instance,
                chip::Ymf271Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Ymz280b => VgmCommand::Ymz280bWrite(
                instance,
                chip::Ymz280bSpec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Rf5c164 => VgmCommand::Rf5c164U8Write(
                instance,
                chip::Rf5c164U8Spec {
                    offset: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Pwm => VgmCommand::PwmWrite(
                instance,
                chip::PwmSpec {
                    register: write_port & 0x0F,
                    value: ((write_command as u32) << 8) | (data as u32),
                },
            ),
            DacStreamChipType::Ay8910 => VgmCommand::Ay8910Write(
                instance,
                chip::Ay8910Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::GbDmg => VgmCommand::GbDmgWrite(
                instance,
                chip::GbDmgSpec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::NesApu => VgmCommand::NesApuWrite(
                instance,
                chip::NesApuSpec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::MultiPcm => VgmCommand::MultiPcmWrite(
                instance,
                chip::MultiPcmSpec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Upd7759 => VgmCommand::Upd7759Write(
                instance,
                chip::Upd7759Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Okim6258 => VgmCommand::Okim6258Write(
                instance,
                chip::Okim6258Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Okim6295 => VgmCommand::Okim6295Write(
                instance,
                chip::Okim6295Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::K051649 => VgmCommand::Scc1Write(
                instance,
                chip::Scc1Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::K054539 => VgmCommand::K054539Write(
                instance,
                chip::K054539Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::Huc6280 => VgmCommand::Huc6280Write(
                instance,
                chip::Huc6280Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::C140 => VgmCommand::C140Write(
                instance,
                chip::C140Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::K053260 => VgmCommand::K053260Write(
                instance,
                chip::K053260Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Pokey => VgmCommand::PokeyWrite(
                instance,
                chip::PokeySpec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Qsound => VgmCommand::QsoundWrite(
                instance,
                chip::QsoundSpec {
                    register: write_command,
                    value: ((write_port as u16) << 8) | (data as u16),
                },
            ),
            DacStreamChipType::Scsp => VgmCommand::ScspWrite(
                instance,
                chip::ScspSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::WonderSwan => VgmCommand::WonderSwanWrite(
                instance,
                chip::WonderSwanSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::Vsu => VgmCommand::VsuWrite(
                instance,
                chip::VsuSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::Saa1099 => VgmCommand::Saa1099Write(
                instance,
                chip::Saa1099Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Es5503 => VgmCommand::Es5503Write(
                instance,
                chip::Es5503Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::Es5506 => VgmCommand::Es5506BEWrite(
                instance,
                chip::Es5506U8Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::X1010 => VgmCommand::X1010Write(
                instance,
                chip::X1010Spec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            ),
            DacStreamChipType::C352 => VgmCommand::C352Write(
                instance,
                chip::C352Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data as u16,
                },
            ),
            DacStreamChipType::Ga20 => VgmCommand::Ga20Write(
                instance,
                chip::Ga20Spec {
                    register: write_command,
                    value: data,
                },
            ),
            DacStreamChipType::Mikey => VgmCommand::MikeyWrite(
                instance,
                chip::MikeySpec {
                    register: write_command,
                    value: data,
                },
            ),
        })
    }
}

impl Default for VgmStream {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator implementation for convenient command processing.
impl Iterator for VgmStream {
    type Item = Result<StreamResult, ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_command() {
            Ok(stream_result) => Some(Ok(stream_result)),
            Err(e) => Some(Err(e)),
        }
    }
}
