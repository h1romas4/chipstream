//! `soundlog-micro` — Experimental C interface for MCU (Microcontroller Unit) targets.
//!
//! This binary wraps [`VgmCallbackStream`] with a set of `extern "C"` functions,
//! providing an interface suitable for bare-metal or embedded environments where
//! Rust's standard allocator and runtime may not be available in their usual form.
//!
//! # Design
//!
//! Two initialisation strategies are provided, each sharing the same singleton
//! state slot defined in [`common`]:
//!
//! ## fread-style ([`push_chunk`] module)
//!
//! - `stream_init`: Initialise from a **header-only** buffer (`0x100` bytes).
//!   Returns out-parameters so the C caller can manage its own read pointer into
//!   the command region.
//! - `stream_push_chunk`: Push one chunk (raw pointer + length) and drain
//!   available commands. The C caller advances its offset based on the return code.
//!
//! ## PSRAM-style ([`from_vgm`] module)
//!
//! - `stream_init_from_vgm`: Initialise from a **full VGM buffer** already
//!   resident in PSRAM (or any contiguous RAM). No out-parameters needed.
//! - `stream_next`: Advance the iterator by one step. The C caller loops until
//!   `EndOfStream` or an error is returned.
//!
//! All stream state is kept entirely on the Rust side; the C caller only
//! inspects the integer return value of each function.
//! Thread safety is intentionally omitted (singleton, single-threaded use).
//!
//! # Return codes for `stream_push_chunk` / `stream_next`
//!
//! | Value | `stream_push_chunk`                                        | `stream_next`                    |
//! |------:|------------------------------------------------------------|----------------------------------|
//! |   2   | Loop boundary crossed — C caller must rewind to loop point | *(not used)*                     |
//! |   1   | Normal progress — send the next consecutive chunk          | Command processed — call again   |
//! |   0   | Playback finished (`EndOfStream`)                          | Playback finished (`EndOfStream`)|
//! |  -1   | Initialisation / header-parse error                        | Not initialised / parse error    |
//! |  -2   | `push_chunk` error (e.g. buffer limit exceeded)            | *(not used)*                     |
//! |  -3   | Command-parse error during drain                           | Command-parse error              |

#![allow(unsafe_code)]

// ---------------------------------------------------------------------------
// mod common — shared status codes and singleton state
// ---------------------------------------------------------------------------

mod common {
    use std::mem::ManuallyDrop;
    use std::mem::MaybeUninit;

    use soundlog::VgmCallbackStream;

    /// C-compatible status codes returned by `stream_push_chunk` and `stream_next`.
    ///
    /// Positive = normal continuation, zero = finished, negative = error.
    #[repr(i8)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PushStatus {
        /// Loop boundary crossed — C caller must rewind its offset to `restart_pos`.
        /// (`stream_push_chunk` only)
        LoopRewind = 2,
        /// Normal progress — the caller should send the next chunk / call again.
        NeedsMore = 1,
        /// Playback has finished (`EndOfStream`).
        EndOfStream = 0,
        /// Header parse error, or the stream has not been initialised.
        HeaderParse = -1,
        /// `push_chunk` error (e.g. buffer size limit exceeded).
        /// (`stream_push_chunk` only)
        PushInvalid = -2,
        /// Command parse error encountered while draining the stream.
        IterParse = -3,
    }

    /// Internal state owned entirely by the Rust side.
    ///
    /// The C caller never accesses this directly; it only observes `i8` return
    /// values and out-parameters filled by the init functions.
    pub struct MicroState {
        /// The callback stream instance.
        pub cs: VgmCallbackStream<'static>,
        /// Loop count snapshot taken before each `stream_push_chunk` call.
        /// Used to detect a post-loop buffer clear vs. a mid-command chunk split.
        pub loops_before: u32,
        /// When `stream_init_from_vgm` is used with a zero-copy PSRAM buffer,
        /// the `Vec<u8>` that wraps the raw pointer is stored here wrapped in
        /// `ManuallyDrop` so that it is never dropped — and therefore never
        /// passed to the allocator's `free`.  `None` for all other init paths.
        ///
        /// # Safety invariant
        /// If `Some`, the inner `Vec` must NOT be taken out and dropped; it
        /// must only ever be forgotten or left in place until the process ends.
        #[allow(dead_code)]
        pub psram_vec: Option<ManuallyDrop<Vec<u8>>>,
    }

    /// Singleton [`MicroState`]. Uninitialised until an init function succeeds.
    pub static mut STATE: MaybeUninit<MicroState> = MaybeUninit::uninit();

    /// Tracks whether an init function has been called successfully.
    pub static mut INITIALIZED: bool = false;

    /// Returns a raw mutable pointer to the [`MicroState`] storage.
    ///
    /// Using a raw pointer instead of `&mut STATE` avoids the `static_mut_refs`
    /// lint introduced in Rust 2024 edition.
    #[inline]
    pub fn state_ptr() -> *mut MicroState {
        // SAFETY: STATE is a valid MaybeUninit<MicroState>; callers are
        // responsible for ensuring it has been initialised before dereferencing.
        unsafe { (*std::ptr::addr_of_mut!(STATE)).as_mut_ptr() }
    }
}

