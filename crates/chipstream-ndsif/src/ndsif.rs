//! Scheduled chipstream adapters for external NDSIF transports.
//!
//! Hardware-backed chips emit silence into host PCM; audible output must be
//! captured or played through the physical device. Transport events use
//! absolute output-frame positions and can arrive out of chronological order.

use std::{cell::RefCell, collections::VecDeque, num::NonZeroU32, rc::Rc};

use ::ndsif::{
    AudioPositionClock, AudioTiming, BytePosition, Divider, OkiClock, Pan, RegisterWrite, WriteId,
    ZeroPair,
};
use soundlog::chip::{Chip, Okim6258Spec};
use soundlog::vgm::command::Instance;

use chipstream::{
    chip::{ChipError, ChipRegisterWrite, SampleRate, ScheduledChip, SoundChip},
    sequencer::{ChipFactoryConfig, ChipFactoryRegistry},
};

use ndsif_transport::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent, NdsifPsgEvent, NdsifRegisterEvent,
    NdsifTransport,
};

/// NDSIF-backed chip replacement that emits silence to the host PCM path.
///
/// The physical chip produces the audible output. Register events are handed
/// to the injected transport with absolute output-frame positions.
///
/// Untimed calls to [`SoundChip::apply_register_write`] are scheduled at frame
/// zero. Sequencer playback uses [`ScheduledChip::apply_register_write_at`] and
/// supplies the correct absolute frame.
pub struct NdsifSoundChip<T> {
    chip: Chip,
    instance: u8,
    clock: u32,
    output_rate: u32,
    sample_rate: SampleRate,
    channels: usize,
    transport: Rc<RefCell<T>>,
}

/// AY-family variants accepted by an NDSIF AY-compatible target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NdsifAyCompatibility {
    /// Accept only AY8910.
    #[default]
    Ay8910,
    /// Also accept YM2149 as an approximation.
    Ay8910AndYm2149,
}

impl<T: NdsifTransport> NdsifSoundChip<T> {
    /// Configure a physical chip and create its scheduled sound-chip adapter.
    pub fn new(
        config: &ChipFactoryConfig<'_>,
        transport: Rc<RefCell<T>>,
    ) -> Result<Self, ChipError> {
        Self::with_target_instance(config, config.instance(), transport)
    }

    /// Configure a physical chip instance independently of the source instance.
    pub fn with_target_instance(
        config: &ChipFactoryConfig<'_>,
        target: Instance,
        transport: Rc<RefCell<T>>,
    ) -> Result<Self, ChipError> {
        Self::with_target_ay_compatibility(config, target, transport, NdsifAyCompatibility::Ay8910)
    }

    /// Configure an instance with an explicitly selected AY compatibility policy.
    ///
    /// The caller must ensure the target supports the selected approximation.
    /// Unsupported AY output and clock flags remain rejected.
    pub fn with_target_ay_compatibility(
        config: &ChipFactoryConfig<'_>,
        target: Instance,
        transport: Rc<RefCell<T>>,
        compatibility: NdsifAyCompatibility,
    ) -> Result<Self, ChipError> {
        use soundlog::vgm::header::Ay8910ChipType;

        let chip = config.chip().clone();
        let Some((divisor, channels, write_id)) = ndsif_chip_config(&chip) else {
            return Err(ChipError::UnsupportedChip(chip));
        };
        if chip == Chip::Ay8910
            && (!(config.header().ay_chip_type == Ay8910ChipType::Ay8910
                || (compatibility == NdsifAyCompatibility::Ay8910AndYm2149
                    && config.header().ay_chip_type == Ay8910ChipType::Ym2149))
                || config.header().ay8910_flags.ym_pin26_low
                || config.header().ay8910_flags.discrete_output
                || config.header().ay8910_flags.raw_output
                || config.header().ay8910_flags.reserved != 0)
        {
            return Err(ChipError::InvalidConfiguration {
                chip,
                reason: "NDSIF AY routing requires a supported AY chip type with mixed output and no YM clock-divider flags",
            });
        }
        if config.clock() < divisor {
            return Err(ChipError::InvalidClock {
                clock: config.clock(),
                minimum: divisor,
            });
        }
        let instance = instance_id(target);
        let write_id = write_id_for(write_id, &chip)?;
        transport
            .try_borrow_mut()
            .map_err(|_| external_error(&chip, "NDSIF transport is already borrowed"))?
            .configure_chip(write_id, instance, config.clock())
            .map_err(|error| external_error(&chip, error))?;

        Ok(Self {
            chip,
            instance,
            clock: config.clock(),
            output_rate: config.output_rate(),
            sample_rate: SampleRate::from_ratio(
                config.clock(),
                NonZeroU32::new(divisor).expect("YMFM sample-rate divisors are nonzero"),
            ),
            channels,
            transport,
        })
    }

