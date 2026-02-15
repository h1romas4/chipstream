//! VGM stream with callback support for chip state tracking.
//!
//! This module provides `VgmCallbackStream`, a wrapper around `VgmStream` that
//! allows registering callbacks for chip register writes with automatic state
//! tracking and event detection.
//!
//! # Iterator Behavior
//!
//! `VgmCallbackStream` implements `Iterator` and wraps the underlying `VgmStream`.
//! When the underlying stream reaches `StreamResult::EndOfStream`, this iterator
//! returns `None` to terminate iteration. The `EndOfStream` result itself is not
//! yielded to the caller - instead, the iterator simply ends.
//!
//! # Examples
//!
//! ```
//! use soundlog::vgm::{VgmStream, VgmCallbackStream};
//! use soundlog::vgm::command::{Instance, VgmCommand};
//! use soundlog::vgm::stream::StreamResult;
//! use soundlog::chip::event::StateEvent;
//!
//! # let mut doc = soundlog::VgmDocument::default();
//! # doc.commands.push(VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
//! let stream = VgmStream::from_document(doc);
//!
//! let mut callback_stream = VgmCallbackStream::new(stream);
//! // Enable state tracking using State type
//! callback_stream.track_state::<soundlog::chip::state::Ym2612State>(Instance::Primary, 7_670_454.0);
//! // Register callback using Spec type
//! callback_stream.on_write(|inst, spec: soundlog::chip::Ym2612Spec, sample, event| {
//!         println!("YM2612[{:?}] @ sample {} reg={:02X} val={:02X}", inst, sample, spec.register, spec.value);
//!         if let Some(events) = event {
//!             for ev in events {
//!                 if let StateEvent::KeyOn { channel, tone } = ev {
//!                     println!("  -> KeyOn ch={} fnum={} block={}", channel, tone.fnum, tone.block);
//!                 }
//!             }
//!         }
//!     });
//!
//! for result in callback_stream {
//!     match result {
//!         Ok(StreamResult::Command(_)) => {
//!             // Callbacks have already been invoked
//!         }
//!         Err(e) => {
//!             eprintln!("Stream error: {:?}", e);
//!             break;
//!         }
//!         _ => {}
//!     }
//! }
//! // Iterator returns None on EndOfStream, terminating the loop
//! ```
#![allow(private_interfaces)]

use crate::VgmDocument;
use crate::binutil::ParseError;
use crate::chip;
use crate::chip::event::StateEvent;
use crate::chip::state::{
    Ay8910State, C140State, C352State, ChipState, Es5503State, Es5506State, Ga20State, GbDmgState,
    Huc6280State, K051649State, K053260State, K054539State, MikeyState, MultiPcmState, NesApuState,
    Okim6258State, Okim6295State, PokeyState, QsoundState, Rf5c68State, Rf5c164State, Saa1099State,
    Scc1State, ScspState, SegaPcmState, Sn76489State, Upd7759State, VsuState, WonderSwanState,
    X1010State, Y8950State, Ym2151State, Ym2203State, Ym2413State, Ym2608State, Ym2610bState,
    Ym2612State, Ym3526State, Ym3812State, Ymf262State, Ymf271State, Ymf278bState, Ymz280bState,
};
use crate::vgm::command::{
    Ay8910StereoMask, DataBlock, EndOfData, Instance, PcmRamWrite, ReservedU8, ReservedU16,
    ReservedU24, ReservedU32, UnknownSpec, VgmCommand, WaitSamples,
};
use crate::vgm::header::ChipInstances;
use crate::vgm::stream::{StreamResult, VgmStream};

type ChipCallback<'a, S> = Option<Box<dyn FnMut(Instance, S, u64, Option<Vec<StateEvent>>) + 'a>>;
type CommandCallback<'a, S> = Option<Box<dyn FnMut(S, u64, Option<Vec<StateEvent>>) + 'a>>;
type CommandRefCallback<'a, S> = Option<Box<dyn FnMut(&S, u64, Option<Vec<StateEvent>>) + 'a>>;
type AnyCallback<'a> = Option<Box<dyn FnMut(&VgmCommand, u64) + 'a>>;

/// Trait for chip specifications that can register write callbacks.
///
/// This trait allows generic callback registration using `on_write::<ChipSpec>()`
/// instead of requiring chip-specific method names.
///
/// # Examples
///
/// ```
/// use soundlog::vgm::VgmCallbackStream;
/// use soundlog::vgm::command::Instance;
/// use soundlog::chip;
/// # let mut doc = soundlog::VgmDocument::default();
/// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
/// let mut stream = VgmCallbackStream::from_document(doc);
///
/// // Enable state tracking using State type
/// stream.track_state::<chip::state::Ym2612State>(Instance::Primary, 7_670_454.0);
///
/// // Register callback using Spec type with type annotation
/// stream.on_write(|_inst, spec: chip::Ym2612Spec, _sample, _event| {
///     println!("YM2612 write: reg={:02X}", spec.register);
/// });
/// ```
pub trait WriteCallbackTarget: sealed::Sealed + 'static {
    /// Register the callback for this chip type.
    #[doc(hidden)]
    fn register_callback<'a, F>(callbacks: &mut Callbacks<'a>, callback: F)
    where
        F: FnMut(Instance, Self, u64, Option<Vec<StateEvent>>) + 'a,
        Self: Sized;
}

/// Trait for chip state types that can be tracked.
///
/// This trait allows generic state tracking using `track_state::<StateType>()`
/// instead of requiring chip-specific method names.
///
/// # Examples
///
/// ```
/// use soundlog::vgm::VgmCallbackStream;
/// use soundlog::vgm::command::Instance;
/// use soundlog::chip::state::Ym2612State;
/// # let mut doc = soundlog::VgmDocument::default();
/// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
/// let mut stream = VgmCallbackStream::from_document(doc);
///
/// // Enable state tracking using State type
/// stream.track_state::<Ym2612State>(Instance::Primary, 7_670_454.0);
/// ```
pub trait StateTracker: sealed::SealedState + 'static {
    /// The corresponding Spec type for this state.
    type Spec: WriteCallbackTarget;

    /// Initialize the state tracker for this chip type.
    #[doc(hidden)]
    fn init_tracker(trackers: &mut StateTrackers, instance: Instance, clock: f64);
}

// Seal the traits to prevent external implementations
mod sealed {
    pub trait Sealed {}
    pub trait SealedState {}

