//! Lazy S98-to-VGM command generation.

use super::document::{S98Command, S98Device, S98DeviceType, S98Document, S98Port};
use crate::ParseError;
use crate::chip;
use crate::vgm::command::{Instance, VgmCommand, WaitSamples};
use crate::vgm::header::Ay8910ChipType;
use crate::{ExecutionEvent, VgmCommandGenerator, VgmHeader};
use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

/// Resource limits and repetition policy for S98 playback.
#[derive(Debug, Clone, Copy)]
pub struct S98PlaybackOptions {
    /// Number of plays, including the first; `None` repeats indefinitely.
    /// Zero is treated as one play.
    pub loop_count: Option<u32>,
    /// Maximum total elapsed S98 ticks, including repeats. Default: 100,000,000.
    pub max_ticks: Option<u64>,
    /// Maximum source steps without producing a sample wait, including loop
    /// boundaries.
    /// Default: 100,000. `None` disables the limit and can permit an
    /// infinite zero-time loop inside `next_command`.
    pub max_commands_without_wait: Option<u32>,
}

impl Default for S98PlaybackOptions {
    fn default() -> Self {
        Self {
            loop_count: Some(1),
            max_ticks: Some(100_000_000),
            max_commands_without_wait: Some(100_000),
        }
    }
}

/// Error produced while generating S98 playback commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum S98PlaybackError {
    /// The configured tick limit would be exceeded.
    TickLimitExceeded {
        /// Configured limit.
        max_ticks: u64,
    },
    /// The configured command budget was exceeded.
    CommandLimitExceeded {
        /// Configured limit.
        max_commands_without_wait: u32,
    },
    /// Elapsed time exceeded the representable sample or tick range.
    TimeOverflow,
}

impl fmt::Display for S98PlaybackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TickLimitExceeded { max_ticks } => {
                write!(f, "S98 playback tick limit exceeded ({max_ticks})")
            }
            Self::CommandLimitExceeded {
                max_commands_without_wait,
            } => write!(
                f,
                "S98 playback command limit exceeded ({max_commands_without_wait})"
            ),
            Self::TimeOverflow => write!(f, "S98 playback time overflow"),
        }
    }
}

impl std::error::Error for S98PlaybackError {}

impl From<S98PlaybackError> for ParseError {
    fn from(error: S98PlaybackError) -> Self {
        Self::GeneratorError(Box::new(error))
    }
}

/// Notification that the selected finite loop boundary was reached.
#[derive(Debug, Clone, Default)]
pub struct S98LoopMarker(Rc<Cell<Option<u64>>>);

impl S98LoopMarker {
    /// Whether playback has reached the configured loop boundary.
    pub fn reached(&self) -> bool {
        self.0.get().is_some()
    }

    /// The boundary on the 44,100 Hz generation timeline.
    pub fn sample(&self) -> Option<u64> {
        self.0.get()
    }
}

struct Observer(Box<dyn FnMut(ExecutionEvent<usize>) -> Result<(), ParseError>>);

impl fmt::Debug for Observer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Observer(..)")
    }
}

/// A bounded, lazy S98-to-VGM generator.
///
/// The generator handles repeats itself. Its [`vgm_header`](Self::vgm_header)
/// must be used to configure the VGM consumer. Device panning, unequal clocks
/// for two instances, and device types not faithfully representable here are
/// rejected rather than silently discarded.
#[derive(Debug)]
pub struct S98VgmGenerator {
    document: S98Document,
    header: VgmHeader,
    devices: Vec<S98Device>,
    instances: Vec<Option<Instance>>,
    options: S98PlaybackOptions,
    index: usize,
    plays: u32,
    ticks: u64,
    sample: u64,
    remainder: u128,
    pending_wait: u64,
    commands_without_wait: u32,
    finished: bool,
    finished_at_loop: bool,
    loop_marker: Option<S98LoopMarker>,
    observer: Option<Observer>,
}

fn invalid(message: impl Into<String>) -> ParseError {
    ParseError::DataInconsistency(message.into())
}