    /// Validate and schedule a YMFM register write on the transport.
    fn schedule_write(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        sample_rate: u32,
        vgm_initial: bool,
    ) -> Result<(), ChipError> {
        let actual_chip = write.chip();
        if actual_chip != self.chip {
            return Err(ChipError::ChipMismatch {
                expected: self.chip.clone(),
                actual: actual_chip,
            });
        }
        let (write_id, page, address, value) = register_parts(&write)?;
        let event = NdsifRegisterEvent {
            write_id: write_id_for(write_id, &self.chip)?,
            instance: self.instance,
            page,
            write: RegisterWrite::new(address, value),
            sample_frame,
            sample_rate,
        };
        let mut transport = self
            .transport
            .try_borrow_mut()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?;
        if vgm_initial {
            transport.schedule_vgm_initial_register_write(event)
        } else {
            transport.schedule_register_write(event)
        }
        .map_err(|error| external_error(&self.chip, error))
    }
}

impl<T: NdsifTransport> SoundChip for NdsifSoundChip<T> {
    /// Hardware output is external; host PCM is silent.
    fn has_no_audio_output(&self) -> bool {
        true
    }

    /// Return the chip type configured for this adapter.
    fn chip_type(&self) -> &Chip {
        &self.chip
    }

    /// Return the configured chip clock in hertz.
    fn clock(&self) -> u32 {
        self.clock
    }

    /// Return the floored native sample rate in hertz.
    fn sample_rate(&self) -> u32 {
        self.sample_rate.floor_hz()
    }

    /// Return the exact native sample-rate ratio.
    fn sample_rate_ratio(&self) -> SampleRate {
        self.sample_rate
    }

    /// Return the host-side channel count for this chip.
    fn channels(&self) -> usize {
        self.channels
    }

    /// Schedule an untimed write at output frame zero.
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        self.schedule_write(write, 0, self.output_rate, false)
    }

    /// Expose the adapter's frame-aware scheduling interface.
    fn scheduled(&mut self) -> Option<&mut dyn ScheduledChip> {
        Some(self)
    }

    /// Fill a valid host buffer with silence.
    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(ChipError::InvalidBufferLength {
                length: samples.len(),
                channels: self.channels,
            });
        }
        samples.fill(0);
        Ok(())
    }

    /// No local chip state needs resetting.
    fn reset(&mut self) {}
}

impl<T: NdsifTransport> ScheduledChip for NdsifSoundChip<T> {
    /// Schedule a register write at its absolute output frame.
    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, sample_frame, output_rate, false)
    }

    /// Check whether the transport supports YM2612 initialization batching.
    fn wants_vgm_initialization(&self) -> Result<bool, ChipError> {
        if self.chip != Chip::Ym2612 {
            return Ok(false);
        }
        Ok(self
            .transport
            .try_borrow()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?
            .supports_vgm_initialization())
    }

    /// Schedule an initial VGM register write through the transport's init path.
    fn apply_vgm_initial_write(
        &mut self,
        write: ChipRegisterWrite,
        frame: usize,
        rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, frame, rate, true)
    }

    /// Complete the transport's buffered VGM initialization sequence.
    fn finish_vgm_initialization(&mut self) -> Result<(), ChipError> {
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?
            .finish_vgm_initialization()
            .map_err(|error| external_error(&self.chip, error))
    }

    /// Send queued events through the requested output frame.
    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?
            .flush_scheduled_until(sample_frame, output_rate)
            .map_err(|error| external_error(&self.chip, error))
    }

    /// Finalize any pending transport-side DAC stream work.
    fn finish_scheduled(&mut self, sample_frame: usize, output_rate: u32) -> Result<(), ChipError> {
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?
            .finish_dac_stream(sample_frame, output_rate)
            .map_err(|error| external_error(&self.chip, error))
    }
}

/// NDSIF-backed SN76489 replacement that emits silence to the host PCM path.
///
/// T6W28 and tracks using non-default Game Gear stereo masks are rejected.
pub struct NdsifSn76489<T> {
    instance: u8,
    write_id: WriteId,
    clock: u32,
    output_rate: u32,
    sample_rate: SampleRate,
    stereo_enabled: bool,
    transport: Rc<RefCell<T>>,
}

impl<T: NdsifTransport> NdsifSn76489<T> {
    /// Configure an SN76489 instance and create its scheduled adapter.
    pub fn new(
        config: &ChipFactoryConfig<'_>,
        transport: Rc<RefCell<T>>,
    ) -> Result<Self, ChipError> {
        Self::with_target_instance(config, config.instance(), transport)
    }