    // Spec types
    impl Sealed for crate::chip::Ym2612Spec {}
    impl Sealed for crate::chip::Ym2151Spec {}
    impl Sealed for crate::chip::Ym2203Spec {}
    impl Sealed for crate::chip::Ym2608Spec {}
    impl Sealed for crate::chip::Ym2610Spec {}
    impl Sealed for crate::chip::Ym2413Spec {}
    impl Sealed for crate::chip::Ym3812Spec {}
    impl Sealed for crate::chip::Ym3526Spec {}
    impl Sealed for crate::chip::Y8950Spec {}
    impl Sealed for crate::chip::PsgSpec {}
    impl Sealed for crate::chip::Ay8910Spec {}
    impl Sealed for crate::chip::GbDmgSpec {}
    impl Sealed for crate::chip::NesApuSpec {}
    impl Sealed for crate::chip::Huc6280Spec {}
    impl Sealed for crate::chip::SegaPcmSpec {}
    impl Sealed for crate::chip::Rf5c68U8Spec {}
    impl Sealed for crate::chip::Rf5c68U16Spec {}
    impl Sealed for crate::chip::Rf5c164U8Spec {}
    impl Sealed for crate::chip::Rf5c164U16Spec {}
    impl Sealed for crate::chip::PwmSpec {}
    impl Sealed for crate::chip::MultiPcmSpec {}
    impl Sealed for crate::chip::MultiPcmBankSpec {}
    impl Sealed for crate::chip::Upd7759Spec {}
    impl Sealed for crate::chip::Okim6258Spec {}
    impl Sealed for crate::chip::Okim6295Spec {}
    impl Sealed for crate::chip::K054539Spec {}
    impl Sealed for crate::chip::C140Spec {}
    impl Sealed for crate::chip::K053260Spec {}
    impl Sealed for crate::chip::PokeySpec {}
    impl Sealed for crate::chip::QsoundSpec {}
    impl Sealed for crate::chip::ScspSpec {}
    impl Sealed for crate::chip::WonderSwanSpec {}
    impl Sealed for crate::chip::VsuSpec {}
    impl Sealed for crate::chip::Saa1099Spec {}
    impl Sealed for crate::chip::Es5503Spec {}
    impl Sealed for crate::chip::Es5506U8Spec {}
    impl Sealed for crate::chip::Es5506U16Spec {}
    impl Sealed for crate::chip::X1010Spec {}
    impl Sealed for crate::chip::C352Spec {}
    impl Sealed for crate::chip::Ga20Spec {}
    impl Sealed for crate::chip::MikeySpec {}
    impl Sealed for crate::chip::GameGearPsgSpec {}
    impl Sealed for crate::chip::K051649Spec {}
    impl Sealed for crate::chip::Scc1Spec {}
    impl Sealed for crate::chip::Ymf262Spec {}
    impl Sealed for crate::chip::Ymf278bSpec {}
    impl Sealed for crate::chip::Ymf271Spec {}
    impl Sealed for crate::chip::Ymz280bSpec {}

    // State types
    impl SealedState for crate::chip::state::Ym2612State {}
    impl SealedState for crate::chip::state::Ym2151State {}
    impl SealedState for crate::chip::state::Ym2203State {}
    impl SealedState for crate::chip::state::Ym2608State {}
    impl SealedState for crate::chip::state::Ym2610bState {}
    impl SealedState for crate::chip::state::Ym2413State {}
    impl SealedState for crate::chip::state::Ym3812State {}
    impl SealedState for crate::chip::state::Ym3526State {}
    impl SealedState for crate::chip::state::Y8950State {}
    impl SealedState for crate::chip::state::Sn76489State {}
    impl SealedState for crate::chip::state::Ay8910State {}
    impl SealedState for crate::chip::state::GbDmgState {}
    impl SealedState for crate::chip::state::NesApuState {}
    impl SealedState for crate::chip::state::Huc6280State {}
    impl SealedState for crate::chip::state::SegaPcmState {}
    impl SealedState for crate::chip::state::Rf5c68State {}
    impl SealedState for crate::chip::state::Rf5c164State {}

    impl SealedState for crate::chip::state::MultiPcmState {}
    impl SealedState for crate::chip::state::Upd7759State {}
    impl SealedState for crate::chip::state::Okim6258State {}
    impl SealedState for crate::chip::state::Okim6295State {}
    impl SealedState for crate::chip::state::K054539State {}
    impl SealedState for crate::chip::state::C140State {}
    impl SealedState for crate::chip::state::K053260State {}
    impl SealedState for crate::chip::state::PokeyState {}
    impl SealedState for crate::chip::state::QsoundState {}
    impl SealedState for crate::chip::state::ScspState {}
    impl SealedState for crate::chip::state::WonderSwanState {}
    impl SealedState for crate::chip::state::VsuState {}
    impl SealedState for crate::chip::state::Saa1099State {}
    impl SealedState for crate::chip::state::Es5503State {}
    impl SealedState for crate::chip::state::Es5506State {}
    impl SealedState for crate::chip::state::X1010State {}
    impl SealedState for crate::chip::state::C352State {}
    impl SealedState for crate::chip::state::Ga20State {}
    impl SealedState for crate::chip::state::MikeyState {}
    impl SealedState for crate::chip::state::K051649State {}
    impl SealedState for crate::chip::state::Ymf262State {}
    impl SealedState for crate::chip::state::Ymf278bState {}
    impl SealedState for crate::chip::state::Ymf271State {}
    impl SealedState for crate::chip::state::Ymz280bState {}
}

macro_rules! impl_callback_and_state {
    ($spec_type:ty, $state_type:ty, $callback_field:ident, $tracker_field:ident) => {
        impl WriteCallbackTarget for $spec_type {
            fn register_callback<'a, F>(callbacks: &mut Callbacks<'a>, callback: F)
            where
                F: FnMut(Instance, Self, u64, Option<Vec<StateEvent>>) + 'a,
            {
                callbacks.$callback_field = Some(Box::new(callback));
            }
        }

        impl StateTracker for $state_type {
            type Spec = $spec_type;

            fn init_tracker(trackers: &mut StateTrackers, instance: Instance, clock: f64) {
                trackers.$tracker_field[instance as usize] = Some(<$state_type>::new(clock));
            }
        }
    };
}

