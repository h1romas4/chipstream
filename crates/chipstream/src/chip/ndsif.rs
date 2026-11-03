//! Scheduled chipstream adapters for external NDSIF transports.
//!
//! Hardware-backed chips emit silence into host PCM; audible output must be
//! captured or played through the physical device. Transport events use
//! absolute output-frame positions and can arrive out of chronological order.

use std::{cell::RefCell, num::NonZeroU32, rc::Rc};

use ::ndsif::{
    AudioPositionClock, AudioTiming, BytePosition, Divider, OkiClock, Pan, RegisterWrite, WriteId,
    ZeroPair,
};
use soundlog::chip::{Chip, Okim6258Spec};
use soundlog::vgm::command::Instance;

use crate::{
    chip::{ChipError, ChipRegisterWrite, SampleRate, SoundChip},
    sequencer::{ChipFactoryConfig, ChipFactoryRegistry},
};

/// One timed register write for an NDSIF transport to schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdsifRegisterEvent {
    /// Chip type represented by the write.
    pub chip: Chip,
    /// NDSIF target command ID.
    pub write_id: WriteId,
    /// Target chip instance.
    pub instance: u8,
    /// Target register page.
    pub page: u8,
    /// Register address and value.
    pub write: RegisterWrite,
    /// Absolute host output-frame position.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
}

/// One timed raw byte for an NDSIF SN76489 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdsifPsgEvent {
    /// NDSIF target command ID.
    pub write_id: WriteId,
    /// Target chip instance.
    pub instance: u8,
    /// Target page.
    pub page: u8,
    /// Absolute host output-frame position.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
    /// Raw PSG byte.
    pub value: u8,
}

/// One source ADPCM byte for an NDSIF OKIM6258 stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdsifAdpcmEvent {
    /// Audio stream instance.
    pub instance: u8,
    /// Absolute position in ADPCM bytes.
    pub position: BytePosition,
    /// Absolute host output-frame position.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
    /// Encoded ADPCM byte.
    pub value: u8,
}

/// A control change positioned on the NDSIF ADPCM stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NdsifAudioControl {
    /// Change the stream clock and divider.
    Clock {
        /// Input clock in hertz.
        clock_hz: u32,
        /// Clock divider.
        divider: Divider,
    },
    /// Change the output-channel selection.
    Pan(Pan),
    /// End the stream using a state-dependent repeated ADPCM byte.
    End(ZeroPair),
}

/// One positioned audio-stream control event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NdsifAudioControlEvent {
    /// Audio stream instance.
    pub instance: u8,
    /// Absolute position in ADPCM bytes.
    pub position: BytePosition,
    /// Absolute host output-frame position.
    pub sample_frame: usize,
    /// Host output rate in frames per second.
    pub sample_rate: u32,
    /// Stream control to execute.
    pub control: NdsifAudioControl,
}

/// Schedules chip writes on an NDSIF-capable device.
///
/// Implementations must queue writes for `sample_frame`; they must not treat
/// this call as an instruction to write to the device immediately. Sequencer
/// rendering is ahead of playback, and events from different chip instances
/// may arrive out of chronological order.
pub trait NdsifTransport {
    /// Configure a chip instance before playback.
    fn configure_chip(
        &mut self,
        write_id: WriteId,
        instance: u8,
        clock_hz: u32,
    ) -> Result<(), String>;

    /// Queue a register write at its absolute output-frame position.
    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String>;

    /// Queue a raw PSG byte at its absolute output-frame position.
    fn schedule_psg_write(&mut self, _event: NdsifPsgEvent) -> Result<(), String> {
        Err("NDSIF transport does not support raw PSG writes".to_owned())
    }

    /// Configure an OKIM6258 stream and its host timing.
    fn configure_audio_stream(
        &mut self,
        _instance: u8,
        _clock_hz: u32,
        _divider: Divider,
        _output_rate: u32,
    ) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Queue one ADPCM byte at its absolute output-frame position.
    fn schedule_adpcm_data(&mut self, _event: NdsifAdpcmEvent) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Queue a stream control at its absolute output-frame position.
    fn schedule_audio_control(&mut self, _event: NdsifAudioControlEvent) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Commit all queued events through an output-frame position.
    fn flush_scheduled_until(
        &mut self,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Start a configured audio stream after prebuffering.
    fn start_audio_stream(&mut self, _instance: u8) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Finish an audio stream at an absolute output-frame position.
    fn finish_audio_stream(
        &mut self,
        _instance: u8,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), String> {
        Err("NDSIF transport does not support audio streams".to_owned())
    }

    /// Seal and drain an optional timed DAC profile at end of playback.
    fn finish_dac_stream(&mut self, _sample_frame: usize, _output_rate: u32) -> Result<(), String> {
        Ok(())
    }
}

/// NDSIF-backed chip replacement that emits silence to the host PCM path.
///
/// The physical chip produces the audible output. Register events are handed
/// to the injected transport with absolute output-frame positions.
///
/// Untimed calls to [`SoundChip::apply_register_write`] are scheduled at frame
/// zero. Sequencer playback uses [`SoundChip::apply_register_write_at`] and
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
        let chip = config.chip().clone();
        let Some((divisor, channels, write_id)) = ndsif_chip_config(&chip) else {
            return Err(ChipError::UnsupportedChip(chip));
        };
        if chip == Chip::Ay8910
            && (config.header().ay_chip_type != soundlog::vgm::header::Ay8910ChipType::Ay8910
                || config.header().ay8910_flags.ym_pin26_low
                || config.header().ay8910_flags.discrete_output
                || config.header().ay8910_flags.raw_output
                || config.header().ay8910_flags.reserved != 0)
        {
            return Err(ChipError::InvalidConfiguration {
                chip,
                reason: "NDSIF AY routing requires AY-3-8910 with mixed output and no YM clock-divider flags",
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
            chip: self.chip.clone(),
            write_id: write_id_for(write_id, &self.chip)?,
            instance: self.instance,
            page,
            write: RegisterWrite::new(address, value),
            sample_frame,
            sample_rate,
        };
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?
            .schedule_register_write(event)
            .map_err(|error| external_error(&self.chip, error))
    }
}

impl<T: NdsifTransport> SoundChip for NdsifSoundChip<T> {
    fn chip_type(&self) -> &Chip {
        &self.chip
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate.floor_hz()
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        self.sample_rate
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        self.schedule_write(write, 0, self.output_rate)
    }

    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, sample_frame, output_rate)
    }

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

    fn finish_scheduled(&mut self, sample_frame: usize, output_rate: u32) -> Result<(), ChipError> {
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?
            .finish_dac_stream(sample_frame, output_rate)
            .map_err(|error| external_error(&self.chip, error))
    }

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

    fn reset(&mut self) {}
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
        let (page, register, value) = write.byte_parts_for(&Chip::Sn76489)?;
        if page != 0 {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::Sn76489,
                bank: page,
            });
        }
        if register != 0 {
            return Err(ChipError::InvalidRegister {
                chip: Chip::Sn76489,
                register,
            });
        }
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&Chip::Sn76489, "NDSIF transport is already borrowed"))?
            .schedule_psg_write(NdsifPsgEvent {
                write_id: self.write_id,
                instance: self.instance,
                page,
                sample_frame,
                sample_rate,
                value,
            })
            .map_err(|error| external_error(&Chip::Sn76489, error))
    }
}

