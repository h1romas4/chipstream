//! Device-independent VGM, MDX, and XGM playback.
//!
//! [`VgmSequencer`] and the optional `mdx` and `xgm` sequencers render stereo
//! PCM without an audio device or sound engine. Output is not clipped, and
//! rendering may allocate.
//!
//! Playback options configure each sequencer; [`ChipFactoryRegistry`] can
//! replace selected chip backends. Checkpoints support seeking. Duration scans
//! are bounded and can return [`PlaybackDuration::Unknown`].
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
#[cfg(feature = "mdx")]
mod mdx;
mod options;
mod vgm;
mod volume;
#[cfg(feature = "xgm")]
mod xgm;

#[cfg(feature = "mdx")]
pub use duration::measure_mdx_duration_frames;
#[cfg(feature = "xgm")]
pub use duration::measure_xgm_duration_frames;
pub use duration::{
    PlaybackDuration, measure_generator_duration_frames, measure_vgm_duration_frames,
};
pub use effects::{EffectError, EffectId, OutputGroupId};
pub use event::{VgmStateTracker, VgmStateTrackerKind, VgmWriteCallbackTarget};
pub use factory::{
    Ay8910Settings, Ay8910Variant, AyOutputSettings, C140Variant, ChipBackendSettings,
    ChipFactoryConfig, ChipFactoryRegistry, Okim6258Divider, Okim6258Settings, Sn76489Settings,
};
#[cfg(feature = "mdx")]
pub use mdx::{MdxSequencer, MdxSequencerError, MdxSequencerOptions};
pub use options::PlaybackOptions;
pub use soundlog::chip::Chip;
pub use soundlog::chip::event::{StateEvent, ToneInfo};
pub use soundlog::vgm::command::Instance;
pub use vgm::{
    GeneratorConfig, GeneratorLoopMarker, NativeOutput, VgmSequencer, VgmSequencerCheckpoint,
    VgmSequencerError, VgmSequencerOptions,
};
#[cfg(feature = "xgm")]
pub use xgm::{XgmSequencer, XgmSequencerOptions};