    /// Configure a physical PSG instance independently of the source instance.
    pub fn with_target_instance(
        config: &ChipFactoryConfig<'_>,
        target: Instance,
        transport: Rc<RefCell<T>>,
    ) -> Result<Self, ChipError> {
        if config.header().misc().t6w28_detected == Some(true) {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "T6W28 PSG is not supported",
            });
        }
        if config.sn76489_stereo_used() {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "NDSIF SN76489 does not support Game Gear stereo masks",
            });
        }
        let instance = instance_id(target);
        let write_id = match target {
            Instance::Primary => WriteId::SN76489,
            Instance::Secondary => WriteId::SN76489_SECOND,
        };
        transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Sn76489, "NDSIF transport is already borrowed"))?
            .configure_chip(write_id, instance, config.clock())
            .map_err(|error| external_error(&Chip::Sn76489, error))?;

        Ok(Self {
            instance,
            write_id,
            clock: config.clock(),
            output_rate: config.output_rate(),
            sample_rate: SampleRate::from_ratio(
                config.clock(),
                NonZeroU32::new(16).expect("SN76489 divider is nonzero"),
            ),
            stereo_enabled: config.header().sn76489_flags.gamegear_stereo,
            transport,
        })
    }

    /// Validate and schedule a raw SN76489 byte on the transport.
    fn schedule_write(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        sample_rate: u32,
        vgm_initial: bool,
    ) -> Result<(), ChipError> {
        if let ChipRegisterWrite::GameGearPsg(spec) = &write {
            if !self.stereo_enabled || spec.value == 0xff {
                return Ok(());
            }
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "NDSIF SN76489 does not support Game Gear stereo masks",
            });
        }
        let value = match write {
            ChipRegisterWrite::Psg(spec) => spec.value,
            other => {
                return Err(ChipError::ChipMismatch {
                    expected: other.chip(),
                    actual: Chip::Sn76489,
                });
            }
        };
        let mut transport = self
            .transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Sn76489, "NDSIF transport is already borrowed"))?;
        let event = NdsifPsgEvent {
            write_id: self.write_id,
            instance: self.instance,
            page: 0,
            sample_frame,
            sample_rate,
            value,
        };
        if vgm_initial {
            transport.schedule_vgm_initial_psg_write(event)
        } else {
            transport.schedule_psg_write(event)
        }
        .map_err(|error| external_error(&Chip::Sn76489, error))
    }
}

impl<T: NdsifTransport> SoundChip for NdsifSn76489<T> {
    /// Hardware output is external; host PCM is silent.
    fn has_no_audio_output(&self) -> bool {
        true
    }

    /// Return the SN76489 chip type.
    fn chip_type(&self) -> &Chip {
        &Chip::Sn76489
    }

    /// Return the configured chip clock in hertz.
    fn clock(&self) -> u32 {
        self.clock
    }

    /// Return the floored native sample rate in hertz.
    fn sample_rate(&self) -> u32 {
        self.sample_rate.floor_hz()
    }

    /// Return the exact native sample-rate ratio.
    fn sample_rate_ratio(&self) -> SampleRate {
        self.sample_rate
    }

    /// Return the stereo channel count.
    fn channels(&self) -> usize {
        2
    }

    /// Schedule an untimed PSG write at output frame zero.
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        self.schedule_write(write, 0, self.output_rate, false)
    }

    /// Expose the adapter's frame-aware scheduling interface.
    fn scheduled(&mut self) -> Option<&mut dyn ScheduledChip> {
        Some(self)
    }

    /// Fill a valid stereo host buffer with silence.
    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        if !samples.len().is_multiple_of(2) {
            return Err(ChipError::InvalidBufferLength {
                length: samples.len(),
                channels: 2,
            });
        }
        samples.fill(0);
        Ok(())
    }

    /// No local chip state needs resetting.
    fn reset(&mut self) {}
}

impl<T: NdsifTransport> ScheduledChip for NdsifSn76489<T> {
    /// Schedule a PSG write at its absolute output frame.
    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, sample_frame, output_rate, false)
    }

    /// Check whether the transport supports VGM initialization batching.
    fn wants_vgm_initialization(&self) -> Result<bool, ChipError> {
        Ok(self
            .transport
            .try_borrow()
            .map_err(|_| external_error(&Chip::Sn76489, "NDSIF transport is already borrowed"))?
            .supports_vgm_initialization())
    }

    /// Schedule an initial VGM PSG write through the transport's init path.
    fn apply_vgm_initial_write(
        &mut self,
        write: ChipRegisterWrite,
        frame: usize,
        rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, frame, rate, true)
    }

    /// Complete the transport's buffered VGM initialization sequence.
    fn finish_vgm_initialization(&mut self) -> Result<(), ChipError> {
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Sn76489, "NDSIF transport is already borrowed"))?
            .finish_vgm_initialization()
            .map_err(|error| external_error(&Chip::Sn76489, error))
    }

    /// Send queued events through the requested output frame.
    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Sn76489, "NDSIF transport is already borrowed"))?
            .flush_scheduled_until(sample_frame, output_rate)
            .map_err(|error| external_error(&Chip::Sn76489, error))
    }
}

