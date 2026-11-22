//! YMF271 OPX register and FM voice state tracking.

use super::channel::ChannelState;
use super::chip_state::ChipState;
use super::storage::{RegisterStorage, SparseStorage};
use crate::chip::event::{KeyState, StateEvent, ToneInfo};
use crate::chip::fnumber::{ChipTypeSpec, OpxSpec};

const GROUPS: [i8; 16] = [0, 1, 2, -1, 3, 4, 5, -1, 6, 7, 8, -1, 9, 10, 11, -1];
const CARRIERS_4: [u8; 16] = [8, 8, 8, 8, 8, 8, 12, 12, 9, 9, 14, 14, 14, 11, 13, 15];
const CARRIERS_2: [u8; 4] = [2, 2, 3, 3];
const CARRIERS_3: [u8; 8] = [2, 2, 2, 3, 6, 6, 7, 7];

/// Register storage indexed by `(port << 8) | register`.
pub type Ymf271Storage = SparseStorage<u16, u8>;

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    keyed: bool,
    fnum: u16,
    latch: u8,
    block: u8,
    wave: u8,
    algorithm: u8,
}

/// Tracks FM voices in the YMF271's 12 groups of four slots.
///
/// Select a VGM port with [`Self::set_port`] before each register write.
/// Channel IDs are physical key-on slot numbers (`12 * bank + group`), so
/// independent voices remain distinct in every synchronization mode.
/// PCM and the non-pitched linear waveform are not reported as notes. FM
/// frequency is the voice's fundamental before operator multiplier, detune,
/// feedback and LFO modulation.
#[derive(Debug, Clone)]
pub struct Ymf271State {
    channels: [ChannelState; 48],
    slots: [Slot; 48],
    sync: [u8; 12],
    master_clock_hz: f32,
    current_port: u8,
    registers: Ymf271Storage,
}

impl Ymf271State {
    /// Create a tracker with the chip's master clock in hertz.
    pub fn new(master_clock_hz: f32) -> Self {
        Self {
            channels: std::array::from_fn(|_| ChannelState::new()),
            slots: [Slot::default(); 48],
            sync: [0; 12],
            master_clock_hz,
            current_port: 0,
            registers: Ymf271Storage::default(),
        }
    }

    /// Return the state of a physical key-on slot, from 0 to 47.
    pub fn channel(&self, channel: u8) -> Option<&ChannelState> {
        self.channels.get(usize::from(channel))
    }

    /// Return mutable state for a physical key-on slot.
    pub fn channel_mut(&mut self, channel: u8) -> Option<&mut ChannelState> {
        self.channels.get_mut(usize::from(channel))
    }

    /// Select the VGM register port (FM 0–3, PCM 4, utility 6).
    pub fn set_port(&mut self, port: u8) {
        self.current_port = port;
    }

    /// Return the currently selected VGM port.
    pub fn current_port(&self) -> u8 {
        self.current_port
    }

    /// Legacy alias for [`Self::set_port`]. The argument is a port, not a slot.
    #[deprecated(note = "YMF271 writes select a port, not a slot; use set_port")]
    pub fn set_selected_slot(&mut self, port: u8) {
        self.set_port(port);
    }

    /// Legacy alias for [`Self::current_port`].
    #[deprecated(note = "YMF271 writes select a port, not a slot; use current_port")]
    pub fn selected_slot(&self) -> u8 {
        self.current_port()
    }

    fn voice(&self, group: usize, bank: usize) -> Option<([usize; 4], usize, u8)> {
        let slot = group + bank * 12;
        match (self.sync[group], bank) {
            (0, 0) => Some((
                [group, group + 12, group + 24, group + 36],
                4,
                CARRIERS_4[usize::from(self.slots[slot].algorithm)],
            )),
            (1, 0 | 1) => Some((
                [slot, slot + 24, 0, 0],
                2,
                CARRIERS_2[usize::from(self.slots[slot].algorithm & 3)],
            )),
            (2, 0) => Some((
                [group, group + 12, group + 24, 0],
                3,
                CARRIERS_3[usize::from(self.slots[slot].algorithm & 7)],
            )),
            (2, 3) | (3, _) => Some(([slot, 0, 0, 0], 1, 1)),
            _ => None,
        }
    }