impl S98VgmGenerator {
    /// Validate a document and prepare VGM chip configuration.
    pub fn new(document: S98Document, mut options: S98PlaybackOptions) -> Result<Self, ParseError> {
        document.validate()?;
        if options.loop_count == Some(0) {
            options.loop_count = Some(1);
        }
        let mut header = VgmHeader::default();
        let devices = document.header.effective_devices();
        let mut instances = Vec::with_capacity(devices.len());
        let mut kinds: Vec<Option<S98DeviceType>> = Vec::new();
        for device in &devices {
            if device.kind == S98DeviceType::None {
                instances.push(None);
                kinds.push(None);
                continue;
            }
            if device.pan != 0 {
                return Err(invalid(
                    "S98 device panning cannot be represented by this generator",
                ));
            }
            if device.clock == 0 || device.clock >= 0x4000_0000 {
                return Err(invalid("S98 device clock is invalid for VGM"));
            }
            let normalized = match device.kind {
                S98DeviceType::Ym2149 | S98DeviceType::Ay8910 => S98DeviceType::Ay8910,
                S98DeviceType::Sn76489 | S98DeviceType::Unknown(_) => {
                    return Err(invalid(format!("unsupported S98 device {:?}", device.kind)));
                }
                other => other,
            };
            let previous: Vec<usize> = kinds
                .iter()
                .enumerate()
                .filter_map(|(i, k)| (*k == Some(normalized)).then_some(i))
                .collect();
            if previous.len() >= 2 {
                return Err(invalid(
                    "VGM supports at most two instances of an S98 device",
                ));
            }
            let clock = match normalized {
                S98DeviceType::Ay8910 => &mut header.ay8910_clock,
                S98DeviceType::Ym2203 => &mut header.ym2203_clock,
                S98DeviceType::Ym2612 => &mut header.ym2612_clock,
                S98DeviceType::Ym2608 => &mut header.ym2608_clock,
                S98DeviceType::Ym2151 => &mut header.ym2151_clock,
                S98DeviceType::Ym2413 => &mut header.ym2413_clock,
                S98DeviceType::Ym3526 => &mut header.ym3526_clock,
                S98DeviceType::Ym3812 => &mut header.ym3812_clock,
                S98DeviceType::Ymf262 => &mut header.ymf262_clock,
                _ => return Err(invalid("unsupported S98 device")),
            };
            let instance = if let Some(&first) = previous.first() {
                if *clock != device.clock || devices[first].kind != device.kind {
                    return Err(invalid(
                        "S98 dual devices require matching models and clocks",
                    ));
                }
                *clock |= 0x4000_0000;
                Instance::Secondary
            } else {
                *clock = device.clock;
                Instance::Primary
            };
            if device.kind == S98DeviceType::Ym2149 {
                header.ay_chip_type = Ay8910ChipType::Ym2149;
            } else if device.kind == S98DeviceType::Ay8910 {
                header.ay_chip_type = Ay8910ChipType::Ay8910;
            }
            kinds.push(Some(normalized));
            instances.push(Some(instance));
        }
        for command in &document.commands {
            if let S98Command::Write {
                device,
                port: S98Port::Extended,
                ..
            } = command
            {
                let kind = devices[usize::from(*device)].kind;
                if !matches!(
                    kind,
                    S98DeviceType::None
                        | S98DeviceType::Ym2612
                        | S98DeviceType::Ym2608
                        | S98DeviceType::Ymf262
                ) {
                    return Err(invalid(format!("S98 device {kind:?} has no extended port")));
                }
            }
        }
        Ok(Self {
            document,
            header,
            devices,
            instances,
            options,
            index: 0,
            plays: 1,
            ticks: 0,
            sample: 0,
            remainder: 0,
            pending_wait: 0,
            commands_without_wait: 0,
            finished: false,
            finished_at_loop: false,
            loop_marker: None,
            observer: None,
        })
    }

    /// Continue after the selected finite loop boundary and expose a marker.
    pub fn new_with_loop_marker(
        document: S98Document,
        options: S98PlaybackOptions,
    ) -> Result<(Self, S98LoopMarker), ParseError> {
        if options.loop_count.is_none() {
            return Err(invalid("S98 loop marker requires a finite loop count"));
        }
        let mut generator = Self::new(document, options)?;
        let marker = S98LoopMarker::default();
        generator.loop_marker = Some(marker.clone());
        Ok((generator, marker))
    }

    /// VGM header describing the generated chip configuration.
    pub fn vgm_header(&self) -> &VgmHeader {
        &self.header
    }

    /// Whether playback stopped at its configured loop boundary.
    pub fn finished_at_loop(&self) -> bool {
        self.finished_at_loop
    }

    /// Install a synchronous observer called before each source command.
    ///
    /// Events carry the command index and the generation timeline in samples.
    /// The callback can run ahead of device playback and again during replay.
    /// A reported command may subsequently fail during execution.
    /// An observer error is returned to the caller of `next_command`.
    pub fn set_observer(
        &mut self,
        observer: impl FnMut(ExecutionEvent<usize>) -> Result<(), ParseError> + 'static,
    ) {
        self.observer = Some(Observer(Box::new(observer)));
    }

    fn budget(&mut self) -> Result<(), ParseError> {
        if let Some(max) = self.options.max_commands_without_wait
            && self.commands_without_wait >= max
        {
            return Err(S98PlaybackError::CommandLimitExceeded {
                max_commands_without_wait: max,
            }
            .into());
        }
        self.commands_without_wait = self.commands_without_wait.saturating_add(1);
        Ok(())
    }

