//! Device-independent VGM, MDX, and XGM playback.
//!
//! [`VgmSequencer`], [`MdxSequencer`], and [`XgmSequencer`] render stereo PCM
//! without an audio device or sound engine. Output is not clipped, and
//! rendering may allocate.
//!
//! [`VgmSequencerOptions`], [`MdxSequencerOptions`], and [`XgmSequencerOptions`]
//! configure playback; [`ChipFactoryRegistry`] can replace selected chip
//! backends. Checkpoints support seeking. Duration scans through
//! [`measure_vgm_duration_frames`], [`measure_mdx_duration_frames`], and
//! [`measure_xgm_duration_frames`] are bounded and can return
//! [`PlaybackDuration::Unknown`].
//!
//! VGM generators can supply commands lazily using [`GeneratorConfig`], but
//! single-pass generators do not support checkpoints. After a rendering or
//! checkpoint restore error, create a new sequencer. Usage examples are in
//! the [crate documentation](crate).

mod compat;
mod duration;
mod effects;
mod event;
mod factory;
mod mdx;
mod options;
mod vgm;
mod volume;
mod xgm;

pub use duration::{
    PlaybackDuration, measure_mdx_duration_frames, measure_vgm_duration_frames,
    measure_xgm_duration_frames,
};
pub use effects::{EffectError, EffectId, OutputGroupId};
pub use event::{VgmStateTracker, VgmStateTrackerKind, VgmWriteCallbackTarget};
pub use factory::{
    Ay8910Settings, Ay8910Variant, AyOutputSettings, C140Variant, ChipBackendSettings,
    ChipFactoryConfig, ChipFactoryRegistry, Okim6258Divider, Okim6258Settings, Sn76489Settings,
};
pub use mdx::{MdxSequencer, MdxSequencerError, MdxSequencerOptions};
pub use options::PlaybackOptions;
pub use soundlog::chip::Chip;
pub use soundlog::chip::event::{StateEvent, ToneInfo};
pub use soundlog::vgm::command::Instance;
pub use vgm::{
    GeneratorConfig, NativeOutput, VgmSequencer, VgmSequencerCheckpoint, VgmSequencerError,
    VgmSequencerOptions,
};
pub use xgm::{XgmSequencer, XgmSequencerOptions};
