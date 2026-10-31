//! Typed register-write callbacks and state tracking during sequencer playback.

use std::any::TypeId;

use crate::chip::ChipRegisterWrite;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::{
    ChipState, Y8950State, Ym2151State, Ym2203State, Ym2413State, Ym2608State, Ym2610bState,
    Ym2612State, Ym3526State, Ym3812State, Ymf262State, Ymf278bState,
};
use soundlog::chip::{
    C140Spec, Chip, GameGearPsgSpec, Okim6258Spec, Okim6295Spec, PsgSpec, PwmSpec, SegaPcmSpec,
    Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec, Ym2612Spec, Ym3526Spec,
    Ym3812Spec, Ymf262Spec, Ymf278bSpec,
};
use soundlog::vgm::command::Instance;

mod private {
    pub trait WriteCallbackSealed {}
    pub trait StateTrackerSealed {}
}

/// A chip-write specification accepted by [`VgmSequencer::on_write`].
///
/// The callback receives the instance, typed write, absolute output-frame
/// position, and optional state changes. A callback can be registered for
/// each write specification supported by chipstream.
pub trait VgmWriteCallbackTarget: private::WriteCallbackSealed + Sized + 'static {
    #[doc(hidden)]
    fn matches(write: &ChipRegisterWrite) -> bool;

    #[doc(hidden)]
    fn from_write(write: &ChipRegisterWrite) -> Option<Self>;
}

macro_rules! impl_write_callback_target {
    ($spec:ty, $variant:ident) => {
        impl private::WriteCallbackSealed for $spec {}

        impl VgmWriteCallbackTarget for $spec {
            fn matches(write: &ChipRegisterWrite) -> bool {
                matches!(write, ChipRegisterWrite::$variant(_))
            }

            fn from_write(write: &ChipRegisterWrite) -> Option<Self> {
                match write {
                    ChipRegisterWrite::$variant(spec) => Some(spec.clone()),
                    _ => None,
                }
            }
        }
    };
}

impl_write_callback_target!(PsgSpec, Psg);
impl_write_callback_target!(GameGearPsgSpec, GameGearPsg);
impl_write_callback_target!(SegaPcmSpec, SegaPcm);
impl_write_callback_target!(C140Spec, C140);
impl_write_callback_target!(PwmSpec, Pwm);
impl_write_callback_target!(Okim6258Spec, Okim6258);
impl_write_callback_target!(Okim6295Spec, Okim6295);
impl_write_callback_target!(Ym2413Spec, Ym2413);
impl_write_callback_target!(Ym2151Spec, Ym2151);
impl_write_callback_target!(Ym2203Spec, Ym2203);
impl_write_callback_target!(Ym3812Spec, Ym3812);
impl_write_callback_target!(Ym3526Spec, Ym3526);
impl_write_callback_target!(Y8950Spec, Y8950);
impl_write_callback_target!(Ym2612Spec, Ym2612);
impl_write_callback_target!(Ym2608Spec, Ym2608);
impl_write_callback_target!(Ym2610Spec, Ym2610);
impl_write_callback_target!(Ymf262Spec, Ymf262);
impl_write_callback_target!(Ymf278bSpec, Ymf278b);

/// Supported sound-chip state types for [`VgmSequencer::track_state`].
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VgmStateTrackerKind {
    Ym2151,
    Ym2203,
    Ym2413,
    Ym2608,
    Ym2610b,
    Ym2612,
    Ym3526,
    Y8950,
    Ym3812,
    Ymf262,
    Ymf278b,
}

/// A soundlog state type supported by sequencer event tracking.
pub trait VgmStateTracker: private::StateTrackerSealed + 'static {
    #[doc(hidden)]
    const KIND: VgmStateTrackerKind;
}

macro_rules! impl_state_tracker {
    ($state:ty, $kind:ident) => {
        impl private::StateTrackerSealed for $state {}

        impl VgmStateTracker for $state {
            const KIND: VgmStateTrackerKind = VgmStateTrackerKind::$kind;
        }
    };
}

impl_state_tracker!(Ym2151State, Ym2151);
impl_state_tracker!(Ym2203State, Ym2203);
impl_state_tracker!(Ym2413State, Ym2413);
impl_state_tracker!(Ym2608State, Ym2608);
impl_state_tracker!(Ym2610bState, Ym2610b);
impl_state_tracker!(Ym2612State, Ym2612);
impl_state_tracker!(Ym3526State, Ym3526);
impl_state_tracker!(Y8950State, Y8950);
impl_state_tracker!(Ym3812State, Ym3812);
impl_state_tracker!(Ymf262State, Ymf262);
impl_state_tracker!(Ymf278bState, Ymf278b);

pub(super) const MAX_PENDING_EVENTS: usize = 4096;

#[derive(Clone)]
pub(super) struct PendingWriteEvent {
    pub instance: Instance,
    pub write: ChipRegisterWrite,
    pub state_events: Option<Vec<StateEvent>>,
}

trait WriteCallback {
    fn target_type_id(&self) -> TypeId;
    fn matches(&self, write: &ChipRegisterWrite) -> bool;
    fn call(
        &mut self,
        instance: Instance,
        write: &ChipRegisterWrite,
        sample: usize,
        state_events: Option<Vec<StateEvent>>,
    );
}

struct TypedWriteCallback<C, F> {
    callback: F,
    _target: std::marker::PhantomData<fn() -> C>,
}

