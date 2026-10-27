//! Filtering and tracking register-write events during sequencer playback.

use soundlog::chip::Chip;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::{
    ChipState, Y8950State, Ym2151State, Ym2203State, Ym2413State, Ym2608State, Ym2610bState,
    Ym2612State, Ym3526State, Ym3812State, Ymf262State, Ymf278bState,
};
use soundlog::vgm::command::Instance;

/// Select which applied register writes are reported.
///
/// All specified fields must match. When `register_writes` is false, only
/// writes that produce at least one tracked state change are reported.
/// State tracking receives all writes to matching chips even if port or
/// register filters suppress individual notifications. Raw writes are
/// reported only for instantiated chips. State tracking is available for
/// YM2151, YM2203, YM2413, YM2608, YM2610B, YM2612, YM3526, Y8950,
/// YM3812, YMF262, and YMF278B. Other supported chips can report raw writes
/// but not tone events. State events describe register-derived key state,
/// not measured volume or envelope tails.
///
/// YMFM writes are reported when applied, which can be later than their
/// source timestamp because the sequencer paces queued writes. At the final
/// boundary, writes still pending are applied without producing more audio.
///
/// Both [`super::VgmSequencer`] and [`super::MdxSequencer`] support event
/// collection. Configure it before rendering:
///
/// ```no_run
/// use chipstream::sequencer::{Chip, Instance, StateEvent, VgmEventFilter, VgmSequencer};
///
/// let bytes = std::fs::read("song.vgm")?;
/// let mut player = VgmSequencer::from_vgm(bytes, 48_000, 512)?;
/// player.configure_events(VgmEventFilter {
///     chip: Some(Chip::Ym2151),
///     instance: Some(Instance::Primary),
///     register_writes: false,
///     ..Default::default()
/// })?;
/// while let Some(block) = player.render_with_events(|event| {
///     for state in &event.state_events {
///         if let StateEvent::KeyOn { channel, tone } = state {
///             println!("frame {} channel {} frequency {:?}", event.frame_offset, channel, tone.freq_hz);
///         }
///     }
/// })? {
///     // Queue block.frames() and schedule events relative to this block.
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Event collection and state tracking can allocate and are not real-time
/// safe. The host must account for device buffering when synchronizing events.
#[derive(Debug, Clone)]
pub struct VgmEventFilter {
    pub chip: Option<Chip>,
    pub instance: Option<Instance>,
    pub port: Option<u8>,
    pub register: Option<u8>,
    pub register_writes: bool,
    pub state_changes: bool,
    /// Maximum number of pending write events, including SRC lookahead.
    pub max_pending_events: usize,
}

impl Default for VgmEventFilter {
    fn default() -> Self {
        Self {
            chip: None,
            instance: None,
            port: None,
            register: None,
            register_writes: false,
            state_changes: true,
            max_pending_events: 4096,
        }
    }
}

impl VgmEventFilter {
    pub(super) fn matches_chip(&self, chip: &Chip, instance: Instance) -> bool {
        self.chip.as_ref().is_none_or(|selected| selected == chip)
            && self.instance.is_none_or(|selected| selected == instance)
    }

    pub(super) fn matches_write(
        &self,
        chip: &Chip,
        instance: Instance,
        port: u8,
        register: u8,
    ) -> bool {
        self.matches_chip(chip, instance)
            && self.port.is_none_or(|selected| selected == port)
            && self.register.is_none_or(|selected| selected == register)
    }
}

/// A successfully applied write and any state changes it caused.
///
/// SN76489 uses register 0 for PSG writes and 1 for Game Gear stereo masks.
/// SegaPCM and C140 use `port` for the address's high byte and `register` for
/// its low byte. Events are emitted in playback order, including across loops.
#[derive(Debug, Clone, PartialEq)]
pub struct VgmWriteEvent {
    /// Position relative to the PCM block returned by this render call.
    /// At EOF it may equal the block length, or be zero if no block remains.
    pub frame_offset: u32,
    pub chip: Chip,
    pub instance: Instance,
    pub port: u8,
    pub register: u8,
    /// Full register value (PWM writes may exceed 8 bits).
    pub value: u32,
    pub state_events: Vec<StateEvent>,
}

pub(super) type Tracker = Box<dyn FnMut(u8, u8, u8) -> Option<Vec<StateEvent>>>;

pub(super) fn tracker(chip: &Chip, clock: u32) -> Option<Tracker> {
    macro_rules! single {
        ($ty:ty) => {{
            let mut state = <$ty>::new(clock as f32);
            Some(
                Box::new(move |_port, register, value| state.on_register_write(register, value))
                    as Tracker,
            )
        }};
    }
    macro_rules! multi {
        ($ty:ty) => {{
            let mut state = <$ty>::new(clock as f32);
            Some(Box::new(move |port, register, value| {
                state.set_port(port);
                state.on_register_write(register, value)
            }) as Tracker)
        }};
    }
    match chip {
        Chip::Ym2151 => single!(Ym2151State),
        Chip::Ym2203 => single!(Ym2203State),
        Chip::Ym2413 => single!(Ym2413State),
        Chip::Ym2608 => multi!(Ym2608State),
        Chip::Ym2610b => multi!(Ym2610bState),
        Chip::Ym2612 => multi!(Ym2612State),
        Chip::Ym3526 => single!(Ym3526State),
        Chip::Y8950 => single!(Y8950State),
        Chip::Ym3812 => single!(Ym3812State),
        Chip::Ymf262 => multi!(Ymf262State),
        Chip::Ymf278b => multi!(Ymf278bState),
        _ => None,
    }
}