    fn tone(&self, group: usize, bank: usize) -> Option<ToneInfo> {
        let (slots, length, carriers) = self.voice(group, bank)?;
        let head = self.slots[slots[0]];
        if !head.keyed || head.fnum == 0 {
            return None;
        }
        let pitched = slots[..length]
            .iter()
            .enumerate()
            .any(|(position, &index)| {
                carriers & (1 << position) != 0
                    && self.slots[index].keyed
                    && self.slots[index].wave < 6
            });
        if !pitched {
            return None;
        }
        let frequency =
            OpxSpec::fnum_block_to_freq(u32::from(head.fnum), head.block, self.master_clock_hz)
                .ok()?;
        Some(ToneInfo::new(head.fnum, head.block, Some(frequency)))
    }

    fn update_group(&mut self, group: usize, retrigger: Option<usize>) -> Option<Vec<StateEvent>> {
        let mut events = Vec::new();
        for bank in 0..4 {
            let index = group + bank * 12;
            let next = self.tone(group, bank);
            let channel = &mut self.channels[index];
            match (channel.tone, next) {
                (None, Some(tone)) => events.push(StateEvent::KeyOn {
                    channel: index as u8,
                    tone,
                }),
                (Some(_), None) => events.push(StateEvent::KeyOff {
                    channel: index as u8,
                }),
                (Some(_), Some(tone)) if retrigger == Some(index) => {
                    events.push(StateEvent::KeyOn {
                        channel: index as u8,
                        tone,
                    });
                }
                (Some(old), Some(tone)) if old != tone => {
                    events.push(StateEvent::ToneChange {
                        channel: index as u8,
                        tone,
                    });
                }
                _ => {}
            }
            channel.tone = next;
            channel.key_state = if next.is_some() {
                KeyState::On
            } else {
                KeyState::Off
            };
        }
        (!events.is_empty()).then_some(events)
    }
}

impl ChipState for Ymf271State {
    type Register = u8;
    type Value = u8;

    fn read_register(&self, register: u8) -> Option<u8> {
        self.registers
            .read(u16::from_be_bytes([self.current_port, register]))
    }

    fn on_register_write(&mut self, register: u8, value: u8) -> Option<Vec<StateEvent>> {
        self.registers
            .write(u16::from_be_bytes([self.current_port, register]), value);
        if self.current_port == 6 && register < 16 {
            let group = GROUPS[usize::from(register)];
            if group < 0 {
                return None;
            }
            let group = group as usize;
            self.sync[group] = value & 3;
            return self.update_group(group, None);
        }
        if self.current_port > 3 {
            return None;
        }
        let group = GROUPS[usize::from(register & 15)];
        if group < 0 {
            return None;
        }
        let group = group as usize;
        let bank = usize::from(self.current_port);
        let operation = register >> 4;
        let index = group + bank * 12;
        let mut targets = [index; 4];
        let mut count = 1;
        if matches!(operation, 0 | 9 | 10 | 12 | 13 | 14)
            && self.sync[group] != 3
            && let Some((slots, length, _)) = self.voice(group, bank)
        {
            targets = slots;
            count = length;
        }
        for &target in &targets[..count] {
            let slot = &mut self.slots[target];
            match operation {
                0 => slot.keyed = value & 1 != 0,
                9 => {
                    slot.fnum = (u16::from(slot.latch & 15) << 8) | u16::from(value);
                    slot.block = slot.latch >> 4;
                }
                10 => slot.latch = value,
                11 => slot.wave = value & 7,
                12 => slot.algorithm = value & 15,
                _ => {}
            }
        }
        let retrigger = (operation == 0 && value & 1 != 0).then_some(index);
        self.update_group(group, retrigger)
    }

    fn reset(&mut self) {
        self.channels = std::array::from_fn(|_| ChannelState::new());
        self.slots = [Slot::default(); 48];
        self.sync = [0; 12];
        self.current_port = 0;
        self.registers.clear();
    }

