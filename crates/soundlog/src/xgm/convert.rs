//! Lazy XGM v1 playback as VGM chip writes.

use super::command::XgmCommand;
use super::document::XgmDocument;
use crate::ParseError;
use crate::chip::{PsgSpec, Ym2612Spec};
use crate::vgm::command::{EndOfData, Instance, VgmCommand, WaitSamples};
use crate::vgm::{VgmCommandGenerator, VgmStream};
use std::collections::VecDeque;

const PCM_RATE: u64 = 14_000;
const VGM_RATE: u64 = 44_100;
const MAX_COMMANDS_WITHOUT_OUTPUT: usize = 1_048_576;

/// Playback options for a single XGM track.
#[derive(Debug, Clone, Copy)]
pub struct XgmPlaybackOptions {
    /// Zero-based track index.
    pub track: usize,
    /// Number of plays, including the first; `None` loops indefinitely.
    /// Zero is treated as one play.
    pub loop_count: Option<u32>,
}

impl Default for XgmPlaybackOptions {
    fn default() -> Self {
        Self {
            track: 0,
            loop_count: Some(1),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct PcmChannel {
    sample: Option<u8>,
    position: usize,
    priority: u8,
}

/// A bounded, lazy XGM-to-VGM command generator.
///
/// PCM samples are mixed as signed 8-bit values at the nominal 14 kHz XGM
/// rate and emitted as YM2612 DAC writes on the 44.1 kHz VGM timeline.
/// Looping is handled by the generator, not by [`VgmStream`].
#[derive(Debug)]
pub struct XgmVgmGenerator {
    document: XgmDocument,
    options: XgmPlaybackOptions,
    command_index: usize,
    loops: u32,
    channels: [PcmChannel; 4],
    pending: VecDeque<VgmCommand>,
    time: u64,
    next_pcm_index: u64,
    wait: u16,
    dac_enabled: bool,
    dac_requested: u8,
    dac_dirty: bool,
    finished: bool,
}

impl XgmVgmGenerator {
    /// Validate a document and create a generator for the selected track.
    pub fn new(document: XgmDocument, mut options: XgmPlaybackOptions) -> Result<Self, ParseError> {
        document.validate()?;
        if options.track >= document.tracks.len() {
            return Err(ParseError::DataInconsistency(
                "XGM track index is out of range".into(),
            ));
        }
        if options.loop_count == Some(0) {
            options.loop_count = Some(1);
        }
        Ok(Self {
            document,
            options,
            command_index: 0,
            loops: 0,
            channels: [PcmChannel::default(); 4],
            pending: VecDeque::new(),
            time: 0,
            next_pcm_index: 0,
            wait: 0,
            dac_enabled: false,
            dac_requested: 0,
            dac_dirty: false,
            finished: false,
        })
    }

    fn ym(register: u8, value: u8, port: u8) -> VgmCommand {
        VgmCommand::Ym2612Write(
            Instance::Primary,
            Ym2612Spec {
                port,
                register,
                value,
            },
        )
    }

    fn pcm_tick(&mut self) {
        let mut mixed = 0i32;
        let mut active = false;
        for channel in &mut self.channels {
            let Some(id) = channel.sample else { continue };
            let sample = self.document.samples[usize::from(id - 1)].expect("validated sample");
            let index = sample.offset as usize + channel.position;
            mixed = (mixed + i32::from(self.document.sample_data[index] as i8)).clamp(-128, 127);
            active = true;
            channel.position += 1;
            if channel.position == sample.length as usize {
                channel.sample = None;
            }
        }
        if active {
            if !self.dac_enabled {
                self.pending.push_back(Self::ym(0x2b, 0x80, 0));
                self.dac_enabled = true;
            }
            self.pending
                .push_back(Self::ym(0x2a, (mixed.clamp(-128, 127) + 128) as u8, 0));
            self.dac_dirty = true;
        } else if self.dac_dirty {
            self.pending.push_back(Self::ym(0x2a, 0x80, 0));
            let requested_enabled = self.dac_requested & 0x80 != 0;
            if self.dac_enabled != requested_enabled {
                self.pending
                    .push_back(Self::ym(0x2b, self.dac_requested, 0));
                self.dac_enabled = requested_enabled;
            }
            self.dac_dirty = false;
        }
    }

    fn tick_time(index: u64) -> Result<u64, ParseError> {
        u64::try_from(u128::from(index) * u128::from(VGM_RATE) / u128::from(PCM_RATE))
            .map_err(|_| ParseError::DataInconsistency("XGM playback time overflow".into()))
    }

    fn wait_command(&mut self) -> Result<Option<VgmCommand>, ParseError> {
        if !self.channels.iter().any(|channel| channel.sample.is_some()) && !self.dac_dirty {
            let wait = self.wait;
            self.wait = 0;
            self.time = self.time.checked_add(u64::from(wait)).ok_or_else(|| {
                ParseError::DataInconsistency("XGM playback time overflow".into())
            })?;
            return Ok(Some(VgmCommand::WaitSamples(WaitSamples(wait))));
        }
        let earliest =
            (u128::from(self.time) * u128::from(PCM_RATE)).div_ceil(u128::from(VGM_RATE));
        self.next_pcm_index = self.next_pcm_index.max(
            u64::try_from(earliest)
                .map_err(|_| ParseError::DataInconsistency("XGM PCM time overflow".into()))?,
        );
        let tick = Self::tick_time(self.next_pcm_index)?;
        let distance = tick - self.time;
        if distance >= u64::from(self.wait) {
            let wait = self.wait;
            self.wait = 0;
            self.time = self.time.checked_add(u64::from(wait)).ok_or_else(|| {
                ParseError::DataInconsistency("XGM playback time overflow".into())
            })?;
            return Ok(Some(VgmCommand::WaitSamples(WaitSamples(wait))));
        }
        if distance > 0 {
            let wait = distance as u16;
            self.wait -= wait;
            self.time += distance;
            return Ok(Some(VgmCommand::WaitSamples(WaitSamples(wait))));
        }
        self.next_pcm_index = self
            .next_pcm_index
            .checked_add(1)
            .ok_or_else(|| ParseError::DataInconsistency("XGM PCM time overflow".into()))?;
        self.pcm_tick();
        Ok(self.pending.pop_front())
    }
}

impl VgmCommandGenerator for XgmVgmGenerator {
    fn next_command(&mut self) -> Result<Option<VgmCommand>, ParseError> {
        for _ in 0..MAX_COMMANDS_WITHOUT_OUTPUT {
            if let Some(command) = self.pending.pop_front() {
                return Ok(Some(command));
            }
            if self.finished {
                return Ok(None);
            }
            if self.wait != 0 {
                if let Some(command) = self.wait_command()? {
                    return Ok(Some(command));
                }
                continue;
            }
            let track = &self.document.tracks[self.options.track];
            let Some(command) = track.commands.get(self.command_index) else {
                return Err(ParseError::DataInconsistency(
                    "XGM track ended without an end or loop command".into(),
                ));
            };
            self.command_index += 1;
            match command {
                XgmCommand::Frame => self.wait = self.document.timing.frame_samples(),
                XgmCommand::Psg(bytes) => {
                    self.pending.extend(bytes.iter().map(|&value| {
                        VgmCommand::Sn76489Write(Instance::Primary, PsgSpec { value })
                    }));
                }
                XgmCommand::Ym2612Port0(writes) | XgmCommand::Ym2612Port1(writes) => {
                    let port = u8::from(matches!(command, XgmCommand::Ym2612Port1(_)));
                    for write in writes {
                        if port == 0 && write.register == 0x2b {
                            self.dac_enabled = write.value & 0x80 != 0;
                            self.dac_requested = write.value;
                        }
                        self.pending
                            .push_back(Self::ym(write.register, write.value, port));
                    }
                }
                XgmCommand::Ym2612Key(values) => {
                    self.pending
                        .extend(values.iter().map(|&value| Self::ym(0x28, value, 0)));
                }
                XgmCommand::Pcm {
                    channel,
                    priority,
                    sample_id,
                } => {
                    let channel = &mut self.channels[usize::from(*channel)];
                    if channel.sample.is_none() || channel.priority <= *priority {
                        channel.sample = (*sample_id != 0).then_some(*sample_id);
                        channel.position = 0;
                        channel.priority = *priority;
                    }
                }
                XgmCommand::Loop(target) => {
                    self.loops = self.loops.saturating_add(1);
                    if self
                        .options
                        .loop_count
                        .is_some_and(|count| self.loops >= count)
                    {
                        self.finished = true;
                        return Ok(Some(VgmCommand::EndOfData(EndOfData)));
                    }
                    self.command_index = *target;
                }
                XgmCommand::End => {
                    self.finished = true;
                    return Ok(Some(VgmCommand::EndOfData(EndOfData)));
                }
            }
        }
        Err(ParseError::CommandLimitExceeded {
            limit: MAX_COMMANDS_WITHOUT_OUTPUT as u32,
        })
    }
}

impl TryFrom<(XgmDocument, XgmPlaybackOptions)> for VgmStream {
    type Error = ParseError;

    fn try_from(
        (document, options): (XgmDocument, XgmPlaybackOptions),
    ) -> Result<Self, Self::Error> {
        Ok(Self::from_generator(Box::new(XgmVgmGenerator::new(
            document, options,
        )?)))
    }
}