// Implement WriteCallbackTarget and StateTracker for all chip types
impl_callback_and_state!(chip::Ym2612Spec, Ym2612State, on_ym2612_write, ym2612);
impl_callback_and_state!(chip::Ym2151Spec, Ym2151State, on_ym2151_write, ym2151);
impl_callback_and_state!(chip::Ym2203Spec, Ym2203State, on_ym2203_write, ym2203);
impl_callback_and_state!(chip::Ym2608Spec, Ym2608State, on_ym2608_write, ym2608);
impl_callback_and_state!(chip::Ym2610Spec, Ym2610bState, on_ym2610b_write, ym2610b);
impl_callback_and_state!(chip::Ym2413Spec, Ym2413State, on_ym2413_write, ym2413);
impl_callback_and_state!(chip::Ym3812Spec, Ym3812State, on_ym3812_write, ym3812);
impl_callback_and_state!(chip::Ym3526Spec, Ym3526State, on_ym3526_write, ym3526);
impl_callback_and_state!(chip::Y8950Spec, Y8950State, on_y8950_write, y8950);
impl_callback_and_state!(chip::PsgSpec, Sn76489State, on_sn76489_write, sn76489);
impl_callback_and_state!(chip::Ay8910Spec, Ay8910State, on_ay8910_write, ay8910);
impl_callback_and_state!(chip::Huc6280Spec, Huc6280State, on_huc6280_write, huc6280);
impl_callback_and_state!(chip::PokeySpec, PokeyState, on_pokey_write, pokey);
impl_callback_and_state!(chip::Saa1099Spec, Saa1099State, on_saa1099_write, saa1099);
impl_callback_and_state!(
    chip::WonderSwanSpec,
    WonderSwanState,
    on_wonder_swan_write,
    wonderswan
);
impl_callback_and_state!(chip::VsuSpec, VsuState, on_vsu_write, vsu);
impl_callback_and_state!(chip::MikeySpec, MikeyState, on_mikey_write, mikey);
impl_callback_and_state!(chip::K051649Spec, K051649State, on_k051649_write, k051649);
impl_callback_and_state!(chip::Ymf262Spec, Ymf262State, on_ymf262_write, ymf262);
impl_callback_and_state!(chip::Ymf271Spec, Ymf271State, on_ymf271_write, ymf271);
impl_callback_and_state!(chip::Ymf278bSpec, Ymf278bState, on_ymf278b_write, ymf278b);
impl_callback_and_state!(chip::GbDmgSpec, GbDmgState, on_gb_dmg_write, gb_dmg);
impl_callback_and_state!(chip::NesApuSpec, NesApuState, on_nes_apu_write, nes_apu);
impl_callback_and_state!(chip::SegaPcmSpec, SegaPcmState, on_sega_pcm_write, sega_pcm);
impl_callback_and_state!(chip::Rf5c68U8Spec, Rf5c68State, on_rf5c68_u8_write, rf5c68);
impl_callback_and_state!(chip::QsoundSpec, QsoundState, on_qsound_write, qsound);
impl_callback_and_state!(chip::ScspSpec, ScspState, on_scsp_write, scsp);
impl_callback_and_state!(chip::Es5503Spec, Es5503State, on_es5503_write, es5503);
impl_callback_and_state!(chip::Es5506U8Spec, Es5506State, on_es5506_u8_write, es5506);
impl_callback_and_state!(chip::X1010Spec, X1010State, on_x1_010_write, x1_010);
impl_callback_and_state!(chip::C352Spec, C352State, on_c352_write, c352);
impl_callback_and_state!(chip::Ga20Spec, Ga20State, on_ga20_write, ga20);
impl_callback_and_state!(chip::Ymz280bSpec, Ymz280bState, on_ymz280b_write, ymz280b);
impl_callback_and_state!(
    chip::MultiPcmSpec,
    MultiPcmState,
    on_multi_pcm_write,
    multi_pcm
);
impl_callback_and_state!(chip::Upd7759Spec, Upd7759State, on_upd7759_write, upd7759);
impl_callback_and_state!(
    chip::Okim6258Spec,
    Okim6258State,
    on_okim6258_write,
    okim6258
);
impl_callback_and_state!(
    chip::Okim6295Spec,
    Okim6295State,
    on_okim6295_write,
    okim6295
);
impl_callback_and_state!(chip::K054539Spec, K054539State, on_k054539_write, k054539);
impl_callback_and_state!(chip::C140Spec, C140State, on_c140_write, c140);
impl_callback_and_state!(chip::K053260Spec, K053260State, on_k053260_write, k053260);
// Scc1State is a type alias for K051649State, so we only implement WriteCallbackTarget
impl WriteCallbackTarget for chip::Scc1Spec {
    fn register_callback<'a, F>(callbacks: &mut Callbacks<'a>, callback: F)
    where
        F: FnMut(Instance, Self, u64, Option<Vec<StateEvent>>) + 'a,
    {
        callbacks.on_scc1_write = Some(Box::new(callback));
    }
}
// Rf5c68U16Spec shares the same state as Rf5c68U8Spec
impl WriteCallbackTarget for chip::Rf5c68U16Spec {
    fn register_callback<'a, F>(callbacks: &mut Callbacks<'a>, callback: F)
    where
        F: FnMut(Instance, Self, u64, Option<Vec<StateEvent>>) + 'a,
    {
        callbacks.on_rf5c68_u16_write = Some(Box::new(callback));
    }
}
impl_callback_and_state!(
    chip::Rf5c164U8Spec,
    Rf5c164State,
    on_rf5c164_u8_write,
    rf5c164
);
// Rf5c164U16Spec shares the same state as Rf5c164U8Spec
impl WriteCallbackTarget for chip::Rf5c164U16Spec {
    fn register_callback<'a, F>(callbacks: &mut Callbacks<'a>, callback: F)
    where
        F: FnMut(Instance, Self, u64, Option<Vec<StateEvent>>) + 'a,
    {
        callbacks.on_rf5c164_u16_write = Some(Box::new(callback));
    }
}
// Es5506U16Spec shares the same state as Es5506U8Spec
impl WriteCallbackTarget for chip::Es5506U16Spec {
    fn register_callback<'a, F>(callbacks: &mut Callbacks<'a>, callback: F)
    where
        F: FnMut(Instance, Self, u64, Option<Vec<StateEvent>>) + 'a,
    {
        callbacks.on_es5506_u16_write = Some(Box::new(callback));
    }
}

// Special cases that don't have state tracking
macro_rules! impl_write_callback_target_no_state {
    ($spec_type:ty, $callback_field:ident) => {
        impl WriteCallbackTarget for $spec_type {
            fn register_callback<'a, F>(callbacks: &mut Callbacks<'a>, callback: F)
            where
                F: FnMut(Instance, Self, u64, Option<Vec<StateEvent>>) + 'a,
            {
                callbacks.$callback_field = Some(Box::new(callback));
            }
        }
    };
}
impl_write_callback_target_no_state!(chip::PwmSpec, on_pwm_write);
impl_write_callback_target_no_state!(chip::MultiPcmBankSpec, on_multi_pcm_bank_write);
impl_write_callback_target_no_state!(chip::GameGearPsgSpec, on_game_gear_psg_write);

/// State trackers for various sound chips
/// Each chip type supports up to 2 instances (Primary and Secondary)
#[derive(Default)]
struct StateTrackers {
    ym2612: [Option<Ym2612State>; 2],
    ym2151: [Option<Ym2151State>; 2],
    ym2203: [Option<Ym2203State>; 2],
    ym2608: [Option<Ym2608State>; 2],
    ym2610b: [Option<Ym2610bState>; 2],
    ym2413: [Option<Ym2413State>; 2],
    ym3812: [Option<Ym3812State>; 2],
    ym3526: [Option<Ym3526State>; 2],
    y8950: [Option<Y8950State>; 2],
    ymf262: [Option<Ymf262State>; 2],
    ymf271: [Option<Ymf271State>; 2],
    ymf278b: [Option<Ymf278bState>; 2],
    sn76489: [Option<Sn76489State>; 2],
    ay8910: [Option<Ay8910State>; 2],
    gb_dmg: [Option<GbDmgState>; 2],
    nes_apu: [Option<NesApuState>; 2],
    huc6280: [Option<Huc6280State>; 2],
    pokey: [Option<PokeyState>; 2],
    saa1099: [Option<Saa1099State>; 2],
    wonderswan: [Option<WonderSwanState>; 2],
    vsu: [Option<VsuState>; 2],
    mikey: [Option<MikeyState>; 2],
    k051649: [Option<K051649State>; 2],
    scc1: [Option<Scc1State>; 2],
    sega_pcm: [Option<SegaPcmState>; 2],
    rf5c68: [Option<Rf5c68State>; 2],
    rf5c164: [Option<Rf5c164State>; 2],
    multi_pcm: [Option<MultiPcmState>; 2],
    upd7759: [Option<Upd7759State>; 2],
    okim6258: [Option<Okim6258State>; 2],
    okim6295: [Option<Okim6295State>; 2],
    k054539: [Option<K054539State>; 2],
    c140: [Option<C140State>; 2],
    c352: [Option<C352State>; 2],
    k053260: [Option<K053260State>; 2],
    qsound: [Option<QsoundState>; 2],
    scsp: [Option<ScspState>; 2],
    es5503: [Option<Es5503State>; 2],
    es5506: [Option<Es5506State>; 2],
    x1_010: [Option<X1010State>; 2],
    ga20: [Option<Ga20State>; 2],
    ymz280b: [Option<Ymz280bState>; 2],
}