impl<C, F> WriteCallback for TypedWriteCallback<C, F>
where
    C: VgmWriteCallbackTarget,
    F: FnMut(Instance, C, usize, Option<Vec<StateEvent>>),
{
    fn target_type_id(&self) -> TypeId {
        TypeId::of::<C>()
    }

    fn matches(&self, write: &ChipRegisterWrite) -> bool {
        C::matches(write)
    }

    fn call(
        &mut self,
        instance: Instance,
        write: &ChipRegisterWrite,
        sample: usize,
        state_events: Option<Vec<StateEvent>>,
    ) {
        if let Some(spec) = C::from_write(write) {
            (self.callback)(instance, spec, sample, state_events);
        }
    }
}

pub(super) struct CallbackRegistry<'a> {
    callbacks: Vec<Box<dyn WriteCallback + 'a>>,
}

impl Default for CallbackRegistry<'_> {
    fn default() -> Self {
        Self {
            callbacks: Vec::new(),
        }
    }
}

impl<'a> CallbackRegistry<'a> {
    pub(super) fn has_callback(&self, write: &ChipRegisterWrite) -> bool {
        self.callbacks
            .iter()
            .any(|callback| callback.matches(write))
    }

    pub(super) fn dispatch(&mut self, event: PendingWriteEvent, sample: usize) {
        if let Some(callback) = self
            .callbacks
            .iter_mut()
            .find(|callback| callback.matches(&event.write))
        {
            callback.call(event.instance, &event.write, sample, event.state_events);
        }
    }

    pub(super) fn register<C, F>(&mut self, callback: F)
    where
        C: VgmWriteCallbackTarget,
        F: FnMut(Instance, C, usize, Option<Vec<StateEvent>>) + 'a,
    {
        self.callbacks
            .retain(|registered| registered.target_type_id() != TypeId::of::<C>());
        self.callbacks.push(Box::new(TypedWriteCallback::<C, F> {
            callback,
            _target: std::marker::PhantomData,
        }));
    }
}

#[derive(Clone)]
pub(super) enum Tracker {
    Ym2151(Ym2151State),
    Ym2203(Ym2203State),
    Ym2413(Ym2413State),
    Ym2608(Ym2608State),
    Ym2610b(Ym2610bState),
    Ym2612(Ym2612State),
    Ym3526(Ym3526State),
    Y8950(Y8950State),
    Ym3812(Ym3812State),
    Ymf262(Ymf262State),
    Ymf278b(Ymf278bState),
}

impl Tracker {
    pub(super) fn on_register_write(
        &mut self,
        port: u8,
        register: u8,
        value: u8,
    ) -> Option<Vec<StateEvent>> {
        match self {
            Self::Ym2151(state) => state.on_register_write(register, value),
            Self::Ym2203(state) => state.on_register_write(register, value),
            Self::Ym2413(state) => state.on_register_write(register, value),
            Self::Ym2608(state) => {
                state.set_port(port);
                state.on_register_write(register, value)
            }
            Self::Ym2610b(state) => {
                state.set_port(port);
                state.on_register_write(register, value)
            }
            Self::Ym2612(state) => {
                state.set_port(port);
                state.on_register_write(register, value)
            }
            Self::Ym3526(state) => state.on_register_write(register, value),
            Self::Y8950(state) => state.on_register_write(register, value),
            Self::Ym3812(state) => state.on_register_write(register, value),
            Self::Ymf262(state) => {
                state.set_port(port);
                state.on_register_write(register, value)
            }
            Self::Ymf278b(state) => {
                state.set_port(port);
                state.on_register_write(register, value)
            }
        }
    }
}

pub(super) fn tracker_chip(kind: VgmStateTrackerKind) -> Chip {
    match kind {
        VgmStateTrackerKind::Ym2151 => Chip::Ym2151,
        VgmStateTrackerKind::Ym2203 => Chip::Ym2203,
        VgmStateTrackerKind::Ym2413 => Chip::Ym2413,
        VgmStateTrackerKind::Ym2608 => Chip::Ym2608,
        VgmStateTrackerKind::Ym2610b => Chip::Ym2610b,
        VgmStateTrackerKind::Ym2612 => Chip::Ym2612,
        VgmStateTrackerKind::Ym3526 => Chip::Ym3526,
        VgmStateTrackerKind::Y8950 => Chip::Y8950,
        VgmStateTrackerKind::Ym3812 => Chip::Ym3812,
        VgmStateTrackerKind::Ymf262 => Chip::Ymf262,
        VgmStateTrackerKind::Ymf278b => Chip::Ymf278b,
    }
}

pub(super) fn tracker(kind: VgmStateTrackerKind, clock: f32) -> Tracker {
    match kind {
        VgmStateTrackerKind::Ym2151 => Tracker::Ym2151(Ym2151State::new(clock)),
        VgmStateTrackerKind::Ym2203 => Tracker::Ym2203(Ym2203State::new(clock)),
        VgmStateTrackerKind::Ym2413 => Tracker::Ym2413(Ym2413State::new(clock)),
        VgmStateTrackerKind::Ym2608 => Tracker::Ym2608(Ym2608State::new(clock)),
        VgmStateTrackerKind::Ym2610b => Tracker::Ym2610b(Ym2610bState::new(clock)),
        VgmStateTrackerKind::Ym2612 => Tracker::Ym2612(Ym2612State::new(clock)),
        VgmStateTrackerKind::Ym3526 => Tracker::Ym3526(Ym3526State::new(clock)),
        VgmStateTrackerKind::Y8950 => Tracker::Y8950(Y8950State::new(clock)),
        VgmStateTrackerKind::Ym3812 => Tracker::Ym3812(Ym3812State::new(clock)),
        VgmStateTrackerKind::Ymf262 => Tracker::Ymf262(Ymf262State::new(clock)),
        VgmStateTrackerKind::Ymf278b => Tracker::Ymf278b(Ymf278bState::new(clock)),
    }
}