/// NDSIF-backed OKIM6258 replacement.
///
/// Register-1 ADPCM bytes are decoded as source audio and re-encoded for the
/// continuous device stream. `generate()` supplies silence to host PCM; the
/// device's OKIM6258 produces the audible output. Clock-byte writes at the
/// same frame are combined into one change.
/// The source and device predictors use 12-bit precision, matching the
/// NanoDrive reference player; the VGM output-precision flag is not applied.
/// NDSIF supports 4 MHz and 8 MHz clocks and positions changes at ADPCM byte
/// boundaries. Writes to `0x10..=0x17` are ignored for VGMMAME compatibility.
pub struct NdsifOkim6258<T> {
    instance: u8,
    clock: u32,
    divider: Divider,
    output_rate: u32,
    stream_finished: bool,
    next_byte_position: u32,
    timeline: Vec<AudioPositionClock>,
    pending_timing: Option<PendingOkiTiming>,
    source_decoder: super::oki_adpcm::OkiAdpcmState,
    output_decoder: super::oki_adpcm::OkiAdpcmState,
    source_playing: bool,
    source_data: VecDeque<u8>,
    transport: Rc<RefCell<T>>,
}

struct PendingOkiTiming {
    frame: usize,
    rate: u32,
    clock: u32,
    divider: Divider,
}

impl<T: NdsifTransport> NdsifOkim6258<T> {
    /// Configure an NDSIF audio stream from the sequencer's chip metadata.
    pub fn new(
        config: &ChipFactoryConfig<'_>,
        transport: Rc<RefCell<T>>,
    ) -> Result<Self, ChipError> {
        Self::with_target_instance(config, config.instance(), transport)
    }

    /// Configure a physical ADPCM instance independently of the source instance.
    pub fn with_target_instance(
        config: &ChipFactoryConfig<'_>,
        target: Instance,
        transport: Rc<RefCell<T>>,
    ) -> Result<Self, ChipError> {
        let instance = instance_id(target);
        let divider = match config.header().okim6258_flags.clock_divider {
            0 => Divider::Div1024,
            1 => Divider::Div768,
            _ => Divider::Div512,
        };
        if config.clock() < divider as u32 {
            return Err(ChipError::InvalidClock {
                clock: config.clock(),
                minimum: divider as u32,
            });
        }
        let chip = Chip::Okim6258;
        let clock = OkiClock::try_from(config.clock()).map_err(|_| {
            external_error(
                &chip,
                format!("unsupported OKIM6258 clock {} Hz", config.clock()),
            )
        })?;
        let timeline =
            AudioPositionClock::new(AudioTiming::new(clock, divider), config.output_rate())
                .map_err(|error| {
                    external_error(&chip, format!("invalid OKIM6258 timeline: {error}"))
                })?;
        {
            let mut transport = transport
                .try_borrow_mut()
                .map_err(|_| external_error(&chip, "NDSIF transport is already borrowed"))?;
            transport
                .configure_chip(WriteId::OKIM6258, instance, config.clock())
                .map_err(|error| external_error(&chip, error))?;
            transport
                .configure_audio_stream(instance, config.clock(), divider, config.output_rate())
                .map_err(|error| external_error(&chip, error))?;
            transport
                .schedule_audio_control(NdsifAudioControlEvent {
                    instance,
                    position: BytePosition::new(0),
                    sample_frame: 0,
                    sample_rate: config.output_rate(),
                    control: NdsifAudioControl::Pan(Pan::Both),
                })
                .map_err(|error| external_error(&chip, error))?;
        }

        Ok(Self {
            instance,
            clock: config.clock(),
            divider,
            output_rate: config.output_rate(),
            stream_finished: false,
            next_byte_position: 0,
            timeline: vec![timeline],
            pending_timing: None,
            source_decoder: super::oki_adpcm::OkiAdpcmState::new(12),
            output_decoder: super::oki_adpcm::OkiAdpcmState::new(12),
            source_playing: false,
            source_data: VecDeque::new(),
            transport,
        })
    }