// ---------------------------------------------------------------------------
// mod common (continued) — stream_deinit shared by both interface styles
// ---------------------------------------------------------------------------

/// Release all stream resources and reset the singleton to uninitialised.
///
/// This function must be called when playback finishes (or is aborted) to
/// ensure that resources are freed correctly regardless of which init path
/// was used.
///
/// ## Why a dedicated deinit is necessary for the `from_vgm` path
///
/// `stream_init_from_vgm` forges a `Vec<u8>` that points directly at a PSRAM
/// buffer (zero-copy).  That `Vec` ends up owned by `VgmStreamSource::File`
/// inside `VgmCallbackStream`.  If `MicroState` were simply dropped, Rust
/// would call `Vec::drop` on that forged `Vec`, which would pass the PSRAM
/// pointer to the allocator's `free` — undefined behaviour.
///
/// The safe teardown sequence is:
///
/// 1. **Forget the sentinel** — `MicroState::psram_vec` holds a
///    `ManuallyDrop<Vec<u8>>` wrapping the same PSRAM pointer.  We call
///    `ManuallyDrop::drop` on it *only* to run any book-keeping, but since
///    `ManuallyDrop` never calls the inner destructor, this is a no-op for
///    `Vec`.  Actually we just leave it forgotten — `ManuallyDrop` guarantees
///    the inner `Vec` is never freed.
/// 2. **Neutralise the stream's Vec** — reach into `VgmCallbackStream` via
///    `stream_mut()` and call `VgmStream::reset()`, which clears internal
///    position state but does **not** drop `data`.  We then replace the
///    entire `MicroState` slot with `MaybeUninit::uninit()` using a raw
///    `ptr::write`, which overwrites without running the old drop glue.
///    Because we are overwriting with uninitialised bytes, no destructor for
///    the old value runs — preventing the spurious `free`.
/// 3. **Reset the flag** — set `INITIALIZED = false` so subsequent guard
///    checks in `stream_push_chunk` / `stream_next` reject calls immediately.
///
/// For the `push_chunk` path (`psram_vec` is `None`) there is no PSRAM
/// pointer involved, so the Vec inside the stream is a normal heap allocation.
/// In that case we use `MaybeUninit::assume_init_drop()` to run full drop
/// glue and free the heap memory properly.
///
/// # Safety
/// Must be called in a single-threaded context.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stream_deinit() {
    use common::{INITIALIZED, STATE, state_ptr};

    // Nothing to do if never initialised.
    if unsafe { !*std::ptr::addr_of!(INITIALIZED) } {
        return;
    }

    unsafe {
        let state = &mut *state_ptr();

        if state.psram_vec.is_some() {
            // from_vgm path: the Vec inside VgmCallbackStream points at PSRAM.
            // We must NOT let Rust drop it normally.
            //
            // Strategy: overwrite STATE with MaybeUninit::uninit() via a raw
            // pointer write.  ptr::write does not run drop glue on the value
            // being overwritten, so the forged Vec is silently discarded
            // (leaked) rather than freed — which is exactly what we want for
            // memory that belongs to the C/PSRAM side.
            //
            // The ManuallyDrop sentinel in psram_vec is also silently
            // discarded, which is correct because ManuallyDrop never calls
            // the inner destructor anyway.
            std::ptr::write(
                std::ptr::addr_of_mut!(STATE),
                std::mem::MaybeUninit::uninit(),
            );
        } else {
            // push_chunk path: the Vec inside VgmCallbackStream is a normal
            // heap allocation.  assume_init_drop() runs full drop glue and
            // frees it correctly.
            std::ptr::addr_of_mut!(STATE)
                .cast::<std::mem::MaybeUninit<common::MicroState>>()
                .as_mut()
                .unwrap_unchecked()
                .assume_init_drop();
        }

        *std::ptr::addr_of_mut!(INITIALIZED) = false;
    }
}