/// Callback functions for chip write events
#[derive(Default)]
struct Callbacks<'a> {
    on_ym2612_write: ChipCallback<'a, chip::Ym2612Spec>,
    on_ym2151_write: ChipCallback<'a, chip::Ym2151Spec>,
    on_ym2203_write: ChipCallback<'a, chip::Ym2203Spec>,
    on_ym2608_write: ChipCallback<'a, chip::Ym2608Spec>,
    on_ym2610b_write: ChipCallback<'a, chip::Ym2610Spec>,
    on_ym2413_write: ChipCallback<'a, chip::Ym2413Spec>,
    on_ym3812_write: ChipCallback<'a, chip::Ym3812Spec>,
    on_ym3526_write: ChipCallback<'a, chip::Ym3526Spec>,
    on_y8950_write: ChipCallback<'a, chip::Y8950Spec>,
    on_sn76489_write: ChipCallback<'a, chip::PsgSpec>,
    on_ay8910_write: ChipCallback<'a, chip::Ay8910Spec>,
    on_gb_dmg_write: ChipCallback<'a, chip::GbDmgSpec>,
    on_nes_apu_write: ChipCallback<'a, chip::NesApuSpec>,
    on_huc6280_write: ChipCallback<'a, chip::Huc6280Spec>,
    on_sega_pcm_write: ChipCallback<'a, chip::SegaPcmSpec>,
    on_rf5c68_u8_write: ChipCallback<'a, chip::Rf5c68U8Spec>,
    on_rf5c68_u16_write: ChipCallback<'a, chip::Rf5c68U16Spec>,
    on_rf5c164_u8_write: ChipCallback<'a, chip::Rf5c164U8Spec>,
    on_rf5c164_u16_write: ChipCallback<'a, chip::Rf5c164U16Spec>,
    on_pwm_write: ChipCallback<'a, chip::PwmSpec>,
    on_multi_pcm_write: ChipCallback<'a, chip::MultiPcmSpec>,
    on_multi_pcm_bank_write: ChipCallback<'a, chip::MultiPcmBankSpec>,
    on_upd7759_write: ChipCallback<'a, chip::Upd7759Spec>,
    on_okim6258_write: ChipCallback<'a, chip::Okim6258Spec>,
    on_okim6295_write: ChipCallback<'a, chip::Okim6295Spec>,
    on_k054539_write: ChipCallback<'a, chip::K054539Spec>,
    on_c140_write: ChipCallback<'a, chip::C140Spec>,
    on_k053260_write: ChipCallback<'a, chip::K053260Spec>,
    on_pokey_write: ChipCallback<'a, chip::PokeySpec>,
    on_qsound_write: ChipCallback<'a, chip::QsoundSpec>,
    on_scsp_write: ChipCallback<'a, chip::ScspSpec>,
    on_wonder_swan_write: ChipCallback<'a, chip::WonderSwanSpec>,
    on_vsu_write: ChipCallback<'a, chip::VsuSpec>,
    on_saa1099_write: ChipCallback<'a, chip::Saa1099Spec>,
    on_es5503_write: ChipCallback<'a, chip::Es5503Spec>,
    on_es5506_u8_write: ChipCallback<'a, chip::Es5506U8Spec>,
    on_es5506_u16_write: ChipCallback<'a, chip::Es5506U16Spec>,
    on_x1_010_write: ChipCallback<'a, chip::X1010Spec>,
    on_c352_write: ChipCallback<'a, chip::C352Spec>,
    on_ga20_write: ChipCallback<'a, chip::Ga20Spec>,
    on_mikey_write: ChipCallback<'a, chip::MikeySpec>,
    on_game_gear_psg_write: ChipCallback<'a, chip::GameGearPsgSpec>,
    on_k051649_write: ChipCallback<'a, chip::K051649Spec>,
    on_scc1_write: ChipCallback<'a, chip::Scc1Spec>,
    on_ymf262_write: ChipCallback<'a, chip::Ymf262Spec>,
    on_ymf278b_write: ChipCallback<'a, chip::Ymf278bSpec>,
    on_ymf271_write: ChipCallback<'a, chip::Ymf271Spec>,
    on_ymz280b_write: ChipCallback<'a, chip::Ymz280bSpec>,
    on_ay8910_stereo_mask: CommandCallback<'a, Ay8910StereoMask>,
    on_reserved_u8_write: CommandCallback<'a, ReservedU8>,
    on_reserved_u16_write: CommandCallback<'a, ReservedU16>,
    on_reserved_u24_write: CommandCallback<'a, ReservedU24>,
    on_reserved_u32_write: CommandCallback<'a, ReservedU32>,
    on_unknown_command: CommandCallback<'a, UnknownSpec>,
    on_wait_samples: CommandCallback<'a, WaitSamples>,
    on_data_block: CommandRefCallback<'a, DataBlock>,
    on_end_of_data: CommandCallback<'a, EndOfData>,
    on_pcm_ram_write: CommandRefCallback<'a, PcmRamWrite>,
    on_any_command: AnyCallback<'a>,
}

/// A wrapper around `VgmStream` that provides callback support for chip register writes
/// with automatic state tracking and event detection.
///
/// # Iterator Behavior
///
/// `VgmCallbackStream` implements `Iterator` and wraps the underlying `VgmStream`.
/// When the underlying stream reaches `StreamResult::EndOfStream`, this iterator
/// returns `None` to terminate iteration. The `EndOfStream` result itself is not
/// yielded to the caller - instead, the iterator simply ends.
///
/// # Examples
///
/// ```
/// use soundlog::vgm::VgmCallbackStream;
/// use soundlog::vgm::command::{Instance, VgmCommand};
/// use soundlog::vgm::stream::StreamResult;
/// use soundlog::chip::event::StateEvent;
///
/// # let mut doc = soundlog::VgmDocument::default();
/// # doc.commands.push(VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
/// let mut callback_stream = VgmCallbackStream::from_document(doc);
/// callback_stream.track_state::<soundlog::chip::state::Ym2612State>(Instance::Primary, 7_670_454.0);
/// callback_stream.on_write(|inst, spec: soundlog::chip::Ym2612Spec, sample, event| {
///         println!("YM2612[{:?}] @ sample {} reg={:02X} val={:02X}", inst, sample, spec.register, spec.value);
///         if let Some(events) = event {
///             for ev in events {
///                 if let StateEvent::KeyOn { channel, tone } = ev {
///                     println!("  -> KeyOn ch={} fnum={} block={}", channel, tone.fnum, tone.block);
///                 }
///             }
///         }
///     });
///
/// // Process stream - iterator returns None on EndOfStream
/// for _result in callback_stream {
///     // Callbacks are invoked automatically
/// }
/// ```
pub struct VgmCallbackStream<'a> {
    /// The underlying VGM stream
    stream: VgmStream,
    /// State trackers for each chip instance
    state_trackers: StateTrackers,
    /// Registered callbacks
    callbacks: Callbacks<'a>,
}

impl<'a> VgmCallbackStream<'a> {
    /// Creates a new callback stream from a VGM stream.
    ///
    /// # Arguments
    ///
    /// * `stream` - The underlying VGM stream
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::{VgmStream, VgmCallbackStream};
    /// # let mut doc = soundlog::VgmDocument::default();
    /// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
    /// let stream = VgmStream::from_document(doc);
    /// let callback_stream = VgmCallbackStream::new(stream);
    /// ```
    pub fn new(stream: VgmStream) -> Self {
        Self {
            stream,
            state_trackers: StateTrackers::default(),
            callbacks: Callbacks::default(),
        }
    }