    /// Translate an OKIM6258 write into positioned data or control events.
    fn schedule_write(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        sample_rate: u32,
    ) -> Result<(), ChipError> {
        let ChipRegisterWrite::Okim6258(Okim6258Spec { register, value }) = write else {
            return Err(ChipError::ChipMismatch {
                expected: Chip::Okim6258,
                actual: write.chip(),
            });
        };
        if (0x10..=0x17).contains(&register) {
            return Ok(());
        }
        let timing_write = matches!(register, 0x08..=0x0c);
        if self.pending_timing.as_ref().is_some_and(|pending| {
            pending.frame != sample_frame || pending.rate != sample_rate || !timing_write
        }) {
            self.commit_timing()?;
        }
        if timing_write {
            let pending = self.pending_timing.get_or_insert(PendingOkiTiming {
                frame: sample_frame,
                rate: sample_rate,
                clock: self.clock,
                divider: self.divider,
            });
            match register {
                0x08..=0x0b => {
                    let shift = u32::from(register - 0x08) * 8;
                    pending.clock =
                        (pending.clock & !(0xff << shift)) | (u32::from(value) << shift);
                }
                0x0c => {
                    pending.divider = match value & 3 {
                        0 => Divider::Div1024,
                        1 => Divider::Div768,
                        _ => Divider::Div512,
                    };
                }
                _ => unreachable!("timing register was checked"),
            }
            return Ok(());
        }
        self.render_until(sample_frame, sample_rate)?;
        let position = self.position_for_frame(sample_frame, sample_rate)?;
        let event = match register {
            0x00 => {
                if value & 4 != 0 {
                    return Err(external_error(
                        &Chip::Okim6258,
                        "OKIM6258 recording is not supported",
                    ));
                }
                if value & 1 != 0 {
                    self.source_playing = false;
                    self.source_data.clear();
                    self.source_decoder.reset();
                } else if value & 2 != 0 && !self.source_playing {
                    self.source_playing = true;
                    self.source_data.clear();
                    self.source_decoder.reset();
                }
                None
            }
            0x01 => {
                if self.source_playing {
                    const MAX_SOURCE_BYTES: usize = 65_536;
                    if self.source_data.len() >= MAX_SOURCE_BYTES {
                        return Err(external_error(
                            &Chip::Okim6258,
                            "OKIM6258 source queue capacity exceeded",
                        ));
                    }
                    self.source_data.push_back(value);
                }
                None
            }
            0x02 => {
                let pan = match value & 3 {
                    0 => Pan::Both,
                    1 => Pan::Right,
                    2 => Pan::Left,
                    _ => Pan::Off,
                };
                Some(Okim6258Event::Control(NdsifAudioControlEvent {
                    instance: self.instance,
                    position: BytePosition::new(self.next_byte_position.max(position.get())),
                    sample_frame,
                    sample_rate,
                    control: NdsifAudioControl::Pan(pan),
                }))
            }
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::Okim6258,
                    register,
                });
            }
        };

        let Some(event) = event else {
            return Ok(());
        };
        let mut transport = self
            .transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Okim6258, "NDSIF transport is already borrowed"))?;
        match event {
            Okim6258Event::Control(event) => transport
                .schedule_audio_control(event)
                .map_err(|error| external_error(&Chip::Okim6258, error)),
        }
    }

    /// Apply a coalesced clock or divider change at its scheduled position.
    fn commit_timing(&mut self) -> Result<(), ChipError> {
        let Some(pending) = self.pending_timing.take() else {
            return Ok(());
        };
        if pending.clock == self.clock && pending.divider == self.divider {
            return Ok(());
        }
        let clock = OkiClock::try_from(pending.clock).map_err(|_| {
            external_error(
                &Chip::Okim6258,
                format!("unsupported OKIM6258 clock {} Hz", pending.clock),
            )
        })?;
        self.render_until(pending.frame, pending.rate)?;
        let position = self.position_for_frame(pending.frame, pending.rate)?;
        let timeline = self
            .timeline
            .last()
            .expect("initial audio timeline exists")
            .change_at(
                pending.frame,
                pending.rate,
                position,
                AudioTiming::new(clock, pending.divider),
            )
            .map_err(|error| {
                external_error(
                    &Chip::Okim6258,
                    format!("invalid OKIM6258 clock change: {error}"),
                )
            })?;
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Okim6258, "NDSIF transport is already borrowed"))?
            .schedule_audio_control(NdsifAudioControlEvent {
                instance: self.instance,
                position,
                sample_frame: pending.frame,
                sample_rate: pending.rate,
                control: NdsifAudioControl::Clock {
                    clock_hz: pending.clock,
                    divider: pending.divider,
                },
            })
            .map_err(|error| external_error(&Chip::Okim6258, error))?;
        self.clock = pending.clock;
        self.divider = pending.divider;
        self.timeline.push(timeline);
        Ok(())
    }

    /// Convert a host frame to a nondecreasing ADPCM byte position.
    fn position_for_frame(
        &self,
        sample_frame: usize,
        sample_rate: u32,
    ) -> Result<BytePosition, ChipError> {
        let index = self
            .timeline
            .partition_point(|segment| segment.frame() <= sample_frame)
            .saturating_sub(1);
        let position = self.timeline[index]
            .position_for_frame(sample_frame, sample_rate)
            .map_err(|error| {
                external_error(&Chip::Okim6258, format!("invalid ADPCM position: {error}"))
            })?;
        Ok(BytePosition::new(
            position.get().max(self.next_byte_position),
        ))
    }

    /// Schedule one ADPCM byte and advance the next byte position.
    fn schedule_data(
        &mut self,
        sample_frame: usize,
        sample_rate: u32,
        value: u8,
    ) -> Result<(), ChipError> {
        let position = BytePosition::new(self.next_byte_position);
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Okim6258, "NDSIF transport is already borrowed"))?
            .schedule_adpcm_data(NdsifAdpcmEvent {
                instance: self.instance,
                position,
                sample_frame,
                sample_rate,
                value,
            })
            .map_err(|error| external_error(&Chip::Okim6258, error))?;
        self.next_byte_position = self
            .next_byte_position
            .checked_add(1)
            .ok_or_else(|| external_error(&Chip::Okim6258, "ADPCM position overflow"))?;
        Ok(())
    }

    /// Encode source ADPCM or silence up to the frame's byte position.
    fn render_until(&mut self, sample_frame: usize, sample_rate: u32) -> Result<(), ChipError> {
        let target = self.position_for_frame(sample_frame, sample_rate)?.get();
        let index = self
            .timeline
            .partition_point(|segment| segment.frame() <= sample_frame)
            .saturating_sub(1);
        while self.next_byte_position < target {
            let completed_position = self
                .next_byte_position
                .checked_add(1)
                .ok_or_else(|| external_error(&Chip::Okim6258, "ADPCM position overflow"))?;
            let due_frame = self.timeline[index]
                .first_frame_for_position(BytePosition::new(completed_position))
                .map_err(|error| {
                    external_error(&Chip::Okim6258, format!("invalid ADPCM frame: {error}"))
                })?;
            let samples = if self.source_playing {
                if let Some(value) = self.source_data.pop_front() {
                    self.source_decoder.decode_pair(value)
                } else {
                    [self.source_decoder.signal(); 2]
                }
            } else {
                [0; 2]
            };
            let value = self.output_decoder.encode_pair(samples);
            self.schedule_data(due_frame, sample_rate, value)?;
        }
        Ok(())
    }
}