// ---------------------------------------------------------------------------
// mod push_chunk — fread-style C interface
// ---------------------------------------------------------------------------

/// fread-style interface: the C caller owns the read pointer and feeds the
/// stream header-first, then command-data in fixed-size chunks.
///
/// # C-side pseudo-code
///
/// ```text
/// uint8_t header_buf[0x100];
/// fread(header_buf, 1, 0x100, fp);
///
/// size_t cmd_start, cmd_end, restart_abs;
/// stream_init(header_buf, 0x100, 2, 4096, &cmd_start, &cmd_end, &restart_abs);
///
/// size_t offset = 0;
/// size_t restart_pos = restart_abs - cmd_start;
/// size_t region_len  = cmd_end - cmd_start;
///
/// while (offset < region_len) {
///     size_t  chunk_len = min(4096, region_len - offset);
///     uint8_t chunk_buf[chunk_len];
///     fseek(fp, cmd_start + offset, SEEK_SET);
///     fread(chunk_buf, 1, chunk_len, fp);
///
///     int8_t rc = stream_push_chunk(chunk_buf, chunk_len);
///     if      (rc == 2) { offset = restart_pos; }   // LoopRewind
///     else if (rc == 1) { offset += chunk_len;  }   // NeedsMore
///     else              { break; }                   // 0 = done, neg = error
/// }
/// ```
mod push_chunk {
    use std::hint::black_box;

    use soundlog::VgmCallbackStream;
    use soundlog::VgmHeader;
    use soundlog::VgmStream;
    use soundlog::chip::Ym2612Spec;
    use soundlog::chip::state::Sn76489State;
    use soundlog::chip::state::Ym2612State;
    use soundlog::vgm::command::Instance;
    use soundlog::vgm::stream::StreamResult;

    use super::common::{INITIALIZED, MicroState, PushStatus, state_ptr};

