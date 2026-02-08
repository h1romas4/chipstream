//! VGM stream parser for real-time, memory-efficient command processing.
//!
//! This module provides a streaming parser for VGM data that processes commands
//! one at a time to minimize memory usage. It handles incomplete data gracefully
//! by buffering only the minimum necessary bytes.
//!
//! # Features
//!
//! - **Memory-efficient streaming**: Processes commands one at a time with minimal buffering
//! - **DAC Stream Control**: Automatically handles VGM DAC stream control commands (0x90-0x95)
//!   and generates chip register writes from stream data at the appropriate sample times
//! - **Data block management**: Stores and decompresses data blocks for stream playback
//! - **Flexible iteration**: Iterator-based API for easy command processing
//!
//! # DAC Stream Control
//!
//! The parser automatically handles DAC Stream Control commands, which allow VGM files to
//! store PCM data efficiently and play it back by writing to chip registers at specific
//! sample rates. When the parser encounters stream control commands, it:
//!
//! 1. Stores stream configuration (chip type, port, register, frequency)
//! 2. References data blocks containing PCM samples
//! 3. Generates chip write commands at the correct sample positions during Wait commands
//! 4. Interleaves generated writes with parsed commands in the output
//!
//! Supported stream control commands:
//! - `0x90 SetupStreamControl`: Configure stream chip target
//! - `0x91 SetStreamData`: Set data source and stepping
//! - `0x92 SetStreamFrequency`: Set playback frequency
//! - `0x93 StartStream`: Begin stream playback
//! - `0x94 StopStream`: Stop stream playback
//! - `0x95 StartStreamFastCall`: Quick-start stream playback
//!
//! # Supported Chips
//!
//! All VGM chips are supported for DAC Stream Control (40+ chips):
//! - Sound Generators: SN76489, AY8910, GameBoy DMG, NES APU, Pokey, SAA1099
//! - FM Synthesis: YM2413, YM2612, YM2151, YM2203, YM2608, YM2610/B, YM3812, YM3526, Y8950,
//!   YMF262, YMF278B, YMF271, YMZ280B
//! - PCM: Sega PCM, RF5C68, RF5C164, PWM, MultiPCM, uPD7759, OKIM6258, OKIM6295, HuC6280,
//!   SCSP, WonderSwan, VSU, QSound, Mikey
//! - Wavetable: K051649/SCC1, K054539, C140, K053260, ES5503, ES5506, X1-010, C352, GA20
//!
//! Chip types follow VGM header clock order with bit 7 indicating secondary chip instance.
//!
//! # Examples
//!
//! ## Basic Usage
//!
//! ```
//! use soundlog::vgm::stream::{VgmStream, StreamResult};
//!
//! let mut parser = VgmStream::new();
//! parser.set_loop_count(Some(2));
//!
//! // Feed VGM data in chunks
//! let vgm_data = vec![0x62, 0x63, 0x66]; // Example VGM commands
//! parser.push_data(&vgm_data);
//!
//! // Process all available commands using the iterator interface
//! for result in &mut parser {
//!     match result {
//!         Ok(StreamResult::Command(cmd)) => {
//!             println!("Parsed: {:?}", cmd);
//!             // Process command immediately to save memory
//!         }
//!         Ok(StreamResult::NeedsMoreData) => break,
//!         Ok(StreamResult::EndOfStream) => break,
//!         Err(e) => {
//!             eprintln!("Parse error: {}", e);
//!             break;
//!         }
//!     }
//! }
//! ```
//!
//! ## Streaming from Multiple Data Chunks
//!
//! ```
//! use soundlog::vgm::stream::{VgmStream, StreamResult};
//!
//! let mut parser = VgmStream::new();
//!
//! // Simulate receiving data in chunks (like from a network stream)
//! let chunks = vec![
//!     vec![0x61, 0x44], // Incomplete wait command
//!     vec![0x01],       // Complete the wait command
//!     vec![0x62, 0x63], // Two complete commands
//! ];
//!
//! for chunk in chunks {
//!     parser.push_data(&chunk);
//!
//!     // Process all commands that are now complete using the iterator interface
//!     for result in &mut parser {
//!         match result {
//!             Ok(StreamResult::Command(cmd)) => {
//!                 println!("Got command: {:?}", cmd);
//!             }
//!             Ok(StreamResult::NeedsMoreData) => break,
//!             Ok(StreamResult::EndOfStream) => break,
//!             Err(e) => {
//!                 eprintln!("Parse error: {}", e);
//!                 break;
//!             }
//!         }
//!     }
//! }
//! ```
//!
use crate::VgmDocument;
use crate::binutil::ParseError;
use crate::vgm::command::{DataBlock, VgmCommand, WaitSamples};
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
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
enum VgmStreamSource {
    /// Raw byte stream that needs to be parsed into commands.
    Bytes {
        /// Buffer containing incomplete or unparsed VGM data.
        buffer: Vec<u8>,
        /// VGM header (parsed upfront to know loop offset, etc.)
        header: Option<VgmHeader>,
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
    /// Whether the stream is currently active/playing
    active: bool,
    /// Current read position in the data block (byte offset)
    current_data_pos: usize,
    /// Next sample number when a write should occur (at 44100 Hz sample rate)
    next_write_sample: u64,
    /// Remaining commands to write (used when length_mode == 1)
    remaining_commands: Option<u32>,
}

/// Result type for stream parsing operations.
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
/// This parser processes VGM commands one at a time, maintaining minimal internal
/// state to reduce memory usage. It can handle incomplete data by buffering only
/// the bytes necessary to complete the current command.
#[derive(Debug)]
pub struct VgmStream {
    /// Internal source of VGM commands (either bytes or pre-parsed commands)
    source: VgmStreamSource,
    /// Data blocks referenced by commands (stored by value)
    data_blocks: HashMap<u8, DataBlock>,
    /// Uncompressed streams stored by data type
    uncompressed_streams: HashMap<u8, UncompressedStream>,
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
}

impl VgmStream {
    /// Creates a new VGM stream parser.
    pub fn new() -> Self {
        Self {
            source: VgmStreamSource::Bytes {
                buffer: Vec::with_capacity(16),
                header: None,
            },
            data_blocks: HashMap::new(),
            uncompressed_streams: HashMap::new(),
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
    pub fn from_document(doc: VgmDocument) -> Self {
        // Calculate loop index from header loop_offset
        let loop_index = Self::calculate_loop_index(&doc);

        Self {
            source: VgmStreamSource::Commands {
                document: Box::new(doc),
                current_index: 0,
                loop_index,
            },
            data_blocks: HashMap::new(),
            uncompressed_streams: HashMap::new(),
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
        }
    }

    /// Calculates the command index corresponding to loop_offset in the header.
    fn calculate_loop_index(doc: &VgmDocument) -> Option<usize> {
        if doc.header.loop_offset == 0 {
            return None;
        }

        // Get header length
        let data_offset = if doc.header.data_offset == 0 {
            use crate::vgm::header::VgmHeader;
            (VgmHeader::fallback_header_size_for_version(doc.header.version) - 0x34) as u32
        } else {
            doc.header.data_offset
        };

        let mut header_len = doc.header.to_bytes(0, data_offset).len() as u32;

        // Add extra header size if present
        if let Some(ref extra) = doc.extra_header {
            header_len += extra.to_bytes().len() as u32;
        }

        // Calculate absolute loop position: 0x1C + loop_offset
        let loop_abs_offset = 0x1C_u32.wrapping_add(doc.header.loop_offset);

        // Convert to offset relative to start of commands
        let loop_command_offset = loop_abs_offset.wrapping_sub(header_len);

        // Get command offsets and find matching index
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
    /// * `data` - Raw VGM bytes to add to the parsing buffer
    pub fn push_data(&mut self, data: &[u8]) {
        match &mut self.source {
            VgmStreamSource::Bytes { buffer, header } => {
                buffer.extend_from_slice(data);

                // Try to parse header if we don't have it yet
                // Collapse the condition into a single check to keep clippy happy.
                if header.is_none() && buffer.len() >= 0x40 {
                    match crate::vgm::parser::parse_vgm_header(buffer) {
                        Ok((parsed_header, _size)) => {
                            *header = Some(parsed_header);
                        }
                        Err(_) => { /* not enough data or invalid header yet */ }
                    }
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
        // Check if we have a pending wait to emit
        if let Some(wait_samples) = self.pending_wait.take() {
            // Process the pending wait through the same logic to handle any stream writes
            return self.process_wait_with_streams(wait_samples as u64);
        }

        // Check if we have pending stream writes to emit
        if !self.pending_stream_writes.is_empty() {
            let cmd = self.pending_stream_writes.remove(0);
            return Ok(StreamResult::Command(cmd));
        }

        // Check if we have a pending data block to return
        if let Some(block) = self.pending_data_block.take() {
            return Ok(StreamResult::Command(VgmCommand::DataBlock(block)));
        }

        // Check if we've already reached the end
        if self.encountered_end {
            // Check if we're in fadeout period
            if let (Some(fadeout_samples), Some(loop_end_sample)) =
                (self.fadeout_samples, self.loop_end_sample)
            {
                // Continue processing until fadeout period expires
                if self.current_sample >= loop_end_sample + fadeout_samples {
                    return Ok(StreamResult::EndOfStream);
                }
                // During fadeout, if no more data, generate synthetic wait commands
                // to advance time until fadeout expires
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

        // Get the next raw command from the source
        let command = match self.get_next_raw_command()? {
            Some(cmd) => cmd,
            None => return Ok(StreamResult::NeedsMoreData),
        };

        // Process the command
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

                // Try to parse a command - need to clone buffer reference to avoid borrow issues
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
                // Get command at current index (document is boxed)
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
        // Handle special commands
        match &command {
            VgmCommand::EndOfData(_) => {
                self.handle_end_of_data();
                // Don't return EndOfData to the iterator, get the next command instead
                return self.next_command();
            }
            VgmCommand::DataBlock(block) => {
                return self.handle_data_block(block.clone());
            }
            VgmCommand::SetupStreamControl(setup) => {
                self.handle_setup_stream_control(setup);
                // Don't return stream control commands to the iterator
                return self.next_command();
            }
            VgmCommand::SetStreamData(data) => {
                self.handle_set_stream_data(data);
                // Don't return stream control commands to the iterator
                return self.next_command();
            }
            VgmCommand::SetStreamFrequency(freq) => {
                self.handle_set_stream_frequency(freq);
                // Don't return stream control commands to the iterator
                return self.next_command();
            }
            VgmCommand::StartStream(start) => {
                self.handle_start_stream(start)?;
                // Don't return stream control commands to the iterator
                return self.next_command();
            }
            VgmCommand::StopStream(stop) => {
                self.handle_stop_stream(stop);
                // Don't return stream control commands to the iterator
                return self.next_command();
            }
            VgmCommand::StartStreamFastCall(fast) => {
                self.handle_start_stream_fast_call(fast)?;
                // Don't return stream control commands to the iterator
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
                // WaitNSample uses lower 4 bits + 1
                let samples = (w.0 & 0x0F) as u64 + 1;
                return self.process_wait_with_streams(samples);
            }
            VgmCommand::YM2612Port0Address2AWriteAndWaitN(cmd) => {
                // 0x8n: Write PCM data from data bank to YM2612 DAC, then wait n samples
                let wait_samples = cmd.0 as u64;

                // Read byte from PCM data bank (type 0x00)
                if let Some(data_byte) = self.read_pcm_data_bank_byte()? {
                    // Create YM2612 DAC write command
                    let dac_write = VgmCommand::Ym2612Write(
                        crate::vgm::command::Instance::Primary,
                        crate::chip::Ym2612Spec {
                            port: 0,
                            register: 0x2A,
                            value: data_byte,
                        },
                    );

                    // Advance PCM data offset
                    self.pcm_data_offset += 1;

                    // If there's a wait, process it with streams
                    if wait_samples > 0 {
                        // Emit the DAC write first
                        self.pending_stream_writes.insert(0, dac_write);
                        return self.process_wait_with_streams(wait_samples);
                    } else {
                        return Ok(StreamResult::Command(dac_write));
                    }
                } else {
                    // No data available, just do the wait
                    if wait_samples > 0 {
                        return self.process_wait_with_streams(wait_samples);
                    } else {
                        return self.next_command();
                    }
                }
            }
            VgmCommand::SeekOffset(seek_offset) => {
                // 0xE0: Seek to offset in PCM data bank
                self.pcm_data_offset = seek_offset.0 as usize;
                // Don't return this command to the iterator
                return self.next_command();
            }
            _ => {}
        }

        Ok(StreamResult::Command(command))
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
    pub fn set_fadeout_samples(&mut self, samples: Option<u64>) {
        self.fadeout_samples = samples;
    }

    /// Gets the current fadeout grace period setting.
    pub fn fadeout_samples(&self) -> Option<u64> {
        self.fadeout_samples
    }

    /// Shrinks the buffer if it has grown too large relative to its usage.
    fn shrink_buffer_if_needed(&mut self) {
        // Collapse nested `if` into a single condition to satisfy clippy::collapsible_if.
        if let VgmStreamSource::Bytes { buffer, .. } = &mut self.source
            && buffer.capacity() > 1024
            && buffer.len() < buffer.capacity() / 4
        {
            buffer.shrink_to_fit();
        }
    }

    /// Returns the current size of the internal buffer.
    pub fn buffer_size(&self) -> usize {
        match &self.source {
            VgmStreamSource::Bytes { buffer, .. } => buffer.len(),
            VgmStreamSource::Commands { .. } => 0,
        }
    }

    /// Optimizes memory usage by cleaning up unused resources.
    pub fn optimize_memory(&mut self) {
        self.cleanup_unused_data_blocks();
        if let VgmStreamSource::Bytes { buffer, .. } = &mut self.source {
            buffer.shrink_to_fit();
        }
    }

    /// Resets the parser state, clearing all buffers and data blocks.
    pub fn reset(&mut self) {
        match &mut self.source {
            VgmStreamSource::Bytes { buffer, header } => {
                buffer.clear();
                *header = None;
            }
            VgmStreamSource::Commands { current_index, .. } => {
                *current_index = 0;
            }
        }
        self.data_blocks.clear();
        self.uncompressed_streams.clear();
        self.decompression_tables.clear();
        self.current_loops = 0;
        self.encountered_end = false;
        self.loop_byte_offset = None;
        self.pending_data_block = None;
        self.stream_states.clear();
        self.current_sample = 0;
        self.pending_stream_writes.clear();
        self.pending_wait = None;
        self.fadeout_samples = None;
        self.loop_end_sample = None;
        self.pcm_data_offset = 0;
    }

    /// Handles end of data command, potentially starting a new loop.
    fn handle_end_of_data(&mut self) {
        self.current_loops += 1;

        if let Some(max_loops) = self.loop_count {
            if self.current_loops >= max_loops {
                self.encountered_end = true;
                // Record the sample position when loop ends for fadeout tracking
                if self.fadeout_samples.is_some() {
                    self.loop_end_sample = Some(self.current_sample);
                }
            } else {
                // Loop back to loop point
                self.jump_to_loop_point();

                // Reset loop-specific state
                self.reset_loop_state();

                // Record sample position on final loop for fadeout
                if self.current_loops + 1 == max_loops && self.fadeout_samples.is_some() {
                    self.loop_end_sample = Some(self.current_sample);
                }
            }
        } else {
            // No loop limit set, treat as single playthrough (no looping)
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
                    // No loop point, restart from beginning
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
        // Reset PCM data offset to beginning
        self.pcm_data_offset = 0;

        // Reset stream states to inactive
        for state in self.stream_states.values_mut() {
            state.active = false;
            state.current_data_pos = 0;
            state.next_write_sample = self.current_sample;
            state.remaining_commands = None;
        }

        // Clear pending operations
        self.pending_stream_writes.clear();
        self.pending_wait = None;
    }

    /// Handles a data block command by parsing it and storing or returning it.
    fn handle_data_block(&mut self, block: DataBlock) -> Result<StreamResult, ParseError> {
        let data_type = block.data_type;

        // Try to parse the data block into its detailed type
        match parse_data_block(block.clone()) {
            Ok(parsed) => {
                match parsed {
                    DataBlockType::UncompressedStream(stream) => {
                        // Store uncompressed stream in state
                        self.uncompressed_streams.insert(data_type, stream);
                        // Don't return this to the iterator
                        self.next_command()
                    }
                    DataBlockType::CompressedStream(stream) => {
                        // Decompress and store as uncompressed (delegated to helper)
                        self.process_compressed_stream(data_type, stream)?;
                        // Don't return this to the iterator
                        self.next_command()
                    }
                    DataBlockType::DecompressionTable(table) => {
                        // Store decompression table in state
                        self.decompression_tables.insert(data_type, table);
                        // Don't return this to the iterator
                        self.next_command()
                    }
                    _ => {
                        // For other types (RomRamDump, RamWrite16, RamWrite32),
                        // store the raw block and return it to the iterator
                        self.data_blocks.insert(data_type, block.clone());
                        Ok(StreamResult::Command(VgmCommand::DataBlock(block)))
                    }
                }
            }
            Err(_) => {
                // If parsing fails, store the raw block and return it
                self.data_blocks.insert(data_type, block.clone());
                Ok(StreamResult::Command(VgmCommand::DataBlock(block)))
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
        // Decompress the stream and obtain the uncompressed bytes
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

        // Create and store uncompressed stream
        let uncompressed = UncompressedStream {
            chip_type: stream.chip_type,
            data: decompressed_data,
        };
        self.uncompressed_streams.insert(data_type, uncompressed);
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
                active: false,
                current_data_pos: 0,
                next_write_sample: 0,
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
                step_size: 1,
                step_base: 0,
                frequency_hz: None,
                start_offset: None,
                length_mode: 0,
                data_length: 0,
                active: false,
                current_data_pos: 0,
                next_write_sample: 0,
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
            state.active = true;

            // Calculate initial data position
            let base_offset = if start.data_start_offset >= 0 {
                start.data_start_offset as usize
            } else {
                0
            };
            state.current_data_pos = base_offset + state.step_base as usize;

            // Set up next write sample
            state.next_write_sample = self.current_sample;

            // Initialize remaining commands counter if length_mode is 1 (command count)
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
            // Stop all streams
            for state in self.stream_states.values_mut() {
                state.active = false;
            }
        } else {
            // Stop specific stream
            if let Some(state) = self.stream_states.get_mut(&stop.stream_id) {
                state.active = false;
            }
        }
    }

    /// Handles StartStreamFastCall command (0x95).
    fn handle_start_stream_fast_call(
        &mut self,
        fast: &crate::vgm::command::StartStreamFastCall,
    ) -> Result<(), ParseError> {
        if let Some(state) = self.stream_states.get_mut(&fast.stream_id) {
            // Fast call uses block_id and flags
            // block_id refers to a data block number in the current data bank
            // For now, we'll treat block_id as the data_start_offset
            // Flags bit 0: loop enable, bit 1: reverse playback

            state.active = true;
            state.current_data_pos = fast.block_id as usize + state.step_base as usize;
            state.next_write_sample = self.current_sample;

            // Length mode 3 = play until end of block
            state.length_mode = 3;
            state.remaining_commands = None;
        }
        Ok(())
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

        // Collect stream IDs to process (avoid borrowing issues)
        let stream_ids: Vec<u8> = self.stream_states.keys().copied().collect();

        for stream_id in stream_ids {
            // Extract all needed state info upfront to avoid borrowing issues
            let (_freq, sample_interval, mut active, mut next_write_sample, mut current_data_pos) = {
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

                let sample_interval = 44100.0 / freq as f64;
                (
                    freq,
                    sample_interval,
                    state.active,
                    state.next_write_sample,
                    state.current_data_pos,
                )
            };

            // Generate writes for all samples that have passed
            while next_write_sample <= self.current_sample && active {
                // Get state again for checking length mode
                let (
                    data_bank_id,
                    chip_type,
                    write_port,
                    write_command,
                    step_size,
                    length_mode,
                    remaining_commands,
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
                    )
                };

                // Check if we should stop based on length mode
                if length_mode == 1 {
                    // Command count mode
                    if let Some(remaining) = remaining_commands {
                        if remaining == 0 {
                            active = false;
                            break;
                        }
                        // Update remaining commands
                        if let Some(state) = self.stream_states.get_mut(&stream_id) {
                            state.remaining_commands = Some(remaining - 1);
                        }
                    }
                }

                // Get data from the appropriate data block
                let data_byte = self.read_stream_byte_at(data_bank_id, current_data_pos)?;

                if let Some(data) = data_byte {
                    // Create a chip write command based on chip_type
                    if let Some(cmd) = Self::create_stream_write_command_static(
                        chip_type,
                        write_port,
                        write_command,
                        data,
                    ) {
                        writes.push(cmd);
                    }

                    // Advance data position by step_size
                    current_data_pos += step_size as usize;

                    // Schedule next write
                    next_write_sample += sample_interval as u64;
                } else {
                    // End of data - stop the stream regardless of length mode
                    active = false;
                    break;
                }
            }

            // Update state with new values
            if let Some(state) = self.stream_states.get_mut(&stream_id) {
                state.active = active;
                state.next_write_sample = next_write_sample;
                state.current_data_pos = current_data_pos;
            }
        }

        // Append new writes to pending queue (they should come after existing pending writes)
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

        // Find the earliest stream write time within this wait period
        let next_stream_write_sample = self.find_next_stream_write_sample(target_sample);

        if let Some(next_write_sample) = next_stream_write_sample
            && next_write_sample <= target_sample
            && next_write_sample >= self.current_sample
        {
            // Stream write occurs during this wait period - split the wait
            let wait_until_write = next_write_sample.saturating_sub(self.current_sample);

            // Advance to the stream write time
            self.current_sample = next_write_sample;

            // Generate stream writes at this position (only generates writes at current_sample)
            self.generate_stream_writes()?;

            // Calculate remaining wait time
            let remaining_wait = target_sample.saturating_sub(next_write_sample);
            if remaining_wait > 0 {
                self.pending_wait = Some(remaining_wait.min(u16::MAX as u64) as u16);
            }

            // Return the first part of the wait (up to the stream write)
            if wait_until_write > 0 {
                return Ok(StreamResult::Command(VgmCommand::WaitSamples(WaitSamples(
                    wait_until_write.min(u16::MAX as u64) as u16,
                ))));
            } else {
                // Stream write happens immediately, emit it first
                if !self.pending_stream_writes.is_empty() {
                    let cmd = self.pending_stream_writes.remove(0);
                    return Ok(StreamResult::Command(cmd));
                }
            }
        }

        // No stream writes during this wait period, or we've already handled them
        self.current_sample = target_sample;
        self.generate_stream_writes()?;

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

            // Check if this stream has a write at or before target_sample and at or after current_sample
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
    /// First checks uncompressed streams (which have been decompressed from compressed
    /// data blocks), then falls back to raw data blocks.
    fn read_stream_byte_at(&self, data_bank_id: u8, pos: usize) -> Result<Option<u8>, ParseError> {
        // Try to get the uncompressed stream first
        if let Some(stream) = self
            .uncompressed_streams
            .get(&data_bank_id)
            .filter(|stream| pos < stream.data.len())
        {
            return Ok(Some(stream.data[pos]));
        }

        // Fallback to raw data blocks
        if let Some(block) = self
            .data_blocks
            .get(&data_bank_id)
            .filter(|block| pos < block.data.len())
        {
            return Ok(Some(block.data[pos]));
        }

        Ok(None)
    }

    /// Reads a byte from the PCM data bank (type 0x00) at the current offset.
    ///
    /// This is used by the 0x8n commands to read YM2612 DAC data.
    fn read_pcm_data_bank_byte(&self) -> Result<Option<u8>, ParseError> {
        // Try to get the uncompressed stream first (type 0x00)
        if let Some(stream) = self
            .uncompressed_streams
            .get(&0x00)
            .filter(|stream| self.pcm_data_offset < stream.data.len())
        {
            return Ok(Some(stream.data[self.pcm_data_offset]));
        }

        // Fallback to raw data blocks (type 0x00)
        if let Some(block) = self
            .data_blocks
            .get(&0x00)
            .filter(|block| self.pcm_data_offset < block.data.len())
        {
            return Ok(Some(block.data[self.pcm_data_offset]));
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

        // Extract chip instance from bit 7
        let instance = if chip_type & 0x80 != 0 {
            Instance::Secondary
        } else {
            Instance::Primary
        };
        let chip_id = chip_type & 0x7F;

        // Map chip_type to actual chip write commands
        // Chip type follows VGM header clock order (see VGM spec)
        match chip_id {
            0x00 => {
                // SN76489 PSG
                Some(VgmCommand::Sn76489Write(
                    instance,
                    chip::PsgSpec { value: data },
                ))
            }
            0x01 => {
                // YM2413
                Some(VgmCommand::Ym2413Write(
                    instance,
                    chip::Ym2413Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x02 => {
                // YM2612
                Some(VgmCommand::Ym2612Write(
                    instance,
                    chip::Ym2612Spec {
                        port: write_port,
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x03 => {
                // YM2151
                Some(VgmCommand::Ym2151Write(
                    instance,
                    chip::Ym2151Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x04 => {
                // Sega PCM
                Some(VgmCommand::SegaPcmWrite(
                    instance,
                    chip::SegaPcmSpec {
                        offset: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x05 => {
                // RF5C68 (register write)
                Some(VgmCommand::Rf5c68U8Write(
                    instance,
                    chip::Rf5c68U8Spec {
                        offset: write_command,
                        value: data,
                    },
                ))
            }
            0x06 => {
                // YM2203
                Some(VgmCommand::Ym2203Write(
                    instance,
                    chip::Ym2203Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x07 => {
                // YM2608
                Some(VgmCommand::Ym2608Write(
                    instance,
                    chip::Ym2608Spec {
                        port: write_port,
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x08 => {
                // YM2610/YM2610B
                Some(VgmCommand::Ym2610bWrite(
                    instance,
                    chip::Ym2610Spec {
                        port: write_port,
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x09 => {
                // YM3812
                Some(VgmCommand::Ym3812Write(
                    instance,
                    chip::Ym3812Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x0A => {
                // YM3526
                Some(VgmCommand::Ym3526Write(
                    instance,
                    chip::Ym3526Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x0B => {
                // Y8950
                Some(VgmCommand::Y8950Write(
                    instance,
                    chip::Y8950Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x0C => {
                // YMF262
                Some(VgmCommand::Ymf262Write(
                    instance,
                    chip::Ymf262Spec {
                        port: write_port,
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x0D => {
                // YMF278B
                Some(VgmCommand::Ymf278bWrite(
                    instance,
                    chip::Ymf278bSpec {
                        port: write_port,
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x0E => {
                // YMF271
                Some(VgmCommand::Ymf271Write(
                    instance,
                    chip::Ymf271Spec {
                        port: write_port,
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x0F => {
                // YMZ280B
                Some(VgmCommand::Ymz280bWrite(
                    instance,
                    chip::Ymz280bSpec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x10 => {
                // RF5C164 (register write)
                Some(VgmCommand::Rf5c164U8Write(
                    instance,
                    chip::Rf5c164U8Spec {
                        offset: write_command,
                        value: data,
                    },
                ))
            }
            0x11 => {
                // PWM
                Some(VgmCommand::PwmWrite(
                    instance,
                    chip::PwmSpec {
                        register: write_port & 0x0F,
                        value: ((write_command as u32) << 8) | (data as u32),
                    },
                ))
            }
            0x12 => {
                // AY8910
                Some(VgmCommand::Ay8910Write(
                    instance,
                    chip::Ay8910Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x13 => {
                // GameBoy DMG
                Some(VgmCommand::GbDmgWrite(
                    instance,
                    chip::GbDmgSpec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x14 => {
                // NES APU
                Some(VgmCommand::NesApuWrite(
                    instance,
                    chip::NesApuSpec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x15 => {
                // MultiPCM
                Some(VgmCommand::MultiPcmWrite(
                    instance,
                    chip::MultiPcmSpec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x16 => {
                // uPD7759
                Some(VgmCommand::Upd7759Write(
                    instance,
                    chip::Upd7759Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x17 => {
                // OKIM6258
                Some(VgmCommand::Okim6258Write(
                    instance,
                    chip::Okim6258Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x18 => {
                // OKIM6295
                Some(VgmCommand::Okim6295Write(
                    instance,
                    chip::Okim6295Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x19 => {
                // K051649/SCC1
                Some(VgmCommand::Scc1Write(
                    instance,
                    chip::Scc1Spec {
                        port: write_port,
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x1A => {
                // K054539
                Some(VgmCommand::K054539Write(
                    instance,
                    chip::K054539Spec {
                        register: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x1B => {
                // HuC6280
                Some(VgmCommand::Huc6280Write(
                    instance,
                    chip::Huc6280Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x1C => {
                // C140
                Some(VgmCommand::C140Write(
                    instance,
                    chip::C140Spec {
                        register: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x1D => {
                // K053260
                Some(VgmCommand::K053260Write(
                    instance,
                    chip::K053260Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x1E => {
                // Pokey
                Some(VgmCommand::PokeyWrite(
                    instance,
                    chip::PokeySpec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x1F => {
                // QSound
                Some(VgmCommand::QsoundWrite(
                    instance,
                    chip::QsoundSpec {
                        register: write_command,
                        value: ((write_port as u16) << 8) | (data as u16),
                    },
                ))
            }
            0x20 => {
                // SCSP
                Some(VgmCommand::ScspWrite(
                    instance,
                    chip::ScspSpec {
                        offset: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x21 => {
                // WonderSwan
                Some(VgmCommand::WonderSwanWrite(
                    instance,
                    chip::WonderSwanSpec {
                        offset: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x22 => {
                // VSU
                Some(VgmCommand::VsuWrite(
                    instance,
                    chip::VsuSpec {
                        offset: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x23 => {
                // SAA1099
                Some(VgmCommand::Saa1099Write(
                    instance,
                    chip::Saa1099Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x24 => {
                // ES5503
                Some(VgmCommand::Es5503Write(
                    instance,
                    chip::Es5503Spec {
                        register: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x25 => {
                // ES5506 (8-bit write)
                Some(VgmCommand::Es5506BEWrite(
                    instance,
                    chip::Es5506U8Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x26 => {
                // X1-010
                Some(VgmCommand::X1010Write(
                    instance,
                    chip::X1010Spec {
                        offset: ((write_port as u16) << 8) | (write_command as u16),
                        value: data,
                    },
                ))
            }
            0x27 => {
                // C352
                Some(VgmCommand::C352Write(
                    instance,
                    chip::C352Spec {
                        register: ((write_port as u16) << 8) | (write_command as u16),
                        value: data as u16,
                    },
                ))
            }
            0x28 => {
                // GA20
                Some(VgmCommand::Ga20Write(
                    instance,
                    chip::Ga20Spec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            0x29 => {
                // Mikey
                Some(VgmCommand::MikeyWrite(
                    instance,
                    chip::MikeySpec {
                        register: write_command,
                        value: data,
                    },
                ))
            }
            _ => {
                // Unknown or unsupported chip type
                None
            }
        }
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