enum Okim6258Event {
    Control(NdsifAudioControlEvent),
}

impl<T: NdsifTransport> SoundChip for NdsifOkim6258<T> {
    /// Hardware output is external; host PCM is silent.
    fn has_no_audio_output(&self) -> bool {
        true
    }

    /// Return the OKIM6258 chip type.
    fn chip_type(&self) -> &Chip {
        static CHIP: Chip = Chip::Okim6258;
        &CHIP
    }

    /// Return the configured chip clock in hertz.
    fn clock(&self) -> u32 {
        self.clock
    }

    /// Return the floored native sample rate in hertz.
    fn sample_rate(&self) -> u32 {
        self.clock / self.divider as u32
    }

    /// Return the exact native sample-rate ratio.
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(
            self.clock,
            NonZeroU32::new(self.divider as u32).expect("NDSIF dividers are nonzero"),
        )
    }

    /// Return the mono channel count.
    fn channels(&self) -> usize {
        1
    }

    /// Process an untimed register write at output frame zero.
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        self.schedule_write(write, 0, self.output_rate)
    }

    /// Expose the adapter's frame-aware scheduling interface.
    fn scheduled(&mut self) -> Option<&mut dyn ScheduledChip> {
        Some(self)
    }

    /// Fill the host buffer with silence.
    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        samples.fill(0);
        Ok(())
    }

    /// No local chip state needs resetting.
    fn reset(&mut self) {}
}

impl<T: NdsifTransport> ScheduledChip for NdsifOkim6258<T> {
    /// Process a register write at its absolute output frame.
    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, sample_frame, output_rate)
    }

    /// Start or stop the device's DAC stream at the requested frame.
    fn set_dac_stream_playing_at(
        &mut self,
        playing: bool,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(
            ChipRegisterWrite::Okim6258(Okim6258Spec {
                register: 0,
                value: if playing { 2 } else { 1 },
            }),
            sample_frame,
            output_rate,
        )
    }

    /// Report whether a stop write terminates the active DAC stream.
    fn stops_dac_stream_on_write(&self, write: &ChipRegisterWrite) -> bool {
        matches!(
            write,
            ChipRegisterWrite::Okim6258(Okim6258Spec { register: 0, value })
                if value & 1 != 0
        )
    }

    /// Commit any deferred timing change before scheduled transport work.
    fn prepare_scheduled(&mut self) -> Result<(), ChipError> {
        self.commit_timing()
    }

    /// Render and submit queued data through the requested output frame.
    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.commit_timing()?;
        self.render_until(sample_frame, output_rate)?;
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Okim6258, "NDSIF transport is already borrowed"))?
            .flush_scheduled_until(sample_frame, output_rate)
            .map_err(|error| external_error(&Chip::Okim6258, error))?;
        let keep = self
            .timeline
            .partition_point(|segment| segment.frame() <= sample_frame)
            .saturating_sub(1);
        self.timeline.drain(..keep);
        Ok(())
    }

    /// Flush the stream and send its final zero-pair control.
    fn finish_scheduled(&mut self, sample_frame: usize, output_rate: u32) -> Result<(), ChipError> {
        self.commit_timing()?;
        if self.stream_finished {
            return Ok(());
        }
        self.render_until(sample_frame, output_rate)?;
        for _ in 0..32 {
            let value = self.output_decoder.encode_zero_byte();
            self.schedule_data(sample_frame, output_rate, value)?;
        }
        let pair = ZeroPair::try_from(self.output_decoder.zero_pair())
            .map_err(|error| external_error(&Chip::Okim6258, error.to_string()))?;
        {
            let mut transport = self.transport.try_borrow_mut().map_err(|_| {
                external_error(&Chip::Okim6258, "NDSIF transport is already borrowed")
            })?;
            transport
                .flush_scheduled_until(sample_frame, output_rate)
                .map_err(|error| external_error(&Chip::Okim6258, error))?;
            transport
                .schedule_audio_control(NdsifAudioControlEvent {
                    instance: self.instance,
                    position: BytePosition::new(self.next_byte_position),
                    sample_frame,
                    sample_rate: output_rate,
                    control: NdsifAudioControl::End(pair),
                })
                .map_err(|error| external_error(&Chip::Okim6258, error))?;
        }
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Okim6258, "NDSIF transport is already borrowed"))?
            .finish_audio_stream(self.instance, sample_frame, output_rate)
            .map_err(|error| external_error(&Chip::Okim6258, error))?;
        self.stream_finished = true;
        Ok(())
    }
}