    /// Initialise the stream from a **header-only** VGM buffer (`0x100` bytes).
    ///
    /// Parses the VGM header and constructs a [`VgmCallbackStream`] stored in
    /// the shared singleton. Three out-parameters are written so that the C
    /// side can manage its own read pointer into the command region:
    ///
    /// - `out_cmd_start` — byte offset of the first command byte within the file.
    /// - `out_cmd_end`   — byte offset one past the last command byte (exclusive).
    /// - `out_restart`   — byte offset of the loop restart point within the file
    ///   (equals `out_cmd_start` when the track has no loop).
    ///
    /// # Arguments
    /// * `data`          — Pointer to the header buffer (`0x100` bytes).
    /// * `len`           — Length of the header buffer (should be `0x100`).
    /// * `loop_count`    — Number of playback loops. `0` means infinite.
    /// * `chunk_size`    — Bytes per `stream_push_chunk` call (informational only;
    ///   not stored — the C side manages chunking).
    /// * `out_cmd_start` — Written with the command-region start offset.
    /// * `out_cmd_end`   — Written with the command-region end offset (exclusive).
    /// * `out_restart`   — Written with the loop restart offset.
    ///
    /// # Return value
    /// `0` on success, `-1` if header parsing fails or `data` is null.
    ///
    /// # Safety
    /// - `data` must be valid for `len` bytes.
    /// - `out_cmd_start`, `out_cmd_end`, and `out_restart` must be valid non-null
    ///   writable pointers.
    /// - Must be called in a single-threaded context.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn stream_init(
        data: *const u8,
        len: usize,
        loop_count: u32,
        _chunk_size: usize,
        out_cmd_start: *mut usize,
        out_cmd_end: *mut usize,
        out_restart: *mut usize,
    ) -> i8 {
        if data.is_null() || len == 0 {
            return PushStatus::HeaderParse as i8;
        }

        // SAFETY: caller guarantees data is valid for len bytes.
        let vgm: &'static [u8] = unsafe { std::slice::from_raw_parts(data, len) };

        // Parse the VGM header.
        let header = match VgmHeader::from_bytes(vgm) {
            Ok(h) => h,
            Err(_) => return PushStatus::HeaderParse as i8,
        };

        // Compute command-region boundaries.
        let command_start = VgmHeader::command_start(header.version, header.data_offset);
        if header.eof_offset == 0 {
            return PushStatus::HeaderParse as i8;
        }
        // VGM spec: eof_offset stores (file_size - 4); absolute EOF = eof_offset + 4.
        // Note: `vgm` contains only the header bytes (0x100), not the full file,
        // so we do not validate eof against vgm.len().
        let eof = header.eof_offset as usize + 4;
        if command_start >= eof {
            return PushStatus::HeaderParse as i8;
        }

        // Compute the loop restart position as an absolute file offset.
        let restart_abs = VgmHeader::loop_pos_in_commands(
            header.loop_offset,
            header.version,
            header.data_offset,
            eof,
        )
        // loop_pos_in_commands returns an offset relative to command_start;
        // convert back to an absolute offset within the file.
        .map(|rel| command_start + rel)
        .unwrap_or(command_start);

        // Write out-parameters so the C caller can manage its own read pointer.
        unsafe {
            out_cmd_start.write(command_start);
            out_cmd_end.write(eof);
            out_restart.write(restart_abs);
        }

        // Translate loop_count: 0 means infinite (None).
        let lc: Option<u32> = if loop_count == 0 {
            None
        } else {
            Some(loop_count)
        };

        // Build the VgmCallbackStream.
        let inner = VgmStream::new();
        let mut cs: VgmCallbackStream<'static> = VgmCallbackStream::new(inner);
        cs.set_loop_count(lc);

        // Register YM2612 state tracking and write callback.
        cs.track_state::<Ym2612State>(Instance::Primary, 7_670_454.0);
        cs.on_write(|inst, spec: Ym2612Spec, sample, event| {
            black_box((inst, spec, sample, event));
        });

        // Register SN76489 state tracking and write callback.
        cs.track_state::<Sn76489State>(Instance::Primary, 3_579_545.0);
        cs.on_write(|inst, spec: soundlog::chip::PsgSpec, sample, event| {
            black_box((inst, spec, sample, event));
        });

        // Store state in the singleton slot via raw pointer to avoid
        // the static_mut_refs lint (Rust 2024 edition).
        unsafe {
            state_ptr().write(MicroState {
                cs,
                loops_before: 0,
                psram_vec: None,
            });
            *std::ptr::addr_of_mut!(INITIALIZED) = true;
        }

        0
    }

    /// Push one chunk of VGM command data into the stream and drain available
    /// commands.
    ///
    /// The C caller is responsible for:
    /// 1. Maintaining its own `offset` into the command region (obtained from
    ///    the out-parameters written by `stream_init`).
    /// 2. Slicing `chunk_ptr = file_base + cmd_start + offset` with length
    ///    `min(chunk_size, cmd_end - (cmd_start + offset))`.
    /// 3. Interpreting the return value and updating `offset` accordingly:
    ///    - `2` (`LoopRewind`) → `offset = out_restart - out_cmd_start`
    ///    - `1` (`NeedsMore`)  → `offset += chunk_len`
    ///    - `0` (`EndOfStream`) → stop
    ///    - negative → error
    ///
    /// # Arguments
    /// * `chunk_ptr` — Pointer to the first byte of the chunk to push.
    /// * `chunk_len` — Number of bytes in the chunk.
    ///
    /// # Return value
    /// An `i8` representing a `PushStatus` variant (see module-level table).
    ///
    /// # Safety
    /// - `chunk_ptr` must be valid for `chunk_len` bytes.
    /// - `stream_init` must have been called successfully beforehand.
    /// - Must be called in a single-threaded context.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn stream_push_chunk(chunk_ptr: *const u8, chunk_len: usize) -> i8 {
        if unsafe { !*std::ptr::addr_of!(INITIALIZED) } {
            return PushStatus::HeaderParse as i8;
        }

        if chunk_ptr.is_null() || chunk_len == 0 {
            return PushStatus::PushInvalid as i8;
        }

        // Obtain a mutable reference via raw pointer rather than directly from
        // `static mut`, which is disallowed by the static_mut_refs lint.
        let state = unsafe { &mut *state_ptr() };

        // SAFETY: caller guarantees chunk_ptr is valid for chunk_len bytes.
        let chunk: &[u8] = unsafe { std::slice::from_raw_parts(chunk_ptr, chunk_len) };

        // Snapshot the loop count before pushing so we can detect a post-loop
        // buffer clear (loop count increases) vs. a mid-command chunk split.
        state.loops_before = state.cs.stream().current_loop_count();

        if state.cs.push_chunk(chunk).is_err() {
            return PushStatus::PushInvalid as i8;
        }

        // Drain commands until the stream needs more data, finishes, or errors.
        loop {
            match state.cs.next() {
                Some(Ok(StreamResult::Command(cmd))) => {
                    // Forward through black_box to prevent the compiler from
                    // eliminating the work as dead code.
                    black_box(cmd);
                }
                Some(Ok(StreamResult::NeedsMoreData)) => {
                    // Distinguish two causes of NeedsMoreData:
                    //   Loop count increased → jump_to_loop_point() cleared the
                    //   buffer after EndOfData. Signal the C caller to rewind.
                    //
                    //   Loop count unchanged → a chunk boundary split a multi-byte
                    //   command. Signal the C caller to advance to the next chunk.
                    let looped = state.cs.stream().current_loop_count() > state.loops_before;
                    return if looped {
                        PushStatus::LoopRewind as i8
                    } else {
                        PushStatus::NeedsMore as i8
                    };
                }
                Some(Ok(StreamResult::EndOfStream)) => {
                    return PushStatus::EndOfStream as i8;
                }
                Some(Err(_)) => {
                    return PushStatus::IterParse as i8;
                }
                None => {
                    // Iterator exhausted without an explicit EndOfStream.
                    return PushStatus::EndOfStream as i8;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// mod from_vgm — PSRAM-style C interface
// ---------------------------------------------------------------------------

/// PSRAM-style interface: the full VGM file is already resident in a contiguous
/// memory region (PSRAM, heap, etc.). No offset management is required on the
/// C side — the Rust iterator drives the command region internally.
///
/// # C-side pseudo-code
///
/// ```text
/// // File already in PSRAM:
/// //   uint8_t *psram_buf = psram_alloc(vgm_len);
/// //   memcpy(psram_buf, vgm_flash_ptr, vgm_len);
///
/// stream_init_from_vgm(psram_buf, vgm_len, 2);
///
/// int8_t rc;
/// while ((rc = stream_next()) == 1) {}  // 0 = done, negative = error
/// ```
mod from_vgm {
    use std::hint::black_box;
    use std::mem::ManuallyDrop;

    use soundlog::VgmCallbackStream;
    use soundlog::chip::Ym2612Spec;
    use soundlog::chip::state::Sn76489State;
    use soundlog::chip::state::Ym2612State;
    use soundlog::vgm::command::Instance;
    use soundlog::vgm::stream::StreamResult;

    use super::common::{INITIALIZED, MicroState, PushStatus, state_ptr};

    /// Initialise the stream from a full VGM buffer already resident in PSRAM.
    ///
    /// Unlike `stream_init`, this function takes the **entire** VGM file at once.
    /// Header parsing, command-region detection, and loop-point calculation are
    /// all handled internally; the C caller needs no out-parameters.
    ///
    /// # Zero-copy design
    ///
    /// `VgmCallbackStream::from_vgm` requires a `Vec<u8>` internally.  Passing
    /// a `&[u8]` would trigger an implicit `to_vec()` — a full heap copy of the
    /// PSRAM data.  To avoid this:
    ///
    /// 1. A `Vec<u8>` is **forged** from the raw pointer via
    ///    `Vec::from_raw_parts(data as *mut u8, len, len)`.  This gives Rust a
    ///    `Vec` that points directly at the PSRAM buffer — no allocation, no copy.
    /// 2. The `Vec` is passed to `from_vgm`, which stores it inside the stream
    ///    as `VgmStreamSource::File { data, .. }`.
    /// 3. When that `Vec` is eventually dropped (e.g. when the singleton is
    ///    reinitialised), Rust's allocator would try to `free` the PSRAM pointer,
    ///    causing undefined behaviour.  To prevent this, a second
    ///    `ManuallyDrop<Vec<u8>>` wrapping the same pointer is stored in
    ///    `MicroState::psram_vec`.  Before any future `state_ptr().write(...)`,
    ///    the caller must ensure the old state is cleaned up without dropping
    ///    the inner Vec — currently guaranteed because this is a singleton that
    ///    is only ever initialised once per process lifetime.
    ///
    /// # Arguments
    /// * `data`       — Pointer to the start of the full VGM file in PSRAM.
    /// * `len`        — Total byte length of the buffer at `data`.
    /// * `loop_count` — Number of playback loops. `0` means infinite.
    ///
    /// # Return value
    /// `0` on success, `-1` on parse error or null pointer.
    ///
    /// # Safety
    /// - `data` must be valid for `len` bytes and remain live for the duration
    ///   of playback (i.e. for as long as `stream_next` is called).
    /// - `data` must NOT be freed or mutated while the stream is alive.
    /// - Must be called in a single-threaded context.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn stream_init_from_vgm(
        data: *const u8,
        len: usize,
        loop_count: u32,
    ) -> i8 {
        if data.is_null() || len == 0 {
            return PushStatus::HeaderParse as i8;
        }

        // Forge a Vec<u8> that points directly at the PSRAM buffer.
        // SAFETY:
        //   - `data` is valid for `len` bytes (caller guarantee).
        //   - capacity == len, so no reallocation will occur.
        //   - We store a ManuallyDrop sentinel below to prevent the allocator
        //     from freeing this pointer when the Vec inside the stream is dropped.
        let psram_vec: Vec<u8> = unsafe { Vec::from_raw_parts(data as *mut u8, len, len) };

        // Keep a ManuallyDrop handle to the same allocation so we can prevent
        // the allocator from freeing the PSRAM pointer at drop time.
        let psram_sentinel: ManuallyDrop<Vec<u8>> =
            ManuallyDrop::new(unsafe { Vec::from_raw_parts(data as *mut u8, len, len) });

        // Build the stream from the forged Vec — zero heap copy.
        let mut cs: VgmCallbackStream<'static> = match VgmCallbackStream::from_vgm(psram_vec) {
            Ok(s) => s,
            Err(_) => return PushStatus::HeaderParse as i8,
        };

        // Apply loop count (0 → infinite / None).
        cs.set_loop_count(if loop_count == 0 {
            None
        } else {
            Some(loop_count)
        });

        // Register YM2612 state tracking and write callback.
        cs.track_state::<Ym2612State>(Instance::Primary, 7_670_454.0);
        cs.on_write(|inst, spec: Ym2612Spec, sample, event| {
            black_box((inst, spec, sample, event));
        });

        // Register SN76489 state tracking and write callback.
        cs.track_state::<Sn76489State>(Instance::Primary, 3_579_545.0);
        cs.on_write(|inst, spec: soundlog::chip::PsgSpec, sample, event| {
            black_box((inst, spec, sample, event));
        });

        // Store into the shared singleton slot.
        // psram_sentinel keeps the ManuallyDrop handle alive alongside `cs`
        // so that the PSRAM pointer is never passed to the allocator's free.
        unsafe {
            state_ptr().write(MicroState {
                cs,
                loops_before: 0,
                psram_vec: Some(psram_sentinel),
            });
            *std::ptr::addr_of_mut!(INITIALIZED) = true;
        }

        0
    }

    /// Advance the stream iterator by one step.
    ///
    /// The C caller loops over this function until `EndOfStream` (`0`) or a
    /// negative error code is returned. No buffer management is required on
    /// the C side — the Rust iterator drives the command region internally.
    ///
    /// # Return value
    ///
    /// | Value | Meaning                           |
    /// |------:|-----------------------------------|
    /// |   1   | Command processed — call again    |
    /// |   0   | Playback finished (`EndOfStream`) |
    /// |  -1   | Stream not initialised            |
    /// |  -3   | Command-parse error               |
    ///
    /// # Safety
    /// - `stream_init_from_vgm` must have been called successfully beforehand.
    /// - Must be called in a single-threaded context.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn stream_next() -> i8 {
        if unsafe { !*std::ptr::addr_of!(INITIALIZED) } {
            return PushStatus::HeaderParse as i8;
        }
        let state = unsafe { &mut *state_ptr() };

        match state.cs.next() {
            Some(Ok(StreamResult::Command(cmd))) => {
                black_box(cmd);
                PushStatus::NeedsMore as i8
            }
            Some(Ok(StreamResult::EndOfStream)) | None => PushStatus::EndOfStream as i8,
            Some(Ok(StreamResult::NeedsMoreData)) => {
                // from_vgm owns the full buffer — should never happen.
                PushStatus::IterParse as i8
            }
            Some(Err(_)) => PushStatus::IterParse as i8,
        }
    }
}

