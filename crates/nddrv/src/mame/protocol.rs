//! Decoder for the bundled MAME plugin socket messages.

use anyhow::{Result, bail};

use crate::routing::{ChipInstance, HardwareChip};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plugin {
    NanoDrive6,
    NanoDrive7,
    NanoDrive8,
}

impl Plugin {
    pub fn all_systems() -> &'static [&'static str] {
        &[
            "Sega System C2",
            "Mega Drive",
            "YM2203 / YM2413 / OPL / AY systems",
            "YM2151-based systems",
        ]
    }

    pub fn supported_systems(self) -> &'static [&'static str] {
        match self {
            Self::NanoDrive6 => &["Sega System C2", "Mega Drive"],
            Self::NanoDrive7 => &["YM2203 / YM2413 / OPL / AY systems"],
            Self::NanoDrive8 => &["YM2151-based systems"],
        }
    }

    pub fn output_chips(self) -> &'static [(HardwareChip, ChipInstance)] {
        match self {
            Self::NanoDrive6 => &[
                (HardwareChip::Ym2612, ChipInstance::Primary),
                (HardwareChip::Sn76489, ChipInstance::Primary),
            ],
            Self::NanoDrive7 => &[
                (HardwareChip::Ym2203, ChipInstance::Primary),
                (HardwareChip::Ym2203, ChipInstance::Secondary),
                (HardwareChip::Ay8910, ChipInstance::Primary),
                (HardwareChip::Ay8910, ChipInstance::Secondary),
                (HardwareChip::Ym2413, ChipInstance::Primary),
                (HardwareChip::Ym3526, ChipInstance::Primary),
                (HardwareChip::Ym3812, ChipInstance::Primary),
                (HardwareChip::Ymf262, ChipInstance::Primary),
            ],
            Self::NanoDrive8 => &[(HardwareChip::Ym2151, ChipInstance::Primary)],
        }
    }

    pub fn for_device(
        model: &str,
        capabilities: &[ndsif::CapabilityEntry],
    ) -> Result<Self, &'static str> {
        let supports = |write_id, page| {
            capabilities.iter().any(|entry| {
                entry.write_id == write_id
                    && entry.instance == 0
                    && entry.page == page
                    && entry.features & 1 != 0
            })
        };
        let nd6 = supports(0x52, 0) && supports(0x53, 1) && supports(0x50, 0);
        let nd8 = supports(0x54, 0);
        if model == "NanoDrive 7"
            && [0x51, 0x55, 0x5a, 0x5b, 0x5e, 0xa0]
                .into_iter()
                .any(|id| supports(id, 0))
        {
            return Ok(Self::NanoDrive7);
        }
        match (nd6, nd8) {
            (true, false) => Ok(Self::NanoDrive6),
            (false, true) => Ok(Self::NanoDrive8),
            (true, true) => match model {
                "NanoDrive 6" | "NanoDrive 6.1" => Ok(Self::NanoDrive6),
                "NanoDrive 8" => Ok(Self::NanoDrive8),
                _ => Err("Cannot determine the MAME route for this device."),
            },
            (false, false) => Err("This device does not support a MAME route."),
        }
    }

    pub fn launch_example(self) -> &'static str {
        match self {
            Self::NanoDrive6 => "mame -plugin nanodrive6 megadrij [softname]",
            Self::NanoDrive7 => "mame -plugin nanodrive7 [machine]",
            Self::NanoDrive8 => "mame -plugin nanodrive8 [machine]",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nd7Route {
    pub write_id: u8,
    pub instance: u8,
    pub tag: String,
    pub divide_clock: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Reset,
    Quit,
    Pause,
    Resume,
    Clock(u32),
    Ym2612 {
        tick: u64,
        page: u8,
        address: u8,
        value: u8,
    },
    Psg {
        tick: u64,
        value: u8,
    },
    Timestamp(u64),
    FastForward {
        enabled: bool,
        skipped: u32,
    },
    Machine {
        pid: u32,
        name: String,
        tag: String,
    },
    Ym2151 {
        address: u8,
        value: u8,
    },
    Nd7Machine {
        pid: u32,
        name: String,
        routes: Vec<Nd7Route>,
    },
    Nd7Write {
        write_id: u8,
        instance: u8,
        address: u8,
        value: u8,
    },
}

pub struct Decoder {
    plugin: Plugin,
    pending: Vec<u8>,
}

impl Decoder {
    pub fn new(plugin: Plugin) -> Self {
        Self {
            plugin,
            pending: Vec::new(),
        }
    }

    pub fn push(&mut self, input: &[u8]) -> Result<Vec<Event>> {
        self.pending.extend_from_slice(input);
        let mut events = Vec::new();
        let mut used = 0;
        while let Some((event, length)) = decode_one(self.plugin, &self.pending[used..])? {
            events.push(event);
            used += length;
        }
        self.pending.drain(..used);
        Ok(events)
    }

    pub fn finish(&self) -> Result<()> {
        if !self.pending.is_empty() {
            bail!("MAME disconnected in the middle of a message");
        }
        Ok(())
    }
}

fn decode_one(plugin: Plugin, bytes: &[u8]) -> Result<Option<(Event, usize)>> {
    let Some(&opcode) = bytes.first() else {
        return Ok(None);
    };
    let length = match (plugin, opcode) {
        (_, b'R' | b'Q') => 1,
        (Plugin::NanoDrive6, b'P' | b'U' | b'T') => 9,
        (Plugin::NanoDrive6, b'Y') => 12,
        (Plugin::NanoDrive6, b'G') => 10,
        (Plugin::NanoDrive6, b'F') => 6,
        (Plugin::NanoDrive6, b'C') => 5,
        (Plugin::NanoDrive8, b'P' | b'U') => 1,
        (Plugin::NanoDrive7, b'P' | b'U') => 1,
        (Plugin::NanoDrive7, b'V') => 5,
        (Plugin::NanoDrive7, b'J') if bytes.len() < 3 => return Ok(None),
        (Plugin::NanoDrive7, b'J') => {
            let size = usize::from(u16::from_le_bytes([bytes[1], bytes[2]])) + 3;
            if !(9..=2048).contains(&size) {
                bail!("Invalid ND7 initialization length");
            }
            size
        }
        (Plugin::NanoDrive8, b'W') => 3,
        (Plugin::NanoDrive8, b'I') if bytes.len() < 7 => return Ok(None),
        (Plugin::NanoDrive8, b'I') => 7 + usize::from(bytes[5]) + usize::from(bytes[6]),
        _ => bail!("Unknown or incompatible MAME message 0x{opcode:02x}"),
    };
    if bytes.len() < length {
        return Ok(None);
    }
    let read_u32 = |start| {
        u32::from_le_bytes(
            bytes[start..start + 4]
                .try_into()
                .expect("checked message length"),
        )
    };
    let read_u64 = |start| {
        u64::from_le_bytes(
            bytes[start..start + 8]
                .try_into()
                .expect("checked message length"),
        )
    };
    let event = match opcode {
        b'R' => Event::Reset,
        b'Q' => Event::Quit,
        b'P' => Event::Pause,
        b'U' => Event::Resume,
        b'C' => Event::Clock(read_u32(1)),
        b'T' => Event::Timestamp(read_u64(1)),
        b'Y' => {
            if bytes[9] > 1 {
                bail!("Invalid YM2612 page {}", bytes[9]);
            }
            Event::Ym2612 {
                tick: read_u64(1),
                page: bytes[9],
                address: bytes[10],
                value: bytes[11],
            }
        }
        b'G' => Event::Psg {
            tick: read_u64(1),
            value: bytes[9],
        },
        b'F' => {
            if bytes[1] > 1 {
                bail!("Invalid MAME fast-forward state {}", bytes[1]);
            }
            Event::FastForward {
                enabled: bytes[1] == 1,
                skipped: read_u32(2),
            }
        }
        b'W' => Event::Ym2151 {
            address: bytes[1],
            value: bytes[2],
        },
        b'V' => Event::Nd7Write {
            write_id: bytes[1],
            instance: bytes[2],
            address: bytes[3],
            value: bytes[4],
        },
        b'J' => {
            let name_end = 8 + usize::from(bytes[7]);
            if name_end >= length {
                bail!("Invalid ND7 machine name or missing chip routes");
            }
            let name = std::str::from_utf8(&bytes[8..name_end])?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            {
                bail!("Invalid MAME machine name");
            }
            let mut routes = Vec::<Nd7Route>::new();
            let mut offset = name_end;
            while offset < length {
                if length - offset < 3 {
                    bail!("Incomplete ND7 chip route");
                }
                let id = bytes[offset];
                let instance = bytes[offset + 1] & 0x7f;
                let divide_clock = bytes[offset + 1] & 0x80 != 0;
                let end = offset + 3 + usize::from(bytes[offset + 2]);
                if end > length
                    || !matches!(id, 0x51 | 0x55 | 0x5a | 0x5b | 0x5e | 0xa0)
                    || instance > u8::from(matches!(id, 0x55 | 0xa0))
                    || (divide_clock && id != 0xa0)
                    || routes
                        .iter()
                        .any(|route| route.write_id == id && route.instance == instance)
                {
                    bail!("Invalid ND7 chip route");
                }
                let tag = std::str::from_utf8(&bytes[offset + 3..end])?;
                if tag.len() < 2 || !tag.starts_with(':') || !tag.is_ascii() {
                    bail!("Invalid ND7 chip tag");
                }
                routes.push(Nd7Route {
                    write_id: id,
                    instance,
                    tag: tag.to_owned(),
                    divide_clock,
                });
                offset = end;
            }
            Event::Nd7Machine {
                pid: read_u32(3),
                name: name.to_owned(),
                routes,
            }
        }
        b'I' => {
            let game_end = 7 + usize::from(bytes[5]);
            let name = std::str::from_utf8(&bytes[7..game_end])?;
            let tag = std::str::from_utf8(&bytes[game_end..length])?;
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                || !tag.starts_with(':')
                || !tag.is_ascii()
            {
                bail!("Invalid MAME machine name or YM2151 tag");
            }
            Event::Machine {
                pid: read_u32(1),
                name: name.to_owned(),
                tag: tag.to_owned(),
            }
        }
        _ => unreachable!("opcode was validated"),
    };
    Ok(Some((event, length)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_all_mame_systems_independent_of_device_selection() {
        assert_eq!(
            Plugin::all_systems(),
            &[
                "Sega System C2",
                "Mega Drive",
                "YM2203 / YM2413 / OPL / AY systems",
                "YM2151-based systems"
            ]
        );
    }

    #[test]
    fn mame_plugins_route_to_their_supported_sound_chips() {
        assert_eq!(
            Plugin::NanoDrive6.output_chips(),
            &[
                (HardwareChip::Ym2612, ChipInstance::Primary),
                (HardwareChip::Sn76489, ChipInstance::Primary),
            ]
        );
        assert_eq!(
            Plugin::NanoDrive8.output_chips(),
            &[(HardwareChip::Ym2151, ChipInstance::Primary)]
        );
    }

    #[test]
    fn selects_route_from_connected_device_capabilities() {
        let entry = |write_id, page| ndsif::CapabilityEntry {
            write_id,
            instance: 0,
            page,
            features: 1,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let nd6 = [entry(0x52, 0), entry(0x53, 1), entry(0x50, 0)];
        let nd8 = [entry(0x54, 0)];
        assert_eq!(
            Plugin::for_device("NanoDrive 7", &[entry(0x55, 0)]),
            Ok(Plugin::NanoDrive7)
        );
        assert!(Plugin::for_device("NanoDrive 7", &[]).is_err());
        assert_eq!(
            Plugin::for_device("NanoDrive 6.1", &nd6),
            Ok(Plugin::NanoDrive6)
        );
        assert_eq!(
            Plugin::for_device("NanoDrive 8", &nd8),
            Ok(Plugin::NanoDrive8)
        );
        assert_eq!(Plugin::for_device("Other", &nd8), Ok(Plugin::NanoDrive8));
        assert!(Plugin::for_device("NanoDrive 6", &nd6[..2]).is_err());
        let mut no_write = nd8;
        no_write[0].features = 0;
        assert!(Plugin::for_device("NanoDrive 8", &no_write).is_err());
        let mut secondary = nd8;
        secondary[0].instance = 1;
        assert!(Plugin::for_device("NanoDrive 8", &secondary).is_err());
        let mut wrong_page = nd6;
        wrong_page[1].page = 0;
        assert!(Plugin::for_device("NanoDrive 6", &wrong_page).is_err());
        let both = [nd6[0], nd6[1], nd6[2], nd8[0]];
        assert_eq!(
            Plugin::for_device("NanoDrive 6", &both),
            Ok(Plugin::NanoDrive6)
        );
        assert_eq!(
            Plugin::for_device("NanoDrive 8", &both),
            Ok(Plugin::NanoDrive8)
        );
        assert!(Plugin::for_device("Other", &both).is_err());
        assert!(Plugin::for_device("NanoDrive 8", &[]).is_err());
    }

    #[test]
    fn launch_examples_match_the_plugin_route() {
        assert_eq!(
            Plugin::NanoDrive6.supported_systems(),
            ["Sega System C2", "Mega Drive"]
        );
        assert_eq!(
            Plugin::NanoDrive8.supported_systems(),
            ["YM2151-based systems"]
        );
        assert_eq!(
            Plugin::NanoDrive6.launch_example(),
            "mame -plugin nanodrive6 megadrij [softname]"
        );
        assert_eq!(
            Plugin::NanoDrive8.launch_example(),
            "mame -plugin nanodrive8 [machine]"
        );
    }

    #[test]
    fn decodes_fragmented_and_concatenated_nd6_messages() {
        let mut decoder = Decoder::new(Plugin::NanoDrive6);
        assert_eq!(decoder.push(b"RY\x02").unwrap(), vec![Event::Reset]);
        assert_eq!(
            decoder
                .push(&[0, 0, 0, 0, 0, 0, 0, 0, 0x2a, 0x80, b'Q'])
                .unwrap(),
            vec![
                Event::Ym2612 {
                    tick: 2,
                    page: 0,
                    address: 0x2a,
                    value: 0x80
                },
                Event::Quit,
            ]
        );
        decoder.finish().unwrap();
    }

    #[test]
    fn decodes_nd8_identity_and_writes() {
        let mut decoder = Decoder::new(Plugin::NanoDrive8);
        let events = decoder
            .push(b"I\x2a\0\0\0\x03\x03abc:ymRW\x20\x7fPQ")
            .unwrap();
        assert_eq!(
            events,
            vec![
                Event::Machine {
                    pid: 42,
                    name: "abc".into(),
                    tag: ":ym".into()
                },
                Event::Reset,
                Event::Ym2151 {
                    address: 0x20,
                    value: 0x7f
                },
                Event::Pause,
                Event::Quit,
            ]
        );
        decoder.finish().unwrap();
    }

    #[test]
    fn decodes_fragmented_nd7_identity_and_register_writes() {
        let payload = b"\x2a\0\0\0\x03abc\x55\0\x03:ym\xa0\x81\x04:psg";
        let mut bytes = vec![b'J'];
        bytes.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(b"RV\x55\0\x27\x12V\xa0\x01\x07\x38Q");
        let mut decoder = Decoder::new(Plugin::NanoDrive7);
        let mut events = Vec::new();
        for chunk in bytes.chunks(2) {
            events.extend(decoder.push(chunk).unwrap());
        }
        assert_eq!(
            events,
            vec![
                Event::Nd7Machine {
                    pid: 42,
                    name: "abc".into(),
                    routes: vec![
                        Nd7Route {
                            write_id: 0x55,
                            instance: 0,
                            tag: ":ym".into(),
                            divide_clock: false
                        },
                        Nd7Route {
                            write_id: 0xa0,
                            instance: 1,
                            tag: ":psg".into(),
                            divide_clock: true
                        },
                    ]
                },
                Event::Reset,
                Event::Nd7Write {
                    write_id: 0x55,
                    instance: 0,
                    address: 0x27,
                    value: 0x12
                },
                Event::Nd7Write {
                    write_id: 0xa0,
                    instance: 1,
                    address: 7,
                    value: 0x38
                },
                Event::Quit,
            ]
        );
        decoder.finish().unwrap();
        for payload in [
            &b"\x2a\0\0\0\x03abc\x55\x80\x03:ym"[..],
            &b"\x2a\0\0\0\x03abc\x55\0\x03:ym\x55\0\x03:ym"[..],
            &b"\x2a\0\0\0\x03abc\x55\0\x05:ym"[..],
        ] {
            let mut message = vec![b'J'];
            message.extend_from_slice(&(payload.len() as u16).to_le_bytes());
            message.extend_from_slice(payload);
            assert!(Decoder::new(Plugin::NanoDrive7).push(&message).is_err());
        }
    }

    #[test]
    fn rejects_wrong_plugin_invalid_and_incomplete_messages() {
        assert!(Decoder::new(Plugin::NanoDrive8).push(b"Y").is_err());
        assert!(
            Decoder::new(Plugin::NanoDrive6)
                .push(b"F\x02\0\0\0\0")
                .is_err()
        );
        let mut decoder = Decoder::new(Plugin::NanoDrive8);
        decoder.push(b"W\x20").unwrap();
        assert!(decoder.finish().is_err());
    }
}