/// Register an NDSIF OKIM6258 replacement factory.
pub fn register_okim6258_factory<T: NdsifTransport + 'static>(
    registry: &mut ChipFactoryRegistry,
    transport: Rc<RefCell<T>>,
) {
    registry.register(Chip::Okim6258, move |config| {
        Ok(Box::new(NdsifOkim6258::new(config, Rc::clone(&transport))?))
    });
}

/// Register one YMFM chip type with an NDSIF transport.
pub fn register_ymfm_factory<T: NdsifTransport + 'static>(
    registry: &mut ChipFactoryRegistry,
    chip: Chip,
    transport: Rc<RefCell<T>>,
) -> Result<(), ChipError> {
    if chip == Chip::Ay8910 || ndsif_chip_config(&chip).is_none() {
        return Err(ChipError::UnsupportedChip(chip));
    }

    registry.register(chip, move |config| {
        Ok(Box::new(NdsifSoundChip::new(
            config,
            Rc::clone(&transport),
        )?))
    });
    Ok(())
}

/// Register an AY-3-8910 hardware replacement with an NDSIF transport.
pub fn register_ay8910_factory<T: NdsifTransport + 'static>(
    registry: &mut ChipFactoryRegistry,
    transport: Rc<RefCell<T>>,
) {
    registry.register(Chip::Ay8910, move |config| {
        Ok(Box::new(NdsifSoundChip::new(
            config,
            Rc::clone(&transport),
        )?))
    });
}

/// Register factories for all YMFM-backed chip types.
pub fn register_ymfm_factories<T: NdsifTransport + 'static>(
    registry: &mut ChipFactoryRegistry,
    transport: Rc<RefCell<T>>,
) -> Result<(), ChipError> {
    for chip in [
        Chip::Ym2413,
        Chip::Ym2151,
        Chip::Ym2203,
        Chip::Ym2608,
        Chip::Ym2610b,
        Chip::Ym2612,
        Chip::Ym3526,
        Chip::Y8950,
        Chip::Ym3812,
        Chip::Ymf262,
        Chip::Ymf278b,
    ] {
        register_ymfm_factory(registry, chip, Rc::clone(&transport))?;
    }
    Ok(())
}

/// Return the native divider, channel count, and write ID for a YMFM chip.
fn ndsif_chip_config(chip: &Chip) -> Option<(u32, usize, u8)> {
    Some(match chip {
        Chip::Ym2413 => (72, 2, 0x51),
        Chip::Ay8910 => (8, 1, 0xa0),
        Chip::Ym2151 => (64, 2, 0x54),
        Chip::Ym2203 => (4, 4, 0x55),
        Chip::Ym2608 => (8, 3, 0x56),
        Chip::Ym2610b => (16, 3, 0x58),
        Chip::Ym2612 => (144, 2, 0x52),
        Chip::Ym3526 => (72, 1, 0x5b),
        Chip::Y8950 => (72, 1, 0x5c),
        Chip::Ym3812 => (72, 1, 0x5a),
        Chip::Ymf262 => (288, 4, 0x5e),
        Chip::Ymf278b => (768, 6, 0xd0),
        _ => return None,
    })
}