// ---------------------------------------------------------------------------
// mod driver — test driver that models the C caller
// ---------------------------------------------------------------------------

/// Test drivers that model C-side callers for each interface style.
///
/// `main_push_chunk` exercises `stream_init` + `stream_push_chunk`.
/// `main_from_vgm`   exercises `stream_init_from_vgm` + `stream_next`.
mod driver {
    use super::common::PushStatus;
    use super::from_vgm::{stream_init_from_vgm, stream_next};
    use super::push_chunk::{stream_init, stream_push_chunk};
    use super::stream_deinit;

    /// Embedded VGM asset used only by the test driver.
    ///
    /// `main_push_chunk`: models sequential fread from storage (flash / SD card).
    /// `main_from_vgm`:   models a VGM file already allocated in PSRAM.
    static REMDME_VGM: &[u8] = include_bytes!("../../../soundlog/assets/vgm/REMDME.vgm");

    /// Stub that models `fopen` / `fread` on the C side.
    ///
    /// Returns the slice `REMDME_VGM[start .. start + length]`.
    /// In a real MCU implementation this would read from flash or an SD card.
    ///
    /// # Panics
    /// Panics if the requested range is out of bounds (same as a hard-fault on a
    /// bare-metal target that does not range-check its flash reads).
    #[allow(dead_code)]
    fn vgm_read(start: usize, length: usize) -> &'static [u8] {
        &REMDME_VGM[start..start + length]
    }

    // -------------------------------------------------------------------------
    // main_push_chunk — stream_init + stream_push_chunk (fread-style)
    // -------------------------------------------------------------------------

    /// Test driver that mirrors a C implementation using `stream_init` /
    /// `stream_push_chunk`.
    ///
    /// The VGM header is read with a fixed-size `fread` of `0x100` bytes.
    /// Command data is fed in `chunk_size`-byte increments via `vgm_read`,
    /// which stands in for an `fseek` + `fread` pair on the C side.
    #[allow(dead_code)]
    pub fn main_push_chunk() {
        // ----- parameters (would be chosen by the C caller) ----------------
        let loop_count: u32 = 2;
        let chunk_size: usize = 4096;

        // ----- read VGM header (fixed 0x100 bytes, like a C fread) ---------
        // In C: fread(header_buf, 1, 0x100, fp);
        //       stream_init(header_buf, 0x100, loop_count, chunk_size,
        //                   &cmd_start, &cmd_end, &restart_abs);
        let header_buf = vgm_read(0, 0x100);

        // ----- out-parameters filled by stream_init ------------------------
        let mut cmd_start: usize = 0;
        let mut cmd_end: usize = 0;
        let mut restart_abs: usize = 0;

        let init_rc = unsafe {
            stream_init(
                header_buf.as_ptr(),
                header_buf.len(),
                loop_count,
                chunk_size,
                &raw mut cmd_start,
                &raw mut cmd_end,
                &raw mut restart_abs,
            )
        };
        if init_rc != 0 {
            eprintln!("[push_chunk] stream_init failed: {init_rc}");
            std::process::exit(init_rc as i32);
        }
        println!(
            "[push_chunk] stream_init OK  cmd_start={cmd_start}  cmd_end={cmd_end}  restart_abs={restart_abs}"
        );

        // The C caller computes its offset relative to cmd_start.
        // restart_pos is expressed as an offset from cmd_start for convenience.
        let restart_pos: usize = restart_abs - cmd_start;
        let region_len: usize = cmd_end - cmd_start;

        // ----- push loop (mirrors C implementation) ------------------------
        let mut offset: usize = 0;
        let mut push_count: u32 = 0;

        loop {
            // Guard: if the entire region has been consumed without EndOfStream,
            // treat as end (should not happen under normal circumstances).
            if offset >= region_len {
                println!("[push_chunk] command region exhausted after {push_count} push(es)");
                break;
            }

            // Read the next chunk via the fread stub.
            // In C: chunk_len = min(chunk_size, region_len - offset);
            //        fread(chunk_buf, 1, chunk_len, fp);  /* seek to cmd_start+offset first */
            //        stream_push_chunk(chunk_buf, chunk_len);
            let chunk_len = chunk_size.min(region_len - offset);
            let chunk = vgm_read(cmd_start + offset, chunk_len);

            let rc = unsafe { stream_push_chunk(chunk.as_ptr(), chunk.len()) };
            push_count += 1;

            if rc == PushStatus::LoopRewind as i8 {
                // Loop boundary: rewind offset to the loop restart position.
                offset = restart_pos;
            } else if rc == PushStatus::NeedsMore as i8 {
                // Mid-command split: advance to the next consecutive bytes.
                offset += chunk_len;
            } else if rc == PushStatus::EndOfStream as i8 {
                println!("[push_chunk] EndOfStream after {push_count} push(es)");
                unsafe { stream_deinit() };
                return;
            } else {
                eprintln!("[push_chunk] stream_push_chunk error: {rc} after {push_count} push(es)");
                unsafe { stream_deinit() };
                std::process::exit(rc as i32);
            }
        }
    }

    // -------------------------------------------------------------------------
    // main_from_vgm — stream_init_from_vgm + stream_next (PSRAM-style)
    // -------------------------------------------------------------------------

    /// Test driver that mirrors a C implementation using `stream_init_from_vgm` /
    /// `stream_next`.
    ///
    /// The full VGM file is assumed to be already resident in PSRAM.  Here we
    /// simulate that by holding `REMDME_VGM` as a `'static` slice — no copy
    /// needed. In C this corresponds to a buffer filled by `psram_alloc` +
    /// `memcpy` (or DMA transfer) before calling `stream_init_from_vgm`.
    ///
    /// The C caller drives playback with a simple loop over `stream_next()`:
    ///
    /// ```text
    /// // Pseudo-C
    /// stream_init_from_vgm(psram_buf, vgm_len, 2);
    /// int8_t rc;
    /// while ((rc = stream_next()) == 1) {}   // 0 = done, negative = error
    /// ```
    #[allow(dead_code)]
    pub fn main_from_vgm() {
        // REMDME_VGM is 'static, so we can pass its pointer directly —
        // no allocation needed (simulates data already in PSRAM).
        // In C: stream_init_from_vgm(psram_buf, vgm_len, 2);
        let init_rc = unsafe { stream_init_from_vgm(REMDME_VGM.as_ptr(), REMDME_VGM.len(), 2) };
        if init_rc != 0 {
            eprintln!("[from_vgm] stream_init_from_vgm failed: {init_rc}");
            std::process::exit(init_rc as i32);
        }
        println!("[from_vgm] stream_init_from_vgm OK");

        // Drive playback — mirrors: while ((rc = stream_next()) == 1) {}
        let mut cmd_count: u32 = 0;
        loop {
            let rc = unsafe { stream_next() };
            if rc == PushStatus::NeedsMore as i8 {
                cmd_count += 1;
            } else if rc == PushStatus::EndOfStream as i8 {
                println!("[from_vgm] EndOfStream after {cmd_count} command(s)");
                unsafe { stream_deinit() };
                return;
            } else {
                eprintln!("[from_vgm] stream_next error: {rc} after {cmd_count} command(s)");
                unsafe { stream_deinit() };
                std::process::exit(rc as i32);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    println!("=== push_chunk x3 ===");
    for i in 1..=3 {
        println!("--- run {i} ---");
        driver::main_push_chunk();
    }

    println!();
    println!("=== from_vgm x3 ===");
    for i in 1..=3 {
        println!("--- run {i} ---");
        driver::main_from_vgm();
    }
}
