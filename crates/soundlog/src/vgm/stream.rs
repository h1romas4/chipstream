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
use crate::chip;
use crate::vgm::command::{
    DataBlock, Instance, LengthMode, SetStreamData, SetStreamFrequency, SetupStreamControl,
    StartStream, StartStreamFastCall, StopStream, VgmCommand, WaitSamples,
    Ym2612Port0Address2AWriteAndWaitN,
};
use crate::vgm::detail::{
    BitPackingSubType, CompressedStream, CompressedStreamData, DataBlockType, DecompressionTable,
    StreamChipType, UncompressedStream, parse_data_block,
};
use crate::vgm::header::{ChipId, VgmHeader, VgmHeaderField};
use crate::vgm::parser::parse_vgm_command;
use std::collections::HashMap;

/// Minimum buffer capacity (in bytes) at which we consider shrinking the
/// parser's internal byte buffer. The shrink logic avoids attempting to reduce
/// capacity for small allocations to prevent frequent reallocations and allocator
/// churn on light workloads, while still allowing large, mostly-unused buffers
/// to be reduced to save RAM. Tune this value for more constrained environments
/// (lower for very small-RAM targets).
const MIN_CAP_TO_SHRINK: usize = 64 * 1024; // 64 KiB

/// Internal source of VGM commands for the stream processor.
///
/// The stream processor can work with either raw byte streams that need parsing,
/// or pre-parsed command streams from a VgmDocument.
#[derive(Debug)]
enum VgmStreamSource {
    /// Raw byte stream that needs to be parsed into commands.
    Buffer {
        /// Buffer containing incomplete or unparsed VGM data.
        buffer: Vec<u8>,
    },
    /// Pre-parsed commands from a VgmDocument.
    Document {
        /// Reference to the original document (boxed to avoid large enum variant size)
        document: Box<VgmDocument>,
        /// Current command index
        current_index: usize,
        /// Loop point command index (None if no loop)
        loop_index: Option<usize>,
    },
    /// Raw VGM file bytes owned by the stream (created via `VgmStream::from_vgm`).
    ///
    /// Unlike `Buffer`, the full file is stored and commands are parsed on-demand
    /// by advancing `current_pos` through `data`.  No secondary command buffer is
    /// required, so memory usage is roughly the file size only.
    File {
        /// Complete raw VGM file (including header).
        data: Vec<u8>,
        /// Absolute byte offset of the first command (0x34 + data_offset).
        command_start: usize,
        /// Current parse position within `data`.
        current_pos: usize,
        /// Absolute byte offset of the loop point, or `None` if the file has no loop.
        loop_pos: Option<usize>,
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
/// This structure holds only the **static configuration** established by the
/// VGM stream-control commands (0x90–0x95) plus two anchor values that are set
/// when playback starts (`stream_start_sample` and `start_data_pos`).  All
/// dynamic playback quantities — current data position, next write sample,
/// remaining commands, etc. — are **derived on-demand** from these anchors by
/// [`StreamSnapshot`] rather than being mutated step-by-step.
///
/// ## `LengthMode::Ignore`
///
/// Per the VGM spec (command 0x93, `mm = 0x00`):
/// > "ignore (just change current data position)"
///
/// A stream started with `Ignore` mode becomes active and its `start_data_pos`
/// is updated, but **no chip writes are generated**.  The stream remains active
/// until an explicit `StopStream` command (0x94) is received.
#[derive(Debug, Clone)]
struct StreamState {
    /// Stream ID (kept for debugging, prefixed with _ to avoid unused warning)
    _stream_id: u8,
    /// Typed DAC stream chip id to write to
    chip_type: ChipId,
    /// Instance of the chip (Primary/Secondary)
    instance: Instance,
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
    /// Start offset in the data block (stored for reference)
    start_offset: Option<i32>,
    /// Length mode (0=ignore, 1=count commands, 2=milliseconds, 3=play until end)
    length_mode: LengthMode,
    /// Data length (interpretation depends on length_mode)
    data_length: u32,
    /// End position for the current block (used with FastCall and length_mode 3)
    block_end_pos: Option<usize>,
    /// Whether the stream is currently active/playing
    active: bool,
    /// Byte offset in the data bank where this stream's playback begins.
    /// Forward playback starts here; reverse playback uses this as its lower bound.
    start_data_pos: usize,
    /// The value of `VgmStream::current_sample` at the moment this stream was started.
    /// All timing calculations are relative to this anchor.
    stream_start_sample: usize,
    /// The step index (0-based) of the last byte that was emitted to the chip.
    /// `None` means no byte has been emitted yet since the stream was started.
    /// Used by [`StreamCalc`] to determine which step is next without re-emitting
    /// an already-written byte.
    last_emitted_step: Option<usize>,
}

/// Read-only computed view of a single active DAC stream for one `generate_stream_writes` call.
///
/// Built from [`StreamState`] at the start of each call; all playback positions
/// and write-sample numbers are derived from the static configuration and the
/// `stream_start_sample` anchor rather than being stored as mutable state.
/// The only value written back to [`StreamState`] is `active` (set to `false`
/// when the stream reaches its natural end).
struct StreamSnapshot {
    /// Stream frequency in Hz.
    freq: u32,
    /// Data bank to read from.
    data_bank_id: u8,
    /// Chip write target.
    chip_id: ChipId,
    instance: Instance,
    write_port: u8,
    write_command: u8,
    /// Bytes advanced per step.
    step_size: u8,
    /// Current length mode (carries `reverse` / `looped` flags).
    length_mode: LengthMode,
    /// Optional hard block boundary (set by FastCall).
    block_end_pos: Option<usize>,
    /// Loop / range start in the data bank.
    start_data_pos: usize,
    /// Data length parameter (units depend on `length_mode`).
    data_length: u32,
    /// Total byte length of the data bank (pre-fetched to avoid re-borrowing).
    data_bank_end: usize,
    /// `VgmStream::current_sample` when the stream was started.
    stream_start_sample: usize,
    /// Step index of the last byte already emitted (`None` = nothing emitted yet).
    last_emitted_step: Option<usize>,
}

impl StreamSnapshot {
    /// Build a snapshot view from `state` and pre-fetched `data_bank_end`.
    /// Returns `None` if the stream is inactive, has no valid frequency, or is
    /// in `Ignore` mode (which moves the data position but never emits writes).
    fn from_state(state: &StreamState, data_bank_end: usize) -> Option<Self> {
        if !state.active {
            return None;
        }
        // LengthMode::Ignore means "just change current data position" — no
        // chip writes are generated, so there is nothing for StreamCalc to do.
        if matches!(state.length_mode, LengthMode::Ignore { .. }) {
            return None;
        }
        let freq = match state.frequency_hz {
            Some(f) if f > 0 => f,
            _ => return None,
        };
        Some(Self {
            freq,
            data_bank_id: state.data_bank_id,
            chip_id: state.chip_type,
            instance: state.instance,
            write_port: state.write_port,
            write_command: state.write_command,
            step_size: state.step_size,
            length_mode: state.length_mode,
            block_end_pos: state.block_end_pos,
            start_data_pos: state.start_data_pos,
            data_length: state.data_length,
            data_bank_end,
            stream_start_sample: state.stream_start_sample,
            last_emitted_step: state.last_emitted_step,
        })
    }

    /// Number of steps in one period of `CommandCount` or `Milliseconds` mode.
    ///
    /// For `CommandCount` this is simply `data_length`.
    /// For `Milliseconds` this is `freq * data_length / 1000` (integer, rounded
    /// down), split to avoid 32-bit overflow.
    fn period_steps(&self) -> usize {
        match self.length_mode {
            LengthMode::CommandCount { .. } => self.data_length as usize,
            LengthMode::Milliseconds { .. } => {
                // Split to avoid overflow: freq * ms / 1000
                (self.freq as usize / 1000) * self.data_length as usize
                    + (self.freq as usize % 1000) * self.data_length as usize / 1000
            }
            _ => 0,
        }
    }

    /// Number of steps available in `PlayUntilEnd` mode (one period).
    fn play_until_end_steps(&self) -> usize {
        let end = self.block_end_pos.unwrap_or(self.data_bank_end);
        if self.step_size == 0 {
            return 0;
        }
        let range = end.saturating_sub(self.start_data_pos);
        range / self.step_size as usize
    }

    /// Sample number at which write #`n` (0-based) should be emitted.
    ///
    /// Uses the formula  `start + n * 44100 / freq`  (integer arithmetic) which
    /// guarantees that every write lands at the nearest integer sample boundary
    /// without floating-point accumulation error.
    fn write_sample_for_step(&self, n: usize) -> usize {
        self.stream_start_sample + n * 44100 / self.freq as usize
    }

    /// Position computation
    ///
    /// Translates a raw step index (possibly spanning multiple loop periods) into a
    /// byte offset in the data bank, respecting the current `length_mode`.
    ///
    /// Returns `None` when the stream has permanently ended (no loop, past end).
    fn data_pos_for_step(&self, raw_step: usize) -> Option<usize> {
        let (is_reverse, is_looped) = self.length_mode.flags();
        match self.length_mode {
            LengthMode::CommandCount { .. } | LengthMode::Milliseconds { .. } => {
                let period = self.period_steps();
                if period == 0 {
                    return None;
                }
                let (loop_count, step_in_period) = (raw_step / period, raw_step % period);
                if loop_count > 0 && !is_looped {
                    return None; // past end, not looping
                }
                let pos = if is_reverse {
                    // Reverse: step 0 reads the last byte of the range, step 1 the
                    // second-to-last, etc.
                    let range_end = self.start_data_pos + (period - 1) * self.step_size as usize;
                    range_end.saturating_sub(step_in_period * self.step_size as usize)
                } else {
                    self.start_data_pos + step_in_period * self.step_size as usize
                };
                // If the computed position is out of the data bank this is a "dry"
                // slot caused by step_size > available data.  For looped streams,
                // libvgm restarts from the beginning on the very next update tick
                // (same sample if possible), so we fold the position back to the
                // loop-restart point rather than returning None.  For non-looped
                // streams there is genuinely nothing to read.
                if pos >= self.data_bank_end {
                    if is_looped {
                        // Fold back: use the loop-restart position for this step.
                        let restart = if is_reverse {
                            // Reverse restart is the far end of the range.
                            self.start_data_pos + (period - 1) * self.step_size as usize
                        } else {
                            self.start_data_pos
                        };
                        // If the restart position is also out of range the data
                        // bank is genuinely unusable; return None to deactivate.
                        if restart >= self.data_bank_end {
                            return None;
                        }
                        return Some(restart);
                    }
                    return None;
                }
                Some(pos)
            }
            LengthMode::PlayUntilEnd { .. } => {
                let period = self.play_until_end_steps();
                if period == 0 {
                    return None;
                }
                let (loop_count, step_in_period) = (raw_step / period, raw_step % period);
                if loop_count > 0 && !is_looped {
                    return None;
                }
                let end = self.block_end_pos.unwrap_or(self.data_bank_end);
                let pos = if is_reverse {
                    // Reverse: step 0 reads the last byte of the block.
                    let range_end = end.saturating_sub(self.step_size as usize);
                    range_end.saturating_sub(step_in_period * self.step_size as usize)
                } else {
                    self.start_data_pos + step_in_period * self.step_size as usize
                };
                // Bounds check: position must be within the data bank.
                if pos >= self.data_bank_end {
                    if is_looped {
                        return Some(if is_reverse {
                            end.saturating_sub(self.step_size as usize)
                        } else {
                            self.start_data_pos
                        });
                    }
                    return None;
                }
                Some(pos)
            }
            // Unknown: no automatic writes.
            _ => None,
        }
    }

    /// Returns the step index of the next un-emitted step that is due at or
    /// before `current_sample`, or `None` if no step is due yet or the stream
    /// has permanently ended.
    ///
    /// A step is "due" purely based on its scheduled sample time; whether the
    /// corresponding byte position is inside the data bank is not checked here.
    fn due_step(&self, current_sample: usize) -> Option<usize> {
        let next_step = match self.last_emitted_step {
            Some(last) => last + 1,
            None => 0,
        };
        if !self.step_is_in_range(next_step) {
            return None; // stream permanently ended
        }
        let scheduled = self.write_sample_for_step(next_step);
        if scheduled > current_sample {
            return None; // not due yet
        }
        Some(next_step)
    }

    /// Returns `(data_pos, step_index)` for the next un-emitted step that is
    /// due **and** has a valid (in-bank) data position, or `None` if nothing
    /// is due or the stream has ended.
    ///
    /// Steps whose computed position is outside the data bank are "dry" slots:
    /// they occupy a time-slot but produce no byte.  Callers that need to
    /// advance the clock over dry slots should use [`due_step`] directly.
    fn due_write(&self, current_sample: usize) -> Option<(usize, usize)> {
        let next_step = self.due_step(current_sample)?;
        let pos = self.data_pos_for_step(next_step)?;
        Some((pos, next_step))
    }

    /// The sample number at which the next un-emitted write is scheduled,
    /// or `None` if the stream has permanently ended.
    ///
    /// Unlike `due_write`, this method does **not** require the step to have a
    /// valid data position — it only checks whether the step index is within the
    /// stream's logical range (i.e. `data_pos_for_step` returns Some for a
    /// representative step).  Out-of-bank dry slots still occupy a time-slot and
    /// must be tracked so the sample clock advances correctly.
    fn next_write_sample(&self, current_sample: usize) -> Option<usize> {
        let next_step = match self.last_emitted_step {
            Some(last) => last + 1,
            None => 0,
        };
        // Check whether the stream has any more logical steps at all.
        // We probe with a period-wrapped version: for looped streams any step in
        // [0, period) is always valid, so we check next_step % period (or
        // next_step itself for non-looped streams — if it is past the end,
        // data_pos_for_step returns None and the stream is done).
        //
        // To handle out-of-bank dry slots we use a small helper that checks the
        // *logical* range independently of whether the position is in-bank.
        if !self.step_is_in_range(next_step) {
            return None; // stream permanently ended
        }
        let scheduled = self.write_sample_for_step(next_step);
        // If overdue (should have been processed already), return current_sample
        // so the caller picks it up immediately.
        Some(scheduled.max(current_sample))
    }

    /// Returns true when `raw_step` is within the logical range of this stream
    /// (i.e. the stream has not permanently ended), regardless of whether the
    /// byte position falls inside the data bank.
    fn step_is_in_range(&self, raw_step: usize) -> bool {
        let (_, is_looped) = self.length_mode.flags();
        match self.length_mode {
            LengthMode::CommandCount { .. } | LengthMode::Milliseconds { .. } => {
                let period = self.period_steps();
                if period == 0 {
                    return false;
                }
                let loop_count = raw_step / period;
                // step 0 is always in range; subsequent loops require is_looped.
                loop_count == 0 || is_looped
            }
            LengthMode::PlayUntilEnd { .. } => {
                let period = self.play_until_end_steps();
                if period == 0 {
                    return false;
                }
                let loop_count = raw_step / period;
                loop_count == 0 || is_looped
            }
            _ => false,
        }
    }
}

/// Result type for stream parsing operations.
/// Default maximum size for accumulated data blocks (32 MiB).
const DEFAULT_MAX_DATA_BLOCK_SIZE: usize = 32 * 1024 * 1024;

/// Default maximum size for the internal parsing buffer (64 MiB).
const DEFAULT_MAX_BUFFER_SIZE: usize = 64 * 1024 * 1024;

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
/// It accepts either raw VGM bytes (fed via `push_chunk`) or a pre-parsed
/// `VgmDocument` (via `from_document`) and yields `VgmCommand` values through
/// its iterator interface.
///
/// Key behaviors:
/// - Produces parsed commands as soon as they are available, minimizing buffering.
/// - Automatically handles DAC stream control: it configures streams, reads
///   stream data blocks, schedules writes according to stream frequency, and
///   interleaves generated chip writes with parsed commands during Wait periods.
///
/// # Loop Handling
///
/// **Important**: When processing VGM files with loop points, the default behavior
/// is to loop infinitely (`loop_count: None`). For untrusted input or non-interactive
/// use cases, always call `set_loop_count(Some(n))` to limit the number of loop
/// iterations and prevent infinite loops. For example:
///
/// ```
/// use soundlog::vgm::VgmStream;
///
/// let mut stream = VgmStream::new();
/// stream.set_loop_count(Some(2)); // Play through twice, then stop
/// ```
///
/// ## Memory limits
///
/// The stream parser has two configurable memory limits to prevent unbounded growth:
///
/// 1. **Buffer size limit** (for raw byte parsing): Controls the maximum size of the
///    internal buffer when using `push_chunk()`. Default is 64 MiB. Configure via
///    `set_max_buffer_size()` and query via `max_buffer_size()`.
///
/// 2. **Data block size limit**: Controls the total size of accumulated data blocks
///    (PCM, DAC stream data, etc.). Default is 32 MiB. Configure via
///    `set_max_data_block_size()` and query via `max_data_block_size()` /
///    `total_data_block_size()`.
///
/// When either limit is exceeded, the parser returns `ParseError::DataBlockSizeExceeded`
/// or `ParseError::Other` respectively.
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
/// parser.push_chunk(&vgm_data).expect("push chunk");
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
/// Configuring memory limits for untrusted input:
/// ```
/// use soundlog::vgm::VgmStream;
///
/// let mut parser = VgmStream::new();
/// // Set conservative limits for untrusted input
/// parser.set_max_buffer_size(16 * 1024 * 1024);      // 16 MiB buffer
/// parser.set_max_data_block_size(16 * 1024 * 1024);  // 16 MiB data blocks
/// parser.set_loop_count(Some(2));                     // Limit loops
/// ```
///
/// From a parsed `VgmDocument` (including stream control commands):
/// ```
/// use soundlog::{VgmBuilder, vgm::stream::VgmStream};
/// use soundlog::vgm::stream::StreamResult;
/// use soundlog::vgm::command::{
///     DacStreamChipType, DataBlock,
///     SetupStreamControl, SetStreamData, SetStreamFrequency, StartStream,
///     WaitSamples, Instance,
/// };
/// use soundlog::vgm::header::ChipId;
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
///     chip_type: DacStreamChipType {
///         chip_id: ChipId::Ym2612,
///         instance: Instance::Primary,
///     },
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
///     length_mode: soundlog::vgm::command::LengthMode::PlayUntilEnd { reverse: false, looped: false },
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
    current_sample: usize,
    /// Pending stream write commands to emit
    pending_stream_writes: Vec<VgmCommand>,
    /// Pending wait time that hasn't been emitted yet (in samples)
    pending_wait: Option<u16>,
    /// Fadeout grace period in samples after loop end (None = no fadeout)
    fadeout_samples: Option<usize>,
    /// Sample position when the loop ended (for fadeout tracking)
    loop_end_sample: Option<usize>,
    /// Current read offset in PCM data bank (StreamChipType::Ym2612Pcm) for 0x8n commands
    pcm_data_offset: usize,
    /// Maximum allowed total size for accumulated data blocks
    max_data_block_size: usize,
    /// Current total size of accumulated data blocks
    total_data_block_size: usize,
    /// Maximum allowed size for the internal parsing buffer
    max_buffer_size: usize,
    /// VGM header loop_base field (signed: 0x80..0xFF = -128..-1)
    /// Subtracts from the effective loop count:
    ///  NumLoops = (ProgramNumLoops * modifier / 0x10) - loop_base
    loop_base: i8,
    /// VGM header loop_modifier field (0 is treated as 0x10, i.e. no scaling)
    /// Scales the effective loop count:
    ///  NumLoops = ProgramNumLoops * loop_modifier / 0x10
    loop_modifier: u8,
    /// Scratch buffer reused across `generate_stream_writes` calls to avoid
    /// repeated allocation when collecting active stream IDs.
    stream_id_scratch: Vec<u8>,
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
    /// # Loop Handling
    ///
    /// Default: the stream will play once (loop count = 1). To change loop behavior,
    /// call `set_loop_count(Some(n))` to play a finite number of times, or
    /// `set_loop_count(None)` to enable infinite looping. For untrusted input or
    /// non-interactive playback, prefer a finite loop count to avoid accidental
    /// infinite loops.
    ///
    /// ```
    /// use soundlog::vgm::VgmStream;
    ///
    /// let mut stream = VgmStream::new();
    /// stream.set_loop_count(Some(2)); // Play twice
    /// stream.set_loop_count(None); // Infinite loop
    /// ```
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
    /// parser.push_chunk(chunk).expect("push chunk");
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
            source: VgmStreamSource::Buffer {
                buffer: Vec::with_capacity(MIN_CAP_TO_SHRINK),
            },
            uncompressed_streams: HashMap::new(),
            block_id_map: Vec::new(),
            block_sizes: HashMap::new(),
            decompression_tables: HashMap::new(),
            loop_count: Some(1),
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
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
            loop_base: 0,
            loop_modifier: 0,
            stream_id_scratch: Vec::new(),
        }
    }

    /// Creates a new VGM stream processor from a parsed VgmDocument.
    ///
    /// This is more efficient than serializing and re-parsing when you already
    /// have a parsed document.
    ///
    /// # Loop Handling
    ///
    /// **Warning**: By default, the stream will loop infinitely if the VGM document
    /// contains a loop point. For untrusted input or non-interactive playback,
    /// always call `set_loop_count(Some(n))` to prevent infinite loops:
    ///
    /// ```
    /// use soundlog::{VgmBuilder, vgm::stream::VgmStream};
    /// use soundlog::vgm::command::WaitSamples;
    ///
    /// let mut builder = VgmBuilder::new();
    /// builder.add_vgm_command(WaitSamples(100));
    /// let doc = builder.finalize();
    ///
    /// let mut stream = VgmStream::from_document(doc);
    /// stream.set_loop_count(Some(2)); // Limit to 2 loop iterations
    /// ```
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
    pub fn from_document(document: VgmDocument) -> Self {
        let loop_index = Self::calculate_loop_index(&document);
        let loop_base = document.header.loop_base;
        let loop_modifier = document.header.loop_modifier;
        Self {
            source: VgmStreamSource::Document {
                document: Box::new(document),
                current_index: 0,
                loop_index,
            },
            loop_base,
            loop_modifier,
            ..Self::default()
        }
    }

    /// Creates a new VGM stream processor from a complete raw VGM file.
    ///
    /// Unlike [`new`](Self::new) + [`push_chunk`](Self::push_chunk), this constructor
    /// accepts the full VGM file (including header) at once and parses commands
    /// on demand without building an intermediate command list.  It is simpler to
    /// use than the push-chunk API and more memory-efficient than
    /// [`from_document`](Self::from_document) (which retains the entire parsed
    /// `VgmDocument`).  The raw bytes are kept in memory for random-access looping,
    /// but no duplicate buffer or command object tree is created.
    ///
    /// # Arguments
    /// * `data` — Complete raw VGM file bytes (uncompressed; if you have a `.vgz`
    ///   file, decompress it first).
    ///
    /// # Errors
    /// Returns [`ParseError`] if the VGM header cannot be parsed (invalid magic,
    /// truncated header, etc.).
    ///
    /// # Loop Handling
    ///
    /// **Warning**: By default, the stream will loop infinitely if the VGM file
    /// contains a loop point. Always call `set_loop_count(Some(n))` for untrusted
    /// input or non-interactive playback.
    ///
    /// # Examples
    /// ```
    /// use soundlog::{VgmBuilder, vgm::stream::{VgmStream, StreamResult}};
    /// use soundlog::vgm::command::WaitSamples;
    ///
    /// # let mut builder = VgmBuilder::new();
    /// # builder.add_vgm_command(WaitSamples(735));
    /// # let raw: Vec<u8> = builder.finalize().into();
    ///
    /// let mut stream = VgmStream::from_vgm(raw).expect("valid VGM");
    /// stream.set_loop_count(Some(1));
    /// for item in &mut stream {
    ///     match item {
    ///         Ok(StreamResult::Command(_cmd)) => {}
    ///         Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
    ///         Err(_) => break,
    ///     }
    /// }
    /// ```
    pub fn from_vgm(data: impl Into<Vec<u8>>) -> Result<Self, ParseError> {
        let data = data.into();
        let header = VgmHeader::from_bytes(&data)?;

        // Absolute byte offset of the first command.
        // Use the VgmHeader helper to compute the command start consistently.
        let command_start = VgmHeader::command_start(header.version, header.data_offset);

        // Absolute loop position: field at 0x1C is relative to that offset.
        // Validate that it actually falls within the command region.
        let loop_pos = if header.loop_offset != 0 {
            let abs = VgmHeaderField::LoopOffset
                .offset()
                .wrapping_add(header.loop_offset as usize);
            if abs >= command_start && abs < data.len() {
                Some(abs)
            } else {
                None
            }
        } else {
            None
        };

        let loop_base = header.loop_base;
        let loop_modifier = header.loop_modifier;

        Ok(Self {
            source: VgmStreamSource::File {
                data,
                command_start,
                current_pos: command_start,
                loop_pos,
            },
            loop_base,
            loop_modifier,
            ..Self::default()
        })
    }

    /// Calculates the command index corresponding to loop_offset in the header.
    fn calculate_loop_index(doc: &VgmDocument) -> Option<usize> {
        if doc.header.loop_offset == 0 {
            return None;
        }

        let data_offset = VgmHeader::data_offset(doc.header.version, doc.header.data_offset);

        let mut header_len = doc.header.to_bytes(0, data_offset).len() as u32;
        if let Some(ref extra) = doc.extra_header {
            header_len += extra.to_bytes().len() as u32;
        }
        let loop_abs_offset =
            (VgmHeaderField::LoopOffset.offset() as u32).wrapping_add(doc.header.loop_offset);
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
    /// Appends raw VGM bytes (command/data bytes) to the internal buffer for incremental parsing.
    ///
    /// Note: this method does not parse or strip the VGM header. When you have
    /// a full VGM file, feed only the serialized command/data region starting at
    /// the command stream offset: `VgmHeader::compute_command_start(header.version, header.data_offset)`.
    ///
    /// # Arguments
    /// * `chunk` - Raw VGM command/data bytes to add to the parsing buffer
    ///
    /// # Errors
    /// Returns `ParseError::Other` if adding the chunk would exceed the maximum
    /// buffer size (64 MiB) or if this method is called on a stream created from
    /// a document.
    pub fn push_chunk(&mut self, chunk: &[u8]) -> Result<(), ParseError> {
        match &mut self.source {
            VgmStreamSource::Buffer { buffer } => {
                if buffer.len() + chunk.len() > self.max_buffer_size {
                    return Err(ParseError::Other(format!(
                        "Buffer size limit exceeded: current {} bytes, chunk {} bytes, limit {} bytes",
                        buffer.len(),
                        chunk.len(),
                        self.max_buffer_size
                    )));
                }

                buffer.extend_from_slice(chunk);
                Ok(())
            }
            VgmStreamSource::Document { .. } => Err(ParseError::Other(
                "push_chunk() cannot be called on a VgmStream created from a document".into(),
            )),
            VgmStreamSource::File { .. } => Err(ParseError::Other(
                "push_chunk() cannot be called on a VgmStream created from VgmStream::from_vgm"
                    .into(),
            )),
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
            return self.process_wait_with_streams(wait_samples as usize);
        }

        if let Some(block) = self.pending_data_block.take() {
            return Ok(StreamResult::Command(VgmCommand::DataBlock(Box::new(
                block,
            ))));
        }

        if self.encountered_end {
            if let (Some(fadeout_samples), Some(loop_end_sample)) =
                (self.fadeout_samples, self.loop_end_sample)
            {
                let fadeout_end = loop_end_sample.saturating_add(fadeout_samples);
                if self.current_sample >= fadeout_end {
                    return Ok(StreamResult::EndOfStream);
                }
                let command = match self.get_next_raw_command()? {
                    Some(cmd) => cmd,
                    None => {
                        // No more data, generate a wait to advance to end of fadeout
                        let remaining = fadeout_end.saturating_sub(self.current_sample);
                        let wait_amount = remaining.min(u16::MAX as usize) as u16;
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
            VgmStreamSource::Buffer { buffer, .. } => {
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
            VgmStreamSource::Document {
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
            VgmStreamSource::File {
                data, current_pos, ..
            } => {
                if *current_pos >= data.len() {
                    return Ok(None);
                }
                match parse_vgm_command(data, *current_pos) {
                    Ok((command, consumed)) => {
                        *current_pos += consumed;
                        Ok(Some(command))
                    }
                    Err(e) => Err(e),
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
                return self.handle_data_block(*block.clone());
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
                return self.process_wait_with_streams(w.0 as usize);
            }
            VgmCommand::Wait735Samples(_) => {
                return self.process_wait_with_streams(735);
            }
            VgmCommand::Wait882Samples(_) => {
                return self.process_wait_with_streams(882);
            }
            VgmCommand::WaitNSample(w) => {
                let samples = w.0 as usize;
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
        cmd: &Ym2612Port0Address2AWriteAndWaitN,
    ) -> Result<StreamResult, ParseError> {
        let wait_samples = cmd.0 as usize;

        if let Some(data_byte) = self.read_pcm_data_bank_byte()? {
            let dac_write = VgmCommand::Ym2612Write(
                Instance::Primary,
                chip::Ym2612Spec {
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
    /// Controls how many times the stream will loop when it encounters a loop point.
    /// Set to `None` for infinite looping (default, not recommended for untrusted input),
    /// or `Some(n)` to limit loop iterations.
    ///
    /// **Important**: For untrusted input or automated processing, always set a finite
    /// loop count to prevent infinite loops and potential DoS conditions.
    ///
    /// # Arguments
    /// * `count` - Maximum number of loops to process (None for infinite, Some(n) to limit)
    ///
    /// # Normalization
    ///
    /// Passing `Some(0)` is treated the same as `Some(1)`. Historically callers
    /// have used `Some(0)` to indicate "play once and stop"; to avoid surprising
    /// behavior we normalise zero to one when storing the value.
    ///
    /// Examples:
    /// - `set_loop_count(Some(1))` — play once and stop at the first `EndOfData`.
    /// - `set_loop_count(Some(0))` — equivalent to `Some(1)` and also stops at the first `EndOfData`.
    ///
    /// # Examples
    /// ```
    /// use soundlog::vgm::VgmStream;
    ///
    /// let mut stream = VgmStream::new();
    /// stream.set_loop_count(Some(2)); // Play intro + 1 loop
    /// ```
    pub fn set_loop_count(&mut self, count: Option<u32>) {
        // Normalise `Some(0)` to `Some(1)` so callers that historically used
        // zero to mean "play once" behave as expected.
        self.loop_count = match count {
            Some(0) => Some(1),
            _ => count,
        };
    }

    /// Gets the current loop iteration count.
    pub fn current_loop_count(&self) -> u32 {
        self.current_loops
    }

    /// Sets the loop base value from the VGM header (`loop_base` field, offset `0x7E`).
    ///
    /// This is normally read automatically from the document header when using
    /// `from_document()`. Call this when building a stream via `new()` + `push_chunk()`
    /// and you have parsed the VGM header separately.
    ///
    /// The value is a signed byte: `0x80`..`0xFF` decode as -128..-1.
    /// It is subtracted from the effective loop count after the modifier is applied:
    ///
    /// `NumLoops = (program_loops * modifier / 0x10) - loop_base`
    ///
    /// Default is `0` (no adjustment).
    pub fn set_loop_base(&mut self, value: i8) {
        self.loop_base = value;
    }

    /// Gets the current loop base value.
    pub fn loop_base(&self) -> i8 {
        self.loop_base
    }

    /// Sets the loop modifier value from the VGM header (`loop_modifier` field, offset `0x7F`).
    ///
    /// This is normally read automatically from the document header when using
    /// `from_document()`. Call this when building a stream via `new()` + `push_chunk()`
    /// and you have parsed the VGM header separately.
    ///
    /// A value of `0` is treated as `0x10` (multiplier 1.0, no scaling).
    /// The effective loop count is computed as:
    ///
    /// `NumLoops = (program_loops * modifier / 0x10) - loop_base`
    ///
    /// Default is `0` (no scaling).
    pub fn set_loop_modifier(&mut self, value: u8) {
        self.loop_modifier = value;
    }

    /// Gets the current loop modifier value.
    pub fn loop_modifier(&self) -> u8 {
        self.loop_modifier
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
    pub fn set_fadeout_samples(&mut self, samples: Option<usize>) {
        self.fadeout_samples = samples;
    }

    /// Gets the current fadeout grace period.
    pub fn fadeout_samples(&self) -> Option<usize> {
        self.fadeout_samples
    }

    /// Gets the current sample position (at 44.1 kHz).
    ///
    /// This returns the number of samples that have elapsed since the start of the stream
    /// or since the last loop point. When the stream loops, this value is reset to 0.
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::VgmStream;
    /// # let doc = soundlog::VgmDocument::default();
    /// let stream = VgmStream::from_document(doc);
    /// let position = stream.current_sample();
    /// ```
    pub fn current_sample(&self) -> usize {
        self.current_sample
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

    /// Sets the maximum allowed size for the internal parsing buffer.
    ///
    /// This limit applies to the raw byte buffer used when feeding data via
    /// `push_chunk()`. When the buffer size would exceed this limit, `push_chunk()`
    /// returns an error.
    ///
    /// # Arguments
    ///
    /// * `max_size` - Maximum buffer size in bytes (default is 64 MiB)
    ///
    /// # Examples
    /// ```
    /// use soundlog::vgm::VgmStream;
    ///
    /// let mut stream = VgmStream::new();
    /// stream.set_max_buffer_size(128 * 1024 * 1024); // 128 MiB
    /// ```
    pub fn set_max_buffer_size(&mut self, max_size: usize) {
        self.max_buffer_size = max_size;
    }

    /// Gets the maximum allowed size for the internal parsing buffer.
    pub fn max_buffer_size(&self) -> usize {
        self.max_buffer_size
    }

    /// Shrinks the buffer if it has grown too large relative to its usage.
    fn shrink_buffer_if_needed(&mut self) {
        if let VgmStreamSource::Buffer { buffer, .. } = &mut self.source
            && buffer.capacity() > MIN_CAP_TO_SHRINK
            && buffer.len() < buffer.capacity() / 4
        {
            buffer.shrink_to_fit();
        }
    }

    /// Returns the current size of the internal buffer.
    #[doc(hidden)]
    pub fn buffer_size(&self) -> usize {
        match &self.source {
            VgmStreamSource::Buffer { buffer, .. } => buffer.len(),
            VgmStreamSource::Document { .. } => 0,
            VgmStreamSource::File {
                data, current_pos, ..
            } => data.len().saturating_sub(*current_pos),
        }
    }

    /// Optimizes memory usage by cleaning up unused resources.
    #[doc(hidden)]
    pub fn optimize_memory(&mut self) {
        self.cleanup_unused_data_blocks();
        if let VgmStreamSource::Buffer { buffer, .. } = &mut self.source {
            buffer.shrink_to_fit();
        }
    }

    /// Resets the parser state, clearing all buffers and data blocks.
    /// Resets the stream parser to its initial state.
    pub fn reset(&mut self) {
        match &mut self.source {
            VgmStreamSource::Buffer { buffer } => {
                buffer.clear();
            }
            VgmStreamSource::Document {
                current_index,
                loop_index: _,
                document: _,
            } => {
                *current_index = 0;
            }
            VgmStreamSource::File {
                current_pos,
                command_start,
                ..
            } => {
                *current_pos = *command_start;
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
        // loop_base and loop_modifier are header-derived configuration and are
        // intentionally preserved across reset() calls.
    }

    /// Resets the stream position to the loop point (or start if no loop point exists),
    /// clearing per-loop state such as the sample counter, pending waits, and DAC stream
    /// positions.
    ///
    /// Unlike [`reset`](Self::reset), this method preserves accumulated data blocks,
    /// decompression tables, and all other pre-flight state that was established by
    /// processing the intro section. Only the read cursor and loop-specific runtime
    /// state are affected.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Other`] if called on a stream created with
    /// [`new`](Self::new) + [`push_chunk`](Self::push_chunk) (i.e., a `Buffer`-backed
    /// stream), since those streams have no random-accessible loop position.
    pub(crate) fn reset_to_loop_point(&mut self) -> Result<(), ParseError> {
        if let VgmStreamSource::Buffer { .. } = &self.source {
            return Err(ParseError::Other(
                "seek_to_sample() is not supported for streams created with push_chunk()".into(),
            ));
        }
        self.jump_to_loop_point();
        self.reset_loop_state();
        Ok(())
    }

    /// Moves the stream to the specified sample position within the current loop iteration.
    ///
    /// Because the sample counter resets to 0 at the start of each loop, `target` refers
    /// to a position within one loop iteration (measured in 44100 Hz samples, starting
    /// from 0 at the loop point).  The stream is rewound to the loop point and then
    /// commands are consumed silently until the sample counter reaches `target`.
    ///
    /// All internal state (DAC streams, PCM offsets, stream states, etc.) is correctly
    /// maintained during fast-forward, so the stream is fully ready for playback
    /// immediately after this call returns.
    ///
    /// When used with [`VgmCallbackStream`](crate::vgm::VgmCallbackStream), prefer
    /// calling [`VgmCallbackStream::seek_to_sample`](crate::vgm::VgmCallbackStream::seek_to_sample)
    /// instead, which additionally keeps chip state trackers consistent.
    ///
    /// # Arguments
    ///
    /// * `target` — Target sample position (0-based, resets to 0 at each loop iteration).
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::Other`] if called on a stream created with
    /// [`new`](Self::new) + [`push_chunk`](Self::push_chunk).
    ///
    /// # Notes
    ///
    /// If `target` exceeds the total sample length of the loop iteration, the stream
    /// is positioned at `EndOfStream`.
    pub fn seek_to_sample(&mut self, target: usize) -> Result<(), ParseError> {
        self.reset_to_loop_point()?;
        loop {
            if self.current_sample >= target {
                break;
            }
            match self.next_command()? {
                StreamResult::EndOfStream | StreamResult::NeedsMoreData => break,
                StreamResult::Command(_) => {}
            }
        }
        Ok(())
    }

    /// Handles end of data command, potentially starting a new loop.
    fn handle_end_of_data(&mut self) {
        self.current_loops = self.current_loops.saturating_add(1);

        if let Some(max_loops) = self.effective_loop_count() {
            if self.current_loops >= max_loops {
                self.encountered_end = true;
                if self.fadeout_samples.is_some() {
                    self.loop_end_sample = Some(self.current_sample);
                }
            } else {
                self.jump_to_loop_point();
                self.reset_loop_state();
                if self.current_loops.saturating_add(1) == max_loops
                    && self.fadeout_samples.is_some()
                {
                    self.loop_end_sample = Some(self.current_sample);
                }
            }
        } else {
            // No finite loop limit configured -> infinite loop behavior.
            // Jump to the configured loop point and reset loop-specific state
            // so playback continues indefinitely.
            self.jump_to_loop_point();
            self.reset_loop_state();
        }
    }

    /// Computes the effective loop count by applying `loop_modifier` and `loop_base`
    /// from the VGM header specification.
    ///
    /// When `loop_count` is `None` (infinite), returns `None` unchanged.
    ///
    /// The specification formula is:
    /// ```text
    /// modifier = if loop_modifier == 0 { 0x10 } else { loop_modifier as u32 }
    /// effective = (program_loops * modifier / 0x10).saturating_sub_signed(loop_base)
    /// effective = effective.max(0)
    /// ```
    fn effective_loop_count(&self) -> Option<u32> {
        let program_loops = self.loop_count?;
        let modifier = if self.loop_modifier == 0 {
            0x10_u32
        } else {
            self.loop_modifier as u32
        };
        let scaled = (program_loops as usize).saturating_mul(modifier as usize) / 0x10;
        let effective = scaled as isize - self.loop_base as isize;
        Some(effective.max(0) as u32)
    }

    /// Jumps to the loop point in the command stream.
    fn jump_to_loop_point(&mut self) {
        match &mut self.source {
            VgmStreamSource::Document {
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
            VgmStreamSource::Buffer { buffer } => {
                // For byte stream, the caller is responsible for re-pushing data
                // from the loop point after each loop iteration.
                // Clear any residual bytes so the next push_chunk starts from a
                // clean state and the stale tail bytes are not re-parsed as
                // valid commands.
                buffer.clear();
            }
            VgmStreamSource::File {
                current_pos,
                loop_pos,
                command_start,
                ..
            } => {
                *current_pos = loop_pos.unwrap_or(*command_start);
            }
        }
    }

    /// Resets loop-specific state when starting a new loop iteration.
    fn reset_loop_state(&mut self) {
        self.pcm_data_offset = 0;

        // Reset sample position to 0 when looping
        self.current_sample = 0;

        for state in self.stream_states.values_mut() {
            state.active = false;
            state.stream_start_sample = 0;
            state.last_emitted_step = None;
        }

        self.pending_stream_writes.clear();
        self.pending_wait = None;
    }

    /// Handles a data block command by parsing it and storing or returning it.
    fn handle_data_block(&mut self, block: DataBlock) -> Result<StreamResult, ParseError> {
        // block is passed by value (unboxed at call site)
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

                        // Reconstruct the full data including rom_size and start_address header
                        let mut full_data = Vec::with_capacity(8 + dump.data.len());
                        full_data.extend_from_slice(&dump.rom_size.to_le_bytes());
                        full_data.extend_from_slice(&dump.start_address.to_le_bytes());
                        full_data.extend_from_slice(&dump.data);

                        let block = DataBlock {
                            marker,
                            chip_instance,
                            data_type,
                            size: full_data.len() as u32,
                            data: full_data,
                        };
                        Ok(StreamResult::Command(VgmCommand::DataBlock(Box::new(
                            block,
                        ))))
                    }
                    DataBlockType::RamWrite16(write) => {
                        // For RamWrite16, reconstruct DataBlock and return
                        let current_offset = *self.block_sizes.get(&data_type).unwrap_or(&0);
                        self.block_id_map
                            .push((data_type, current_offset, data_len));
                        *self.block_sizes.entry(data_type).or_insert(0) += data_len;
                        self.total_data_block_size += data_len;

                        // Reconstruct the full data including start_address header
                        let mut full_data = Vec::with_capacity(2 + write.data.len());
                        full_data.extend_from_slice(&write.start_address.to_le_bytes());
                        full_data.extend_from_slice(&write.data);

                        let block = DataBlock {
                            marker,
                            chip_instance,
                            data_type,
                            size: full_data.len() as u32,
                            data: full_data,
                        };
                        Ok(StreamResult::Command(VgmCommand::DataBlock(Box::new(
                            block,
                        ))))
                    }
                    DataBlockType::RamWrite32(write) => {
                        // For RamWrite32, reconstruct DataBlock and return
                        let current_offset = *self.block_sizes.get(&data_type).unwrap_or(&0);
                        self.block_id_map
                            .push((data_type, current_offset, data_len));
                        *self.block_sizes.entry(data_type).or_insert(0) += data_len;
                        self.total_data_block_size += data_len;

                        // Reconstruct the full data including start_address header
                        let mut full_data = Vec::with_capacity(4 + write.data.len());
                        full_data.extend_from_slice(&write.start_address.to_le_bytes());
                        full_data.extend_from_slice(&write.data);

                        let block = DataBlock {
                            marker,
                            chip_instance,
                            data_type,
                            size: full_data.len() as u32,
                            data: full_data,
                        };
                        Ok(StreamResult::Command(VgmCommand::DataBlock(Box::new(
                            block,
                        ))))
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
                Ok(StreamResult::Command(VgmCommand::DataBlock(Box::new(
                    original_block,
                ))))
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
        // Calculate remaining space in data block limit
        let remaining_space = self
            .max_data_block_size
            .saturating_sub(self.total_data_block_size);

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
                bp.decompress(table, remaining_space)?;
                bp.data.clone()
            }
            CompressedStreamData::Dpcm(dpcm) => {
                let table = self.decompression_tables.get(&data_type).ok_or_else(|| {
                    ParseError::DataInconsistency(format!(
                        "DecompressionTable not found for data_type {}",
                        data_type
                    ))
                })?;
                dpcm.decompress(table, remaining_space)?;
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
    fn handle_setup_stream_control(&mut self, setup: &SetupStreamControl) {
        let state = self
            .stream_states
            .entry(setup.stream_id)
            .or_insert_with(|| StreamState {
                _stream_id: setup.stream_id,
                chip_type: ChipId::from_u8(0),
                instance: Instance::Primary,
                write_port: 0,
                write_command: 0,
                data_bank_id: 0,
                step_size: 1,
                step_base: 0,
                frequency_hz: None,
                start_offset: None,
                length_mode: LengthMode::Ignore {
                    reverse: false,
                    looped: false,
                },
                data_length: 0,
                block_end_pos: None,
                active: false,
                start_data_pos: 0,
                stream_start_sample: 0,
                last_emitted_step: None,
            });

        // `SetupStreamControl.chip_type` carries both chip id and instance.
        // Store the header `ChipId` in state and preserve the instance separately.
        state.chip_type = setup.chip_type.chip_id;
        state.instance = setup.chip_type.instance;
        state.write_port = setup.write_port;
        state.write_command = setup.write_command;
    }

    /// Handles SetStreamData command (0x91).
    fn handle_set_stream_data(&mut self, data: &SetStreamData) {
        let state = self
            .stream_states
            .entry(data.stream_id)
            .or_insert_with(|| StreamState {
                _stream_id: data.stream_id,
                chip_type: ChipId::from_u8(0),
                instance: Instance::Primary,
                write_port: 0,
                write_command: 0,
                data_bank_id: 0,
                step_size: 0,
                step_base: 0,
                frequency_hz: None,
                start_offset: None,
                length_mode: LengthMode::Ignore {
                    reverse: false,
                    looped: false,
                },
                data_length: 0,
                block_end_pos: None,
                active: false,
                start_data_pos: 0,
                stream_start_sample: 0,
                last_emitted_step: None,
            });

        state.data_bank_id = data.data_bank_id;
        state.step_size = data.step_size;
        state.step_base = data.step_base;
    }

    /// Handles SetStreamFrequency command (0x92).
    fn handle_set_stream_frequency(&mut self, freq: &SetStreamFrequency) {
        if let Some(state) = self.stream_states.get_mut(&freq.stream_id) {
            state.frequency_hz = Some(freq.frequency);
        }
    }

    /// Handles StartStream command (0x93).
    fn handle_start_stream(&mut self, start: &StartStream) -> Result<(), ParseError> {
        if !self.stream_states.contains_key(&start.stream_id) {
            return Ok(());
        }

        let step_base = self
            .stream_states
            .get(&start.stream_id)
            .map(|s| s.step_base)
            .unwrap_or(0);

        let base_offset = if start.data_start_offset >= 0 {
            start.data_start_offset as usize
        } else {
            0
        };
        let start_data_pos = base_offset + step_base as usize;

        if let Some(state) = self.stream_states.get_mut(&start.stream_id) {
            state.start_offset = Some(start.data_start_offset);
            state.length_mode = start.length_mode;
            state.data_length = start.data_length;
            state.block_end_pos = None; // StartStream doesn't use block boundaries
            state.active = true;
            state.start_data_pos = start_data_pos;
            state.stream_start_sample = self.current_sample;
            state.last_emitted_step = None;
        }
        Ok(())
    }

    /// Handles StopStream command (0x94).
    fn handle_stop_stream(&mut self, stop: &StopStream) {
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
        fast: &StartStreamFastCall,
    ) -> Result<(), ParseError> {
        let (data_bank_id, step_base) = if let Some(state) = self.stream_states.get(&fast.stream_id)
        {
            (state.data_bank_id, state.step_base)
        } else {
            return Ok(());
        };

        let (block_offset, block_size) =
            self.get_block_offset_and_size(data_bank_id, fast.block_id)?;
        let block_end = block_offset + block_size;
        let start_data_pos = block_offset + step_base as usize;

        if let Some(state) = self.stream_states.get_mut(&fast.stream_id) {
            state.active = true;
            state.start_data_pos = start_data_pos;
            state.block_end_pos = Some(block_end);
            state.stream_start_sample = self.current_sample;
            state.last_emitted_step = None;
            // FastCall always uses PlayUntilEnd
            state.length_mode = LengthMode::PlayUntilEnd {
                reverse: fast.flags.reverse,
                looped: fast.flags.looped,
            };
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
    /// Called after each Wait command.  For every active stream whose next
    /// scheduled write has been reached, the data-bank byte at the computed
    /// position is read, a chip-write command is emitted, and — if the stream
    /// has permanently ended — the `active` flag is cleared.  All playback
    /// positions and timing are derived from [`StreamCalc`] rather than being
    /// stored as mutable per-step state.
    fn generate_stream_writes(&mut self) -> Result<(), ParseError> {
        self.stream_id_scratch.clear();
        self.stream_id_scratch
            .extend(self.stream_states.keys().copied());

        for i in 0..self.stream_id_scratch.len() {
            let stream_id = self.stream_id_scratch[i];

            // Pre-fetch data_bank_end before borrowing stream_states.
            let data_bank_end = {
                let data_bank_id = match self.stream_states.get(&stream_id) {
                    Some(s) => s.data_bank_id,
                    None => continue,
                };
                self.uncompressed_streams
                    .get(&data_bank_id)
                    .map(|s| s.data.len())
                    .unwrap_or(0)
            };

            // Build the snapshot view; skip inactive / no-frequency streams.
            let snapshot = match self
                .stream_states
                .get(&stream_id)
                .and_then(|s| StreamSnapshot::from_state(s, data_bank_end))
            {
                Some(c) => c,
                None => continue,
            };

            // Determine which step is due at current_sample.
            //
            // `due_step` tells us whether a step's time-slot has arrived,
            // independently of whether the byte position is in-bank.
            // `due_write` additionally requires an in-bank position.
            //
            // Possible outcomes:
            //  - due_step = None, next_write_sample = None  → permanently ended
            //  - due_step = None, next_write_sample = Some  → not yet due
            //  - due_step = Some, due_write = None          → dry slot (out-of-bank)
            //  - due_step = Some, due_write = Some          → normal emit
            let due_s = snapshot.due_step(self.current_sample);
            let due_w = snapshot.due_write(self.current_sample);

            match (due_s, due_w) {
                (None, _) => {
                    // Either not due yet or permanently ended.
                    if snapshot.next_write_sample(self.current_sample).is_none()
                        && let Some(state) = self.stream_states.get_mut(&stream_id)
                    {
                        state.active = false;
                    }
                    continue;
                }
                (Some(dry_step), None) => {
                    // Dry slot: time-slot is consumed but no byte is in-bank.
                    // Advance last_emitted_step so the clock moves forward,
                    // but do NOT emit anything.
                    if let Some(state) = self.stream_states.get_mut(&stream_id) {
                        state.last_emitted_step = Some(dry_step);
                    }
                    continue;
                }
                (Some(_), Some((data_pos, step))) => {
                    // Normal emit: read byte from data bank and emit chip-write command.
                    if let Some(data) = self.read_stream_byte_at(snapshot.data_bank_id, data_pos)? {
                        if let Some(cmd) = Self::create_stream_write_command_static(
                            snapshot.chip_id,
                            snapshot.instance,
                            snapshot.write_port,
                            snapshot.write_command,
                            data,
                        ) {
                            self.pending_stream_writes.push(cmd);
                        }
                        // Record that this step has been emitted regardless of whether
                        // create_stream_write_command_static produced a command (the
                        // chip type may be unmapped, but the position must still advance).
                        if let Some(state) = self.stream_states.get_mut(&stream_id) {
                            state.last_emitted_step = Some(step);
                        }
                    } else {
                        // read_stream_byte_at returned None even though data_pos_for_step
                        // said the position was valid — should not normally happen, but
                        // treat it as a permanent end to avoid spinning.
                        if let Some(state) = self.stream_states.get_mut(&stream_id) {
                            state.active = false;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Processes a wait command, generating stream writes and splitting the wait as needed.
    ///
    /// This method handles large wait periods by:
    /// 1. Finding the next stream write time within the wait period
    /// 2. Emitting a partial wait up to that point
    /// 3. Generating the stream write
    /// 4. Saving remaining wait time for later emission
    fn process_wait_with_streams(
        &mut self,
        wait_samples: usize,
    ) -> Result<StreamResult, ParseError> {
        let target_sample = self.current_sample.saturating_add(wait_samples);

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
                self.pending_wait = Some(remaining_wait.min(u16::MAX as usize) as u16);
            }

            if wait_until_write > 0 {
                return Ok(StreamResult::Command(VgmCommand::WaitSamples(WaitSamples(
                    wait_until_write.min(u16::MAX as usize) as u16,
                ))));
            } else if !self.pending_stream_writes.is_empty() {
                let cmd = self.pending_stream_writes.remove(0);
                return Ok(StreamResult::Command(cmd));
            }
        }
        self.current_sample = target_sample;
        self.pending_wait = None;

        Ok(StreamResult::Command(VgmCommand::WaitSamples(WaitSamples(
            wait_samples.min(u16::MAX as usize) as u16,
        ))))
    }

    /// Finds the next stream write sample position that is after current_sample and at or before target_sample.
    fn find_next_stream_write_sample(&self, target_sample: usize) -> Option<usize> {
        let mut earliest: Option<usize> = None;

        for state in self.stream_states.values() {
            if !state.active {
                continue;
            }
            let data_bank_end = self
                .uncompressed_streams
                .get(&state.data_bank_id)
                .map(|s| s.data.len())
                .unwrap_or(0);
            let calc = match StreamSnapshot::from_state(state, data_bank_end) {
                Some(c) => c,
                None => continue,
            };
            if let Some(next) = calc.next_write_sample(self.current_sample)
                && next >= self.current_sample
                && next <= target_sample
            {
                match earliest {
                    None => earliest = Some(next),
                    Some(e) if next < e => earliest = Some(next),
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
            .get(&u8::from(StreamChipType::Ym2612Pcm))
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
        chip_id: ChipId,
        instance: Instance,
        write_port: u8,
        write_command: u8,
        data: u8,
    ) -> Option<VgmCommand> {
        match chip_id {
            ChipId::Sn76489 => Some(VgmCommand::Sn76489Write(
                instance,
                chip::PsgSpec { value: data },
            )),
            ChipId::Ym2413 => Some(VgmCommand::Ym2413Write(
                instance,
                chip::Ym2413Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ym2612 => Some(VgmCommand::Ym2612Write(
                instance,
                chip::Ym2612Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ym2151 => Some(VgmCommand::Ym2151Write(
                instance,
                chip::Ym2151Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::SegaPcm => Some(VgmCommand::SegaPcmWrite(
                instance,
                chip::SegaPcmSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::Rf5c68 => Some(VgmCommand::Rf5c68U8Write(
                instance,
                chip::Rf5c68U8Spec {
                    offset: write_command,
                    value: data,
                },
            )),
            ChipId::Ym2203 => Some(VgmCommand::Ym2203Write(
                instance,
                chip::Ym2203Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ym2608 => Some(VgmCommand::Ym2608Write(
                instance,
                chip::Ym2608Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ym2610 => Some(VgmCommand::Ym2610bWrite(
                instance,
                chip::Ym2610Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ym3812 => Some(VgmCommand::Ym3812Write(
                instance,
                chip::Ym3812Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ym3526 => Some(VgmCommand::Ym3526Write(
                instance,
                chip::Ym3526Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Y8950 => Some(VgmCommand::Y8950Write(
                instance,
                chip::Y8950Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ymf262 => Some(VgmCommand::Ymf262Write(
                instance,
                chip::Ymf262Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ymf278b => Some(VgmCommand::Ymf278bWrite(
                instance,
                chip::Ymf278bSpec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ymf271 => Some(VgmCommand::Ymf271Write(
                instance,
                chip::Ymf271Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Ymz280b => Some(VgmCommand::Ymz280bWrite(
                instance,
                chip::Ymz280bSpec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Rf5c164 => Some(VgmCommand::Rf5c164U8Write(
                instance,
                chip::Rf5c164U8Spec {
                    offset: write_command,
                    value: data,
                },
            )),
            ChipId::Pwm => Some(VgmCommand::PwmWrite(
                instance,
                chip::PwmSpec {
                    register: write_port & 0x0F,
                    value: ((write_command as u32) << 8) | (data as u32),
                },
            )),
            ChipId::Ay8910 => Some(VgmCommand::Ay8910Write(
                instance,
                chip::Ay8910Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::GbDmg => Some(VgmCommand::GbDmgWrite(
                instance,
                chip::GbDmgSpec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::NesApu => Some(VgmCommand::NesApuWrite(
                instance,
                chip::NesApuSpec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::MultiPcm => Some(VgmCommand::MultiPcmWrite(
                instance,
                chip::MultiPcmSpec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Upd7759 => Some(VgmCommand::Upd7759Write(
                instance,
                chip::Upd7759Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Okim6258 => Some(VgmCommand::Okim6258Write(
                instance,
                chip::Okim6258Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Okim6295 => Some(VgmCommand::Okim6295Write(
                instance,
                chip::Okim6295Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::K051649 => Some(VgmCommand::Scc1Write(
                instance,
                chip::Scc1Spec {
                    port: write_port,
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::K054539 => Some(VgmCommand::K054539Write(
                instance,
                chip::K054539Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::Huc6280 => Some(VgmCommand::Huc6280Write(
                instance,
                chip::Huc6280Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::C140 => Some(VgmCommand::C140Write(
                instance,
                chip::C140Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::K053260 => Some(VgmCommand::K053260Write(
                instance,
                chip::K053260Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Pokey => Some(VgmCommand::PokeyWrite(
                instance,
                chip::PokeySpec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Qsound => Some(VgmCommand::QsoundWrite(
                instance,
                chip::QsoundSpec {
                    register: write_command,
                    value: ((write_port as u16) << 8) | (data as u16),
                },
            )),
            ChipId::Scsp => Some(VgmCommand::ScspWrite(
                instance,
                chip::ScspSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::WonderSwan => Some(VgmCommand::WonderSwanWrite(
                instance,
                chip::WonderSwanSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::Vsu => Some(VgmCommand::VsuWrite(
                instance,
                chip::VsuSpec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::Saa1099 => Some(VgmCommand::Saa1099Write(
                instance,
                chip::Saa1099Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Es5503 => Some(VgmCommand::Es5503Write(
                instance,
                chip::Es5503Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::Es5506 => Some(VgmCommand::Es5506BEWrite(
                instance,
                chip::Es5506U8Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::X1010 => Some(VgmCommand::X1010Write(
                instance,
                chip::X1010Spec {
                    offset: ((write_port as u16) << 8) | (write_command as u16),
                    value: data,
                },
            )),
            ChipId::C352 => Some(VgmCommand::C352Write(
                instance,
                chip::C352Spec {
                    register: ((write_port as u16) << 8) | (write_command as u16),
                    value: data as u16,
                },
            )),
            ChipId::Ga20 => Some(VgmCommand::Ga20Write(
                instance,
                chip::Ga20Spec {
                    register: write_command,
                    value: data,
                },
            )),
            ChipId::Mikey => Some(VgmCommand::MikeyWrite(
                instance,
                chip::MikeySpec {
                    register: write_command,
                    value: data,
                },
            )),
            _ => None,
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