impl<T: NdsifTransport> SoundChip for NdsifSn76489<T> {
    fn chip_type(&self) -> &Chip {
        &Chip::Sn76489
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate.floor_hz()
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        self.sample_rate
    }

    fn channels(&self) -> usize {
        2
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        self.schedule_write(write, 0, self.output_rate)
    }

    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, sample_frame, output_rate)
    }

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

    fn reset(&mut self) {}
}

/// NDSIF-backed OKIM6258 replacement.
///
/// Register-1 ADPCM bytes are transferred to the device stream. `generate()`
/// supplies silence to host PCM; the device's OKIM6258 produces the audible
/// output. Clock-byte writes at the same frame are combined into one change.
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
    decoder: super::oki_adpcm::OkiAdpcmState,
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
        let output_bits = if config.header().okim6258_flags.output_12bit {
            12
        } else {
            10
        };
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
            decoder: super::oki_adpcm::OkiAdpcmState::new(output_bits),
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
        let position = self.position_for_frame(sample_frame, sample_rate)?;
        let event = match register {
            0x00 => None,
            0x01 => {
                while self.next_byte_position < position.get() {
                    let value = self.decoder.encode_zero_byte();
                    self.schedule_data(sample_frame, sample_rate, value)?;
                }
                self.decoder.decode_byte(value);
                Some(Okim6258Event::Data(NdsifAdpcmEvent {
                    instance: self.instance,
                    position: BytePosition::new(self.next_byte_position),
                    sample_frame,
                    sample_rate,
                    value,
                }))
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
            Okim6258Event::Data(event) => {
                transport
                    .schedule_adpcm_data(event)
                    .map_err(|error| external_error(&Chip::Okim6258, error))?;
                self.next_byte_position = self
                    .next_byte_position
                    .checked_add(1)
                    .ok_or_else(|| external_error(&Chip::Okim6258, "ADPCM position overflow"))?;
                Ok(())
            }
            Okim6258Event::Control(event) => transport
                .schedule_audio_control(event)
                .map_err(|error| external_error(&Chip::Okim6258, error)),
        }
    }

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
}

enum Okim6258Event {
    Data(NdsifAdpcmEvent),
    Control(NdsifAudioControlEvent),
}

impl<T: NdsifTransport> SoundChip for NdsifOkim6258<T> {
    fn chip_type(&self) -> &Chip {
        static CHIP: Chip = Chip::Okim6258;
        &CHIP
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.clock / self.divider as u32
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(
            self.clock,
            NonZeroU32::new(self.divider as u32).expect("NDSIF dividers are nonzero"),
        )
    }

    fn channels(&self) -> usize {
        1
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        self.schedule_write(write, 0, self.output_rate)
    }

    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, sample_frame, output_rate)
    }

    fn prepare_scheduled(&mut self) -> Result<(), ChipError> {
        self.commit_timing()
    }

    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.commit_timing()?;
        let target = self.position_for_frame(sample_frame, output_rate)?.get();
        while self.next_byte_position < target {
            let value = self.decoder.encode_zero_byte();
            self.schedule_data(sample_frame, output_rate, value)?;
        }
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

    fn finish_scheduled(&mut self, sample_frame: usize, output_rate: u32) -> Result<(), ChipError> {
        self.commit_timing()?;
        if self.stream_finished {
            return Ok(());
        }
        let target = self.position_for_frame(sample_frame, output_rate)?.get();
        while self.next_byte_position < target {
            let value = self.decoder.encode_zero_byte();
            self.schedule_data(sample_frame, output_rate, value)?;
        }
        for _ in 0..32 {
            let value = self.decoder.encode_zero_byte();
            self.schedule_data(sample_frame, output_rate, value)?;
        }
        let pair = ZeroPair::try_from(self.decoder.zero_pair())
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

    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        samples.fill(0);
        Ok(())
    }

    fn reset(&mut self) {}
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
