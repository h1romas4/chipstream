//! Chip register state tracking and analysis.
//!
//! This module provides traits and types for tracking sound chip register
//! state, detecting key on/off events, and extracting tone information.
//!
//! # Architecture
//!
//! The state tracking system is built around several core concepts:
//!
//! - **RegisterStorage**: Trait for abstracting register storage backends
//! - **ChannelState**: Generic channel state container
//! - **ChipState**: Trait for chip-specific state tracking logic
//! - **StateEvent**: Events emitted when notable state changes occur
//!
//! # Storage Backends
//!
//! Different chips have different register layouts and usage patterns.
//! This module provides multiple storage backends optimized for different scenarios:
//!
//! - `SparseStorage`: HashMap-based, flexible for large/sparse register spaces
//! - `ArrayStorage<N>`: Fixed-size array, fast for contiguous register spaces
//! - `CompactStorage`: Memory-efficient for tracking many chip instances
//!
//! # Implemented Chips
//!
//! The following sound chips have state tracking implementations:
//!
//! ## FM Synthesis Chips
//!
//! - **YM2612 (OPN2)**: 6-channel FM synthesis (Sega Genesis/Mega Drive)
//! - **YM2151 (OPM)**: 8-channel FM synthesis (Arcade systems)
//! - **YM2413 (OPLL)**: 9-channel FM synthesis (MSX, SMS FM Unit)
//!
//! ## PSG Chips
//!
//! - **SN76489**: 3 tone + 1 noise channels (Sega Master System, Game Gear)
//!
//! # Examples
//!
//! ```rust,ignore
//! use soundlog::chip::state::{Ym2612State, ChipState, StateEvent};
//!
//! let mut state = Ym2612State::new(7_670_454.0);
//!
//! // Set port for YM2612 (required for multi-port chips)
//! state.set_port(0);
//!
//! // Simulate register writes
//! state.on_register_write(0xA4, 0x22); // Block + fnum high
//! state.on_register_write(0xA0, 0x6D); // Fnum low
//!
//! // Key on triggers an event
//! if let Some(StateEvent::KeyOn { channel, tone }) = state.on_register_write(0x28, 0xF0) {
//!     println!("Channel {} key on: fnum={}, block={}", channel, tone.fnum, tone.block);
//! }
//! ```

pub mod ay8910;
pub mod channel;
pub mod chip_state;
pub mod gb_dmg;
pub mod huc6280;
pub mod k051649;
pub mod mikey;
pub mod nes_apu;
pub mod pcm;
pub mod pokey;
pub mod saa1099;
pub mod sn76489;
pub mod storage;
pub mod vsu;
pub mod wonderswan;
pub mod y8950;
pub mod ym2151;
pub mod ym2203;
pub mod ym2413;
pub mod ym2608;
pub mod ym2610b;
pub mod ym2612;
pub mod ym3526;
pub mod ym3812;
pub mod ymf262;
pub mod ymf271;
pub mod ymf278b;

// Re-export commonly used types for convenience
pub use ay8910::Ay8910State;
pub use channel::ChannelState;
pub use chip_state::ChipState;
pub use gb_dmg::GbDmgState;
pub use huc6280::Huc6280State;
pub use k051649::{K051649State, Scc1State};
pub use mikey::MikeyState;
pub use nes_apu::NesApuState;
pub use pcm::{
    C140State, C352State, Es5503State, Es5506State, Ga20State, K053260State, K054539State,
    MultiPcmState, Okim6258State, Okim6295State, QsoundState, Rf5c68State, Rf5c164State, ScspState,
    SegaPcmState, Upd7759State, X1010State, Ymz280bState,
};
pub use pokey::PokeyState;
pub use saa1099::Saa1099State;
pub use sn76489::Sn76489State;
pub use storage::{ArrayStorage, CompactStorage, RegisterStorage, SparseStorage};
pub use vsu::VsuState;
pub use wonderswan::WonderSwanState;
pub use y8950::Y8950State;
pub use ym2151::Ym2151State;
pub use ym2203::Ym2203State;
pub use ym2413::Ym2413State;
pub use ym2608::Ym2608State;
pub use ym2610b::Ym2610bState;
pub use ym2612::Ym2612State;
pub use ym3526::Ym3526State;
pub use ym3812::Ym3812State;
pub use ymf262::Ymf262State;
pub use ymf271::Ymf271State;
pub use ymf278b::Ymf278bState;