    fn emit_wait(&mut self) -> Result<Option<VgmCommand>, ParseError> {
        let count = self.pending_wait.min(u64::from(u16::MAX)) as u16;
        self.pending_wait -= u64::from(count);
        self.sample = self
            .sample
            .checked_add(u64::from(count))
            .ok_or(S98PlaybackError::TimeOverflow)?;
        self.commands_without_wait = 0;
        Ok(Some(VgmCommand::WaitSamples(WaitSamples(count))))
    }
}

impl VgmCommandGenerator for S98VgmGenerator {
    fn next_command(&mut self) -> Result<Option<VgmCommand>, ParseError> {
        if self.finished {
            return Ok(None);
        }
        if self.pending_wait != 0 {
            return self.emit_wait();
        }
        loop {
            if self.index == self.document.commands.len()
                && (self.document.loop_index.is_none()
                    || (self.loop_marker.is_none()
                        && self
                            .options
                            .loop_count
                            .is_some_and(|count| self.plays >= count)))
            {
                self.finished = true;
                self.finished_at_loop = self.document.loop_index.is_some();
                return Ok(None);
            }
            self.budget()?;
            if self.index == self.document.commands.len() {
                if let Some(target) = self.document.loop_index {
                    if self
                        .options
                        .loop_count
                        .is_some_and(|count| self.plays >= count)
                        && let Some(marker) = &self.loop_marker
                        && !marker.reached()
                    {
                        marker.0.set(Some(self.sample));
                    }
                    self.plays = self.plays.saturating_add(1);
                    self.index = target;
                    continue;
                }
                unreachable!("natural termination is handled before the budget check");
            }
            if let Some(observer) = &mut self.observer {
                (observer.0)(ExecutionEvent {
                    sample: self.sample,
                    data: self.index,
                })?;
            }
            let command = self.document.commands[self.index].clone();
            self.index += 1;
            match command {
                S98Command::Wait(ticks) => {
                    let elapsed = self
                        .ticks
                        .checked_add(ticks)
                        .ok_or(S98PlaybackError::TimeOverflow)?;
                    if let Some(max) = self.options.max_ticks
                        && elapsed > max
                    {
                        return Err(S98PlaybackError::TickLimitExceeded { max_ticks: max }.into());
                    }
                    self.ticks = elapsed;
                    let numerator = if self.document.header.timer_numerator == 0 {
                        10
                    } else {
                        self.document.header.timer_numerator
                    };
                    let denominator = if self.document.header.timer_denominator == 0 {
                        1000
                    } else {
                        self.document.header.timer_denominator
                    };
                    let units = u128::from(ticks) * u128::from(numerator) * 44_100 + self.remainder;
                    self.pending_wait = u64::try_from(units / u128::from(denominator))
                        .map_err(|_| S98PlaybackError::TimeOverflow)?;
                    self.remainder = units % u128::from(denominator);
                    if self.pending_wait != 0 {
                        return self.emit_wait();
                    }
                }
                S98Command::Write {
                    device,
                    port,
                    register,
                    value,
                } => {
                    let Some(instance) = self.instances[usize::from(device)] else {
                        continue;
                    };
                    let port = u8::from(port == S98Port::Extended);
                    let kind = self.devices[usize::from(device)].kind;
                    let command = match kind {
                        S98DeviceType::Ym2149 | S98DeviceType::Ay8910 => {
                            VgmCommand::Ay8910Write(instance, chip::Ay8910Spec { register, value })
                        }
                        S98DeviceType::Ym2203 => {
                            VgmCommand::Ym2203Write(instance, chip::Ym2203Spec { register, value })
                        }
                        S98DeviceType::Ym2612 => VgmCommand::Ym2612Write(
                            instance,
                            chip::Ym2612Spec {
                                port,
                                register,
                                value,
                            },
                        ),
                        S98DeviceType::Ym2608 => VgmCommand::Ym2608Write(
                            instance,
                            chip::Ym2608Spec {
                                port,
                                register,
                                value,
                            },
                        ),
                        S98DeviceType::Ym2151 => {
                            VgmCommand::Ym2151Write(instance, chip::Ym2151Spec { register, value })
                        }
                        S98DeviceType::Ym2413 => {
                            VgmCommand::Ym2413Write(instance, chip::Ym2413Spec { register, value })
                        }
                        S98DeviceType::Ym3526 => {
                            VgmCommand::Ym3526Write(instance, chip::Ym3526Spec { register, value })
                        }
                        S98DeviceType::Ym3812 => {
                            VgmCommand::Ym3812Write(instance, chip::Ym3812Spec { register, value })
                        }
                        S98DeviceType::Ymf262 => VgmCommand::Ymf262Write(
                            instance,
                            chip::Ymf262Spec {
                                port,
                                register,
                                value,
                            },
                        ),
                        _ => return Err(invalid("unsupported S98 device")),
                    };
                    return Ok(Some(command));
                }
            }
        }
    }
}