    /// Creates a new callback stream directly from a VGM document.
    ///
    /// This is a convenience method that creates a `VgmStream` from the document
    /// and wraps it in a `VgmCallbackStream`.
    ///
    /// # Arguments
    ///
    /// * `doc` - The VGM document to stream
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::VgmCallbackStream;
    /// use soundlog::vgm::command::{Instance, VgmCommand};
    /// use soundlog::vgm::stream::StreamResult;
    /// use soundlog::chip::event::StateEvent;
    ///
    /// # let mut doc = soundlog::VgmDocument::default();
    /// # doc.commands.push(VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
    /// let mut callback_stream = VgmCallbackStream::from_document(doc);
    /// // Enable state tracking using State type
    /// callback_stream.track_state::<soundlog::chip::state::Ym2612State>(Instance::Primary, 7_670_454.0);
    /// callback_stream.on_write(|inst, spec: soundlog::chip::Ym2612Spec, sample, event| {
    ///     println!("YM2612[{:?}] @ sample {} reg={:02X} val={:02X}", inst, sample, spec.register, spec.value);
    ///     if let Some(events) = event {
    ///         for ev in events {
    ///             if let StateEvent::KeyOn { channel, tone } = ev {
    ///                 println!("  -> KeyOn ch={} fnum={} block={}", channel, tone.fnum, tone.block);
    ///             }
    ///         }
    ///     }
    /// });
    ///
    /// // Process stream - iterator returns None on EndOfStream
    /// for _result in callback_stream {
    ///     // Callbacks are invoked automatically
    /// }
    /// ```
    pub fn from_document(doc: VgmDocument) -> Self {
        let stream = VgmStream::from_document(doc);
        Self::new(stream)
    }

    /// Returns a reference to the underlying stream.
    pub fn stream(&self) -> &VgmStream {
        &self.stream
    }

    /// Returns a mutable reference to the underlying stream.
    pub fn stream_mut(&mut self) -> &mut VgmStream {
        &mut self.stream
    }

    /// Set the loop count for the underlying stream.
    ///
    /// This controls how many times the stream will loop when it reaches the loop point.
    /// `None` means infinite loops, `Some(n)` means loop n times.
    ///
    /// # Arguments
    ///
    /// * `count` - The number of loops, or `None` for infinite
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::VgmCallbackStream;
    /// # let mut doc = soundlog::VgmDocument::default();
    /// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
    /// let mut callback_stream = VgmCallbackStream::from_document(doc);
    /// callback_stream.set_loop_count(Some(1)); // Play once, no loops
    /// ```
    pub fn set_loop_count(&mut self, count: Option<u32>) {
        self.stream.set_loop_count(count);
    }

    /// Enable state tracking for all chips in the given chip instances list.
    ///
    /// This is a convenience method that automatically enables state tracking
    /// for all chips found in a VGM header's chip instances.
    ///
    /// # Arguments
    ///
    /// * `instances` - Chip instances from `VgmHeader::chip_instances()`
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::{VgmStream, VgmCallbackStream};
    /// use soundlog::VgmDocument;
    ///
    /// # let mut doc = VgmDocument::default();
    /// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
    /// let chip_instances = doc.header.chip_instances();
    /// let stream = VgmStream::from_document(doc);
    /// let mut callback_stream = VgmCallbackStream::new(stream);
    /// callback_stream.track_chips(&chip_instances);
    /// ```
    pub fn track_chips(&mut self, instances: &ChipInstances) {
        for (instance, chip, clock_hz) in instances.iter() {
            match chip {
                chip::Chip::Ym2612 => {
                    self.state_trackers.ym2612[*instance as usize] =
                        Some(Ym2612State::new(*clock_hz));
                }
                chip::Chip::Ym2151 => {
                    self.state_trackers.ym2151[*instance as usize] =
                        Some(Ym2151State::new(*clock_hz));
                }
                chip::Chip::Ym2203 => {
                    self.state_trackers.ym2203[*instance as usize] =
                        Some(Ym2203State::new(*clock_hz));
                }
                chip::Chip::Ym2608 => {
                    self.state_trackers.ym2608[*instance as usize] =
                        Some(Ym2608State::new(*clock_hz));
                }
                chip::Chip::Ym2610b => {
                    self.state_trackers.ym2610b[*instance as usize] =
                        Some(Ym2610bState::new(*clock_hz));
                }
                chip::Chip::Ym2413 => {
                    self.state_trackers.ym2413[*instance as usize] =
                        Some(Ym2413State::new(*clock_hz));
                }
                chip::Chip::Ym3812 => {
                    self.state_trackers.ym3812[*instance as usize] =
                        Some(Ym3812State::new(*clock_hz));
                }
                chip::Chip::Ym3526 => {
                    self.state_trackers.ym3526[*instance as usize] =
                        Some(Ym3526State::new(*clock_hz));
                }
                chip::Chip::Y8950 => {
                    self.state_trackers.y8950[*instance as usize] =
                        Some(Y8950State::new(*clock_hz));
                }
                chip::Chip::Sn76489 => {
                    self.state_trackers.sn76489[*instance as usize] =
                        Some(Sn76489State::new(*clock_hz));
                }
                chip::Chip::Ay8910 => {
                    self.state_trackers.ay8910[*instance as usize] =
                        Some(Ay8910State::new(*clock_hz));
                }
                chip::Chip::GbDmg => {
                    self.state_trackers.gb_dmg[*instance as usize] =
                        Some(GbDmgState::new(*clock_hz));
                }
                chip::Chip::NesApu => {
                    self.state_trackers.nes_apu[*instance as usize] =
                        Some(NesApuState::new(*clock_hz));
                }
                chip::Chip::Huc6280 => {
                    self.state_trackers.huc6280[*instance as usize] =
                        Some(Huc6280State::new(*clock_hz));
                }
                chip::Chip::Ymf262 => {
                    self.state_trackers.ymf262[*instance as usize] =
                        Some(Ymf262State::new(*clock_hz));
                }
                chip::Chip::Ymf271 => {
                    self.state_trackers.ymf271[*instance as usize] =
                        Some(Ymf271State::new(*clock_hz));
                }
                chip::Chip::Ymf278b => {
                    self.state_trackers.ymf278b[*instance as usize] =
                        Some(Ymf278bState::new(*clock_hz));
                }
                chip::Chip::Pokey => {
                    self.state_trackers.pokey[*instance as usize] =
                        Some(PokeyState::new(*clock_hz));
                }
                chip::Chip::Saa1099 => {
                    self.state_trackers.saa1099[*instance as usize] =
                        Some(Saa1099State::new(*clock_hz));
                }
                chip::Chip::WonderSwan => {
                    self.state_trackers.wonderswan[*instance as usize] =
                        Some(WonderSwanState::new(*clock_hz));
                }
                chip::Chip::Vsu => {
                    self.state_trackers.vsu[*instance as usize] = Some(VsuState::new(*clock_hz));
                }
                chip::Chip::Mikey => {
                    self.state_trackers.mikey[*instance as usize] =
                        Some(MikeyState::new(*clock_hz));
                }
                chip::Chip::K051649 => {
                    self.state_trackers.k051649[*instance as usize] =
                        Some(K051649State::new(*clock_hz));
                }
                chip::Chip::Scc1 => {
                    self.state_trackers.scc1[*instance as usize] = Some(Scc1State::new(*clock_hz));
                }
                chip::Chip::SegaPcm => {
                    self.state_trackers.sega_pcm[*instance as usize] =
                        Some(SegaPcmState::new(*clock_hz));
                }
                chip::Chip::Rf5c68 => {
                    self.state_trackers.rf5c68[*instance as usize] =
                        Some(Rf5c68State::new(*clock_hz));
                }
                chip::Chip::Rf5c164 => {
                    self.state_trackers.rf5c164[*instance as usize] =
                        Some(Rf5c164State::new(*clock_hz));
                }
                chip::Chip::MultiPcm => {
                    self.state_trackers.multi_pcm[*instance as usize] =
                        Some(MultiPcmState::new(*clock_hz));
                }
                chip::Chip::Upd7759 => {
                    self.state_trackers.upd7759[*instance as usize] =
                        Some(Upd7759State::new(*clock_hz));
                }
                chip::Chip::Okim6258 => {
                    self.state_trackers.okim6258[*instance as usize] =
                        Some(Okim6258State::new(*clock_hz));
                }
                chip::Chip::Okim6295 => {
                    self.state_trackers.okim6295[*instance as usize] =
                        Some(Okim6295State::new(*clock_hz));
                }
                chip::Chip::K054539 => {
                    self.state_trackers.k054539[*instance as usize] =
                        Some(K054539State::new(*clock_hz));
                }
                chip::Chip::C140 => {
                    self.state_trackers.c140[*instance as usize] = Some(C140State::new(*clock_hz));
                }
                chip::Chip::C352 => {
                    self.state_trackers.c352[*instance as usize] = Some(C352State::new(*clock_hz));
                }
                chip::Chip::K053260 => {
                    self.state_trackers.k053260[*instance as usize] =
                        Some(K053260State::new(*clock_hz));
                }
                chip::Chip::Qsound => {
                    self.state_trackers.qsound[*instance as usize] =
                        Some(QsoundState::new(*clock_hz));
                }
                chip::Chip::Scsp => {
                    self.state_trackers.scsp[*instance as usize] = Some(ScspState::new(*clock_hz));
                }
                chip::Chip::Es5503 => {
                    self.state_trackers.es5503[*instance as usize] =
                        Some(Es5503State::new(*clock_hz));
                }
                chip::Chip::Es5506U8 | chip::Chip::Es5506U16 => {
                    self.state_trackers.es5506[*instance as usize] =
                        Some(Es5506State::new(*clock_hz));
                }
                chip::Chip::X1010 => {
                    self.state_trackers.x1_010[*instance as usize] =
                        Some(X1010State::new(*clock_hz));
                }
                chip::Chip::Ga20 => {
                    self.state_trackers.ga20[*instance as usize] = Some(Ga20State::new(*clock_hz));
                }
                chip::Chip::Ymz280b => {
                    self.state_trackers.ymz280b[*instance as usize] =
                        Some(Ymz280bState::new(*clock_hz));
                }
                _ => {
                    // Chips without state tracking are silently skipped
                }
            }
        }
    }