/// Convert a chip write to its NDSIF ID, page, address, and value.
fn register_parts(write: &ChipRegisterWrite) -> Result<(u8, u8, u8, u8), ChipError> {
    let chip = write.chip();
    let invalid_bank = |bank| ChipError::InvalidRegisterBank {
        chip: chip.clone(),
        bank,
    };
    Ok(match write {
        ChipRegisterWrite::Ym2413(spec) => (0x51, 0, spec.register, spec.value),
        ChipRegisterWrite::Ay8910(spec) if spec.register <= 0x0f => {
            (0xa0, 0, spec.register, spec.value)
        }
        ChipRegisterWrite::Ay8910(spec) => {
            return Err(ChipError::InvalidRegister {
                chip,
                register: spec.register,
            });
        }
        ChipRegisterWrite::Ym2151(spec) => (0x54, 0, spec.register, spec.value),
        ChipRegisterWrite::Ym2203(spec) => (0x55, 0, spec.register, spec.value),
        ChipRegisterWrite::Ym2608(spec) => (
            match spec.port {
                0 => 0x56,
                1 => 0x57,
                bank => return Err(invalid_bank(bank)),
            },
            0,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ym2610(spec) => (
            match spec.port {
                0 => 0x58,
                1 => 0x59,
                bank => return Err(invalid_bank(bank)),
            },
            0,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ym2612(spec) => (
            match spec.port {
                0 => 0x52,
                1 => 0x53,
                bank => return Err(invalid_bank(bank)),
            },
            spec.port,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ym3526(spec) => (0x5b, 0, spec.register, spec.value),
        ChipRegisterWrite::Y8950(spec) => (0x5c, 0, spec.register, spec.value),
        ChipRegisterWrite::Ym3812(spec) => (0x5a, 0, spec.register, spec.value),
        ChipRegisterWrite::Ymf262(spec) => (
            match spec.port {
                0 => 0x5e,
                1 => 0x5f,
                bank => return Err(invalid_bank(bank)),
            },
            spec.port,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ymf278b(spec) if spec.port <= 2 => {
            (0xd0, spec.port, spec.register, spec.value)
        }
        ChipRegisterWrite::Ymf278b(spec) => return Err(invalid_bank(spec.port)),
        _ => return Err(ChipError::UnsupportedChip(chip)),
    })
}

/// Validate a protocol write ID and associate errors with its chip.
fn write_id_for(write_id: u8, chip: &Chip) -> Result<WriteId, ChipError> {
    WriteId::try_from(write_id).map_err(|error| external_error(chip, error.to_string()))
}

/// Convert a sequencer instance to its protocol instance number.
fn instance_id(instance: Instance) -> u8 {
    match instance {
        Instance::Primary => 0,
        Instance::Secondary => 1,
    }
}

/// Wrap a transport failure with its associated chip.
fn external_error(chip: &Chip, message: impl Into<String>) -> ChipError {
    ChipError::External {
        chip: chip.clone(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::chip::{
        Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec, Ym2612Spec,
        Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf278bSpec,
    };

    #[test]
    fn maps_ymfm_writes_to_ndsif_ids_and_pages() {
        let writes = [
            (
                ChipRegisterWrite::Ym2413(Ym2413Spec {
                    register: 1,
                    value: 2,
                }),
                (0x51, 0),
            ),
            (
                ChipRegisterWrite::Ym2151(Ym2151Spec {
                    register: 1,
                    value: 2,
                }),
                (0x54, 0),
            ),
            (
                ChipRegisterWrite::Ym2203(Ym2203Spec {
                    register: 1,
                    value: 2,
                }),
                (0x55, 0),
            ),
            (
                ChipRegisterWrite::Ym2608(Ym2608Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x56, 0),
            ),
            (
                ChipRegisterWrite::Ym2608(Ym2608Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x57, 0),
            ),
            (
                ChipRegisterWrite::Ym2610(Ym2610Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x58, 0),
            ),
            (
                ChipRegisterWrite::Ym2610(Ym2610Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x59, 0),
            ),
            (
                ChipRegisterWrite::Ym2612(Ym2612Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x52, 0),
            ),
            (
                ChipRegisterWrite::Ym2612(Ym2612Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x53, 1),
            ),
            (
                ChipRegisterWrite::Ym3526(Ym3526Spec {
                    register: 1,
                    value: 2,
                }),
                (0x5b, 0),
            ),
            (
                ChipRegisterWrite::Y8950(Y8950Spec {
                    register: 1,
                    value: 2,
                }),
                (0x5c, 0),
            ),
            (
                ChipRegisterWrite::Ym3812(Ym3812Spec {
                    register: 1,
                    value: 2,
                }),
                (0x5a, 0),
            ),
            (
                ChipRegisterWrite::Ymf262(Ymf262Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x5e, 0),
            ),
            (
                ChipRegisterWrite::Ymf262(Ymf262Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x5f, 1),
            ),
            (
                ChipRegisterWrite::Ymf278b(Ymf278bSpec {
                    port: 2,
                    register: 1,
                    value: 2,
                }),
                (0xd0, 2),
            ),
        ];

        for (write, expected) in writes {
            let (write_id, page, address, value) = register_parts(&write).unwrap();
            assert_eq!((write_id, page), expected);
            assert_eq!((address, value), (1, 2));
        }
    }
}