    fn channel_count(&self) -> usize {
        48
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(
        state: &mut Ymf271State,
        port: u8,
        register: u8,
        value: u8,
    ) -> Option<Vec<StateEvent>> {
        state.set_port(port);
        state.on_register_write(register, value)
    }

    #[test]
    fn sync_modes_broadcast_and_distinguish_voices() {
        for (mode, bank, expected) in [(0, 0, 0), (1, 1, 12), (2, 0, 0), (3, 3, 36)] {
            let mut state = Ymf271State::new(16_934_400.0);
            write(&mut state, 6, 0, mode);
            write(&mut state, bank, 0xa0, 0x45);
            write(&mut state, bank, 0x90, 0x1c);
            let events = write(&mut state, bank, 0, 1).unwrap();
            assert!(
                matches!(events.as_slice(), [StateEvent::KeyOn { channel, .. }] if *channel == expected)
            );
            let change = write(&mut state, bank, 0x90, 0x1d).unwrap();
            assert!(
                matches!(change.as_slice(), [StateEvent::ToneChange { channel, .. }] if *channel == expected)
            );
            let off = write(&mut state, bank, 0, 0).unwrap();
            assert!(
                matches!(off.as_slice(), [StateEvent::KeyOff { channel }] if *channel == expected)
            );
        }
    }

    #[test]
    fn latched_frequency_signed_block_and_register_ports() {
        let mut state = Ymf271State::new(16_934_400.0);
        write(&mut state, 0, 0xa0, 0xf5);
        write(&mut state, 0, 0x90, 0x1c);
        let events = write(&mut state, 0, 0, 1).unwrap();
        let StateEvent::KeyOn { tone, .. } = events[0] else {
            panic!("expected key on")
        };
        assert_eq!(tone.block, 15);
        assert!((tone.freq_hz.unwrap() - 13.75).abs() < 0.1);
        assert!(write(&mut state, 0, 0xa0, 0x45).is_none());
        let changes = write(&mut state, 0, 0x90, 0x1c).unwrap();
        assert!(matches!(changes[0], StateEvent::ToneChange { .. }));
        write(&mut state, 1, 0xa0, 0x12);
        assert_eq!(state.read_register(0xa0), Some(0x12));
        state.set_port(0);
        assert_eq!(state.read_register(0xa0), Some(0x45));
        state.reset();
        assert_eq!(state.read_register(0xa0), None);
        assert_eq!(state.channel(0).unwrap().key_state, KeyState::Off);
    }

    #[test]
    fn pcm_carriers_and_sync_changes_never_leave_stuck_notes() {
        let mut state = Ymf271State::new(16_934_400.0);
        write(&mut state, 6, 0, 3);
        write(&mut state, 0, 0xb0, 7);
        write(&mut state, 0, 0xa0, 0x45);
        write(&mut state, 0, 0x90, 0x1c);
        assert!(write(&mut state, 0, 0, 1).is_none());
        let on = write(&mut state, 0, 0xb0, 0).unwrap();
        assert!(matches!(on[0], StateEvent::KeyOn { channel: 0, .. }));
        let off = write(&mut state, 0, 0xb0, 6).unwrap();
        assert!(matches!(off[0], StateEvent::KeyOff { channel: 0 }));
        write(&mut state, 0, 0xb0, 0);
        let off = write(&mut state, 6, 0, 0).unwrap();
        assert!(matches!(off[0], StateEvent::KeyOff { channel: 0 }));
    }

    #[test]
    fn two_operator_voices_have_independent_ports_and_keys() {
        let mut state = Ymf271State::new(16_934_400.0);
        write(&mut state, 6, 0, 1);
        for (port, low) in [(0, 0x1c), (1, 0x70)] {
            write(&mut state, port, 0xa0, 0x45);
            write(&mut state, port, 0x90, low);
            let events = write(&mut state, port, 0, 1).unwrap();
            assert!(matches!(events[0], StateEvent::KeyOn { channel, .. } if channel == port * 12));
        }
        assert!(write(&mut state, 0, 0x93, 1).is_none());
        let off = write(&mut state, 0, 0, 0).unwrap();
        assert!(matches!(
            off.as_slice(),
            [StateEvent::KeyOff { channel: 0 }]
        ));
        assert_eq!(state.channel(12).unwrap().key_state, KeyState::On);
        let repeated = write(&mut state, 1, 0, 1).unwrap();
        assert!(matches!(
            repeated.as_slice(),
            [StateEvent::KeyOn { channel: 12, .. }]
        ));
    }
}