    /// Register a callback for chip register writes using a generic type parameter.
    ///
    /// This is a generic interface that allows registering callbacks for any chip
    /// that implements `WriteCallbackTarget`. Use type parameters to specify which
    /// chip you want to register a callback for.
    ///
    /// # Type Parameters
    ///
    /// * `C` - The chip specification type (e.g., `chip::Ym2612Spec`)
    ///
    /// # Arguments
    ///
    /// * `callback` - A closure that receives:
    ///   - `instance`: The chip instance (Primary or Secondary)
    ///   - `spec`: The chip-specific write specification
    ///   - `sample`: Current sample position (at 44.1 kHz, resets to 0 on loop)
    ///   - `event`: Optional state event detected from this write
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::VgmCallbackStream;
    /// use soundlog::vgm::command::Instance;
    /// use soundlog::chip;
    ///
    /// # let mut doc = soundlog::VgmDocument::default();
    /// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
    /// let mut stream = VgmCallbackStream::from_document(doc);
    ///
    /// // Register a callback for YM2612 writes
    /// stream.on_write(|inst, spec: chip::Ym2612Spec, sample, _event| {
    ///     println!("YM2612[{:?}] @ sample {} port={} reg={:02X} val={:02X}",
    ///         inst, sample, spec.port, spec.register, spec.value);
    /// });
    ///
    /// // Register a callback for YM2151 writes
    /// stream.on_write(|inst, spec: chip::Ym2151Spec, sample, _event| {
    ///     println!("YM2151[{:?}] @ sample {} reg={:02X} val={:02X}",
    ///         inst, sample, spec.register, spec.value);
    /// });
    /// ```
    pub fn on_write<C, F>(&mut self, callback: F)
    where
        C: WriteCallbackTarget,
        F: FnMut(Instance, C, u64, Option<Vec<StateEvent>>) + 'a,
    {
        C::register_callback(&mut self.callbacks, callback);
    }

    /// Enable state tracking for a chip using a generic type parameter.
    ///
    /// This is a generic interface that allows enabling state tracking for any chip
    /// that implements `StateTracker`. Use type parameters to specify which
    /// chip state you want to track.
    ///
    /// # Type Parameters
    ///
    /// * `S` - The chip state type (e.g., `chip::state::Ym2612State`)
    ///
    /// # Arguments
    ///
    /// * `instance` - The chip instance (Primary or Secondary)
    /// * `clock` - The chip's master clock frequency in Hz
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::VgmCallbackStream;
    /// use soundlog::vgm::command::Instance;
    /// use soundlog::chip::state::{Ym2612State, Ym2151State};
    ///
    /// # let mut doc = soundlog::VgmDocument::default();
    /// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
    /// let mut stream = VgmCallbackStream::from_document(doc);
    ///
    /// // Enable state tracking for YM2612
    /// stream.track_state::<Ym2612State>(Instance::Primary, 7_670_454.0);
    ///
    /// // Enable state tracking for YM2151
    /// stream.track_state::<Ym2151State>(Instance::Primary, 3_579_545.0);
    /// ```
    pub fn track_state<S>(&mut self, instance: Instance, clock: f64)
    where
        S: StateTracker,
    {
        S::init_tracker(&mut self.state_trackers, instance, clock);
    }

    /// Register a callback for AY8910 stereo mask commands.
    pub fn on_ay8910_stereo_mask<F>(&mut self, callback: F)
    where
        F: FnMut(Ay8910StereoMask, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_ay8910_stereo_mask = Some(Box::new(callback));
    }

    /// Register a callback for reserved U8 commands.
    pub fn on_reserved_u8_write<F>(&mut self, callback: F)
    where
        F: FnMut(ReservedU8, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_reserved_u8_write = Some(Box::new(callback));
    }

    /// Register a callback for reserved U16 commands.
    pub fn on_reserved_u16_write<F>(&mut self, callback: F)
    where
        F: FnMut(ReservedU16, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_reserved_u16_write = Some(Box::new(callback));
    }

    /// Register a callback for reserved U24 commands.
    pub fn on_reserved_u24_write<F>(&mut self, callback: F)
    where
        F: FnMut(ReservedU24, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_reserved_u24_write = Some(Box::new(callback));
    }

    /// Register a callback for reserved U32 commands.
    pub fn on_reserved_u32_write<F>(&mut self, callback: F)
    where
        F: FnMut(ReservedU32, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_reserved_u32_write = Some(Box::new(callback));
    }

    /// Register a callback for unknown commands.
    pub fn on_unknown_command<F>(&mut self, callback: F)
    where
        F: FnMut(UnknownSpec, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_unknown_command = Some(Box::new(callback));
    }

    /// Register a callback for wait samples commands.
    pub fn on_wait<F>(&mut self, callback: F)
    where
        F: FnMut(WaitSamples, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_wait_samples = Some(Box::new(callback));
    }

    /// Register a callback for data block commands.
    pub fn on_data_block<F>(&mut self, callback: F)
    where
        F: FnMut(&DataBlock, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_data_block = Some(Box::new(callback));
    }

    /// Register a callback for end of data commands.
    pub fn on_end_of_data<F>(&mut self, callback: F)
    where
        F: FnMut(EndOfData, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_end_of_data = Some(Box::new(callback));
    }

    /// Register a callback for PCM RAM write commands.
    pub fn on_pcm_ram_write<F>(&mut self, callback: F)
    where
        F: FnMut(&PcmRamWrite, u64, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks.on_pcm_ram_write = Some(Box::new(callback));
    }

    /// Register a callback that will be invoked for every command before any chip-specific callback.
    ///
    /// This callback receives the raw `VgmCommand` and is useful for logging, debugging,
    /// or implementing custom tracking logic that applies to all commands.
    ///
    /// # Arguments
    ///
    /// * `callback` - A closure that takes a reference to a `VgmCommand`
    ///
    /// # Examples
    ///
    /// ```
    /// use soundlog::vgm::{VgmStream, VgmCallbackStream};
    /// # let mut doc = soundlog::VgmDocument::default();
    /// # doc.commands.push(soundlog::vgm::command::VgmCommand::EndOfData(soundlog::vgm::command::EndOfData));
    /// let stream = VgmStream::from_document(doc);
    ///
    /// let mut callback_stream = VgmCallbackStream::new(stream);
    /// callback_stream.on_any_command(|cmd, sample| {
    ///         println!("Command at sample {}: {:?}", sample, cmd);
    ///     });
    /// ```
    pub fn on_any_command<F>(&mut self, callback: F)
    where
        F: FnMut(&VgmCommand, u64) + 'a,
    {
        self.callbacks.on_any_command = Some(Box::new(callback));
    }

    /// Process a VGM command and invoke the appropriate callbacks.
    ///
    /// This is called automatically by the iterator implementation.
    fn process_command(&mut self, cmd: &VgmCommand) {
        let sample = self.stream.current_sample();

        // Call the generic callback first if registered
        if let Some(ref mut cb) = self.callbacks.on_any_command {
            cb(cmd, sample);
        }

        // Process chip-specific commands with state tracking
        #[allow(clippy::single_match)]
        match cmd {
            VgmCommand::Ym2612Write(instance, spec) => {
                let event = self.state_trackers.ym2612[*instance as usize]
                    .as_mut()
                    .and_then(|state| {
                        state.set_port(spec.port);
                        state.on_register_write(spec.register, spec.value)
                    });
                if let Some(ref mut cb) = self.callbacks.on_ym2612_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ym2151Write(instance, spec) => {
                let event = self.state_trackers.ym2151[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ym2151_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ym2203Write(instance, spec) => {
                let event = self.state_trackers.ym2203[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ym2203_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ym2608Write(instance, spec) => {
                let event = self.state_trackers.ym2608[*instance as usize]
                    .as_mut()
                    .and_then(|state| {
                        state.set_port(spec.port);
                        state.on_register_write(spec.register, spec.value)
                    });
                if let Some(ref mut cb) = self.callbacks.on_ym2608_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ym2610bWrite(instance, spec) => {
                let event = self.state_trackers.ym2610b[*instance as usize]
                    .as_mut()
                    .and_then(|state| {
                        state.set_port(spec.port);
                        state.on_register_write(spec.register, spec.value)
                    });
                if let Some(ref mut cb) = self.callbacks.on_ym2610b_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ym2413Write(instance, spec) => {
                let event = self.state_trackers.ym2413[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ym2413_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ym3812Write(instance, spec) => {
                let event = self.state_trackers.ym3812[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ym3812_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ym3526Write(instance, spec) => {
                let event = self.state_trackers.ym3526[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ym3526_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Y8950Write(instance, spec) => {
                let event = self.state_trackers.y8950[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_y8950_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Sn76489Write(instance, spec) => {
                let event = self.state_trackers.sn76489[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.value, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_sn76489_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ay8910Write(instance, spec) => {
                let event = self.state_trackers.ay8910[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ay8910_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::GbDmgWrite(instance, spec) => {
                let event = self.state_trackers.gb_dmg[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_gb_dmg_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::NesApuWrite(instance, spec) => {
                let event = self.state_trackers.nes_apu[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_nes_apu_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Huc6280Write(instance, spec) => {
                let event = self.state_trackers.huc6280[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_huc6280_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::SegaPcmWrite(instance, spec) => {
                let event = self.state_trackers.sega_pcm[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_sega_pcm_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Rf5c68U8Write(instance, spec) => {
                let event = self.state_trackers.rf5c68[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset as u16, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_rf5c68_u8_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Rf5c68U16Write(instance, spec) => {
                let event = self.state_trackers.rf5c68[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_rf5c68_u16_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Rf5c164U8Write(instance, spec) => {
                let event = self.state_trackers.rf5c164[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(u16::from(spec.offset), spec.value));
                if let Some(ref mut cb) = self.callbacks.on_rf5c164_u8_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Rf5c164U16Write(instance, spec) => {
                let event = self.state_trackers.rf5c164[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_rf5c164_u16_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::PwmWrite(instance, spec) => {
                if let Some(ref mut cb) = self.callbacks.on_pwm_write {
                    cb(*instance, spec.clone(), sample, None);
                }
            }
            VgmCommand::MultiPcmWrite(instance, spec) => {
                let event = self.state_trackers.multi_pcm[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_multi_pcm_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::MultiPcmBankWrite(instance, spec) => {
                if let Some(ref mut cb) = self.callbacks.on_multi_pcm_bank_write {
                    cb(*instance, spec.clone(), sample, None);
                }
            }
            VgmCommand::Upd7759Write(instance, spec) => {
                let event = self.state_trackers.upd7759[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_upd7759_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Okim6258Write(instance, spec) => {
                let event = self.state_trackers.okim6258[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_okim6258_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Okim6295Write(instance, spec) => {
                let event = self.state_trackers.okim6295[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_okim6295_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::K054539Write(instance, spec) => {
                let event = self.state_trackers.k054539[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_k054539_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::C140Write(instance, spec) => {
                let event = self.state_trackers.c140[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_c140_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::K053260Write(instance, spec) => {
                let event = self.state_trackers.k053260[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_k053260_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::PokeyWrite(instance, spec) => {
                let event = self.state_trackers.pokey[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_pokey_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::QsoundWrite(instance, spec) => {
                let event = self.state_trackers.qsound[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_qsound_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::ScspWrite(instance, spec) => {
                let event = self.state_trackers.scsp[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_scsp_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::WonderSwanWrite(instance, spec) => {
                let event = self.state_trackers.wonderswan[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset as u8, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_wonder_swan_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::VsuWrite(instance, spec) => {
                let event = self.state_trackers.vsu[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_vsu_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Saa1099Write(instance, spec) => {
                let event = self.state_trackers.saa1099[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_saa1099_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Es5503Write(instance, spec) => {
                let event = self.state_trackers.es5503[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_es5503_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Es5506BEWrite(instance, spec) => {
                let event = self.state_trackers.es5506[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value as u16));
                if let Some(ref mut cb) = self.callbacks.on_es5506_u8_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Es5506D6Write(instance, spec) => {
                let event = self.state_trackers.es5506[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_es5506_u16_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::X1010Write(instance, spec) => {
                let event = self.state_trackers.x1_010[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.offset, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_x1_010_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::C352Write(instance, spec) => {
                let event = self.state_trackers.c352[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_c352_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ga20Write(instance, spec) => {
                let event = self.state_trackers.ga20[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ga20_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::MikeyWrite(instance, spec) => {
                let event = self.state_trackers.mikey[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_mikey_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::GameGearPsgWrite(instance, spec) => {
                if let Some(ref mut cb) = self.callbacks.on_game_gear_psg_write {
                    cb(*instance, spec.clone(), sample, None);
                }
            }
            VgmCommand::Scc1Write(instance, spec) => {
                let event = self.state_trackers.scc1[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_scc1_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ymf262Write(instance, spec) => {
                let event = self.state_trackers.ymf262[*instance as usize]
                    .as_mut()
                    .and_then(|state| {
                        state.set_port(spec.port);
                        state.on_register_write(spec.register, spec.value)
                    });
                if let Some(ref mut cb) = self.callbacks.on_ymf262_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ymf278bWrite(instance, spec) => {
                let event = self.state_trackers.ymf278b[*instance as usize]
                    .as_mut()
                    .and_then(|state| {
                        state.set_port(spec.port);
                        state.on_register_write(spec.register, spec.value)
                    });
                if let Some(ref mut cb) = self.callbacks.on_ymf278b_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ymf271Write(instance, spec) => {
                let event = self.state_trackers.ymf271[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ymf271_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::Ymz280bWrite(instance, spec) => {
                let event = self.state_trackers.ymz280b[*instance as usize]
                    .as_mut()
                    .and_then(|state| state.on_register_write(spec.register, spec.value));
                if let Some(ref mut cb) = self.callbacks.on_ymz280b_write {
                    cb(*instance, spec.clone(), sample, event);
                }
            }
            VgmCommand::AY8910StereoMask(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_ay8910_stereo_mask {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::ReservedU8Write(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_reserved_u8_write {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::ReservedU16Write(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_reserved_u16_write {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::ReservedU24Write(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_reserved_u24_write {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::ReservedU32Write(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_reserved_u32_write {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::UnknownCommand(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_unknown_command {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::EndOfData(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_end_of_data {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::DataBlock(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_data_block {
                    cb(spec, sample, None);
                }
            }
            VgmCommand::PcmRamWrite(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_pcm_ram_write {
                    cb(spec, sample, None);
                }
            }
            VgmCommand::WaitSamples(spec) => {
                if let Some(ref mut cb) = self.callbacks.on_wait_samples {
                    cb(spec.clone(), sample, None);
                }
            }
            VgmCommand::Wait735Samples(_)
            | VgmCommand::Wait882Samples(_)
            | VgmCommand::WaitNSample(_)
            | VgmCommand::YM2612Port0Address2AWriteAndWaitN(_)
            | VgmCommand::SetupStreamControl(_)
            | VgmCommand::SetStreamData(_)
            | VgmCommand::SetStreamFrequency(_)
            | VgmCommand::StartStream(_)
            | VgmCommand::StopStream(_)
            | VgmCommand::StartStreamFastCall(_)
            | VgmCommand::SeekOffset(_) => {
                // These wait commands are expanded to WaitSamples by VgmStream,
                // so they are handled by the on_wait_samples callback instead.
            }
        }
    }
}

impl<'a> Iterator for VgmCallbackStream<'a> {
    type Item = Result<StreamResult, ParseError>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.stream.next()?;

        match result {
            Ok(StreamResult::Command(ref cmd)) => {
                self.process_command(cmd);
                Some(result)
            }
            Ok(StreamResult::EndOfStream) => None,
            _ => Some(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VgmDocument;

    #[test]
    fn test_callback_stream_basic() {
        let doc = VgmDocument::default();
        let stream = VgmStream::from_document(doc);
        let _callback_stream = VgmCallbackStream::new(stream);
    }

    #[test]
    fn test_callback_stream_from_document() {
        let doc = VgmDocument::default();
        let _callback_stream = VgmCallbackStream::from_document(doc);
    }

    #[test]
    fn test_ym2612_callback_invoked() {
        let doc = VgmDocument::default();
        let stream = VgmStream::from_document(doc);

        let invoked = std::cell::RefCell::new(false);
        let mut callback_stream = VgmCallbackStream::new(stream);
        callback_stream.on_write(|_inst, _spec: chip::Ym2612Spec, _sample, _event| {
            *invoked.borrow_mut() = true;
        });

        // Iterate through the stream (limit iterations to avoid infinite loop with empty doc)
        for _result in (&mut callback_stream).take(10) {
            // The callback may or may not be invoked depending on the document content
        }

        // Note: This test just verifies that the callback can be registered and the stream can be iterated.
        // Whether the callback is actually invoked depends on whether the VGM contains YM2612 commands.
    }

    #[test]
    fn test_ym2612_state_tracking() {
        let doc = VgmDocument::default();
        let stream = VgmStream::from_document(doc);

        let key_on_detected = std::cell::RefCell::new(false);
        let mut callback_stream = VgmCallbackStream::new(stream);
        callback_stream.track_state::<Ym2612State>(Instance::Primary, 7_670_454.0);
        callback_stream.on_write(|_inst, _spec: chip::Ym2612Spec, _sample, event| {
            if event
                .is_some_and(|events| events.iter().any(|e| matches!(e, StateEvent::KeyOn { .. })))
            {
                *key_on_detected.borrow_mut() = true;
            }
        });

        // Iterate through the stream (limit iterations to avoid infinite loop with empty doc)
        for _result in (&mut callback_stream).take(10) {
            // Process commands
        }

        // Note: Whether KeyOn is detected depends on the document content
    }

    #[test]
    fn test_any_command_callback() {
        let doc = VgmDocument::default();
        let stream = VgmStream::from_document(doc);

        let command_count = std::cell::RefCell::new(0);
        let mut callback_stream = VgmCallbackStream::new(stream);
        callback_stream.on_any_command(|_cmd, _sample| {
            *command_count.borrow_mut() += 1;
        });

        // Iterate through the stream (limit iterations to avoid infinite loop with empty doc)
        for _result in (&mut callback_stream).take(10) {
            // Process commands
        }

        // The command count should be greater than 0 if there are any commands in the document
        // (Though an empty document may have 0 commands)
    }

    #[test]
    fn test_generic_track_state() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let doc = VgmDocument::default();
        let stream = VgmStream::from_document(doc);

        let mut callback_stream = VgmCallbackStream::new(stream);

        // Test generic track_state interface
        callback_stream.track_state::<Ym2612State>(Instance::Primary, 7_670_454.0);
        callback_stream.track_state::<Ym2151State>(Instance::Secondary, 3_579_545.0);
        callback_stream.track_state::<Sn76489State>(Instance::Primary, 3_579_545.0);

        // Verify the callback can be registered with the generic interface
        let invoked = Rc::new(RefCell::new(false));
        let invoked_clone = invoked.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Ym2612Spec, _sample, _event| {
            *invoked_clone.borrow_mut() = true;
        });

        // Iterate through the stream (limit iterations)
        for _result in (&mut callback_stream).take(10) {
            // Process commands
        }

        // Note: This test verifies the generic interface compiles and can be used
    }
}
