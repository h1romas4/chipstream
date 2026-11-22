//! Preparation of per-instance chip settings for sequencer initialization.
//!
//! Header clocks, flags, and optional extra-header overrides are translated
//! into voice configurations and backend settings. Clock observations are
//! passed to the timing module to determine resampler bounds.

use soundlog::chip::Chip;
use soundlog::vgm::command::Instance;
use soundlog::vgm::header::{Ay8910ChipType, Ay8910Flags, ChipId, VgmExtraHeader, VgmHeader};

use super::timing::Timing;
use crate::sequencer::{
    Ay8910Settings, Ay8910Variant, AyOutputSettings, C140Variant, ChipBackendSettings,
    Okim6258Divider, Okim6258Settings, Sn76489Settings,
};

/// Effective settings for one chip instance.
pub(super) struct VoiceConfig {
    /// Chip type selected by the source.
    pub(super) chip: Chip,
    /// Primary or secondary instance.
    pub(super) instance: Instance,
    /// Effective input clock, including extra-header overrides.
    pub(super) clock: u32,
    /// Whether the source declares a dual-chip configuration.
    pub(super) dual: bool,
    /// Chip-specific header settings.
    pub(super) settings: ChipBackendSettings,
    /// Whether the built-in OKIM6258 backend filters DC.
    pub(super) okim6258_dc_blocker: bool,
}

/// Header-derived voices and timing information for sequencer construction.
pub(in crate::sequencer) struct ChipConfiguration<'a> {
    /// Primary VGM header.
    pub(super) header: &'a VgmHeader,
    /// Optional per-instance clock and volume overrides.
    pub(super) extra: Option<&'a VgmExtraHeader>,
    /// Dynamic-clock observations and bounds.
    pub(super) timing: Timing,
    /// Playable chip instances declared by the header.
    pub(super) voices: Vec<VoiceConfig>,
}

impl<'a> ChipConfiguration<'a> {
    /// Build per-instance settings from the source headers.
    pub(in crate::sequencer) fn from_header(
        header: &'a VgmHeader,
        extra: Option<&'a VgmExtraHeader>,
        okim6258_dc_blocker: bool,
    ) -> Self {
        let clocks = Self::supported_clocks(header);
        let mut voices = Vec::new();
        for (chip, raw) in &clocks {
            if raw & 0x3fff_ffff == 0 {
                continue;
            }
            let dual = raw & 0x4000_0000 != 0;
            for instance in [Instance::Primary, Instance::Secondary] {
                if instance == Instance::Secondary && !dual {
                    continue;
                }
                let clock = extra
                    .and_then(|extra| {
                        extra.chip_clocks.iter().find(|entry| {
                            entry.chip_id == chip_id(chip) && entry.instance == instance
                        })
                    })
                    .map_or(raw & 0x3fff_ffff, |entry| entry.clock & 0x3fff_ffff);
                voices.push(VoiceConfig {
                    chip: chip.clone(),
                    instance,
                    clock,
                    dual,
                    settings: backend_settings(chip, header),
                    okim6258_dc_blocker,
                });
            }
        }
        Self {
            header,
            extra,
            timing: Timing::new(&clocks),
            voices,
        }
    }

    /// Record observed non-default PSG stereo masks by instance.
    pub(super) fn set_psg_stereo(&mut self, stereo: [bool; 2]) {
        for voice in &mut self.voices {
            if let ChipBackendSettings::Sn76489(settings) = &mut voice.settings {
                settings.stereo_used = stereo[voice.instance as usize];
            }
        }
    }

    /// Include a complete clock value in the instance's timing bounds.
    pub(in crate::sequencer) fn observe_clock(
        &mut self,
        chip: &Chip,
        instance: Instance,
        clock: u32,
    ) {
        self.timing.observe_clock(chip, instance, clock);
    }

    /// Extract supported clocks, including legacy header aliases.
    fn supported_clocks(header: &VgmHeader) -> [(Chip, u32); 36] {
        let misc = header.misc();
        [
            (Chip::Sn76489, header.sn76489_clock),
            (Chip::SegaPcm, header.sega_pcm_clock),
            (Chip::Pwm, header.pwm_clock),
            (Chip::Rf5c68, header.rf5c68_clock),
            (Chip::Rf5c164, header.rf5c164_clock),
            (Chip::Saa1099, header.saa1099_clock),
            (Chip::K051649, header.k051649_clock),
            (Chip::Huc6280, header.huc6280_clock),
            (Chip::Pokey, header.pokey_clock),
            (Chip::Ga20, header.ga20_clock),
            (Chip::GbDmg, header.gb_dmg_clock),
            (Chip::C352, header.c352_clock),
            (Chip::Es5503, header.es5503_clock),
            (Chip::K053260, header.k053260_clock),
            (Chip::K054539, header.k054539_clock),
            (Chip::X1010, header.x1_010_clock),
            (Chip::Ymz280b, header.ymz280b_clock),
            (Chip::MultiPcm, header.multipcm_clock),
            (Chip::Mikey, header.mikey_clock),
            (Chip::NesApu, header.nes_apu_clock),
            (Chip::Upd7759, header.upd7759_clock),
            (Chip::Ay8910, header.ay8910_clock),
            (Chip::C140, header.c140_clock),
            (Chip::Ym2413, header.ym2413_clock),
            (
                Chip::Ym2612,
                misc.use_ym2413_clock_for_ym2612
                    .unwrap_or(header.ym2612_clock),
            ),
            (
                Chip::Ym2151,
                misc.use_ym2413_clock_for_ym2151
                    .unwrap_or(header.ym2151_clock),
            ),
            (Chip::Ym2203, header.ym2203_clock),
            (Chip::Ym2608, header.ym2608_clock),
            (Chip::Ym2610b, header.ym2610b_clock),
            (Chip::Ym3812, header.ym3812_clock),
            (Chip::Ym3526, header.ym3526_clock),
            (Chip::Y8950, header.y8950_clock),
            (Chip::Ymf262, header.ymf262_clock),
            (Chip::Ymf278b, header.ymf278b_clock),
            (Chip::Okim6258, header.okim6258_clock),
            (Chip::Okim6295, header.okim6295_clock),
        ]
    }
}

impl From<&VgmHeader> for Ay8910Settings {
    fn from(header: &VgmHeader) -> Self {
        let variant = match header.ay_chip_type {
            Ay8910ChipType::Ay8910 => Ay8910Variant::Ay8910,
            Ay8910ChipType::Ay8912 => Ay8910Variant::Ay8912,
            Ay8910ChipType::Ay8913 => Ay8910Variant::Ay8913,
            Ay8910ChipType::Ay8930 => Ay8910Variant::Ay8930,
            Ay8910ChipType::Ay8914 => Ay8910Variant::Ay8914,
            Ay8910ChipType::Ym2149 => Ay8910Variant::Ym2149,
            Ay8910ChipType::Ym3439 => Ay8910Variant::Ym3439,
            Ay8910ChipType::Ymz284 => Ay8910Variant::Ymz284,
            Ay8910ChipType::Ymz294 => Ay8910Variant::Ymz294,
            Ay8910ChipType::Unknown(value) => Ay8910Variant::Unknown(value),
        };
        Self {
            variant,
            output: header.ay8910_flags.into(),
        }
    }
}

impl From<Ay8910Flags> for AyOutputSettings {
    fn from(flags: Ay8910Flags) -> Self {
        Self {
            legacy_output: flags.legacy_output,
            single_output: flags.single_output,
            discrete_output: flags.discrete_output,
            raw_output: flags.raw_output,
            ym_pin26_low: flags.ym_pin26_low,
            reserved_flags: flags.reserved,
        }
    }
}

/// Translate header flags into chip-specific factory settings.
fn backend_settings(chip: &Chip, header: &VgmHeader) -> ChipBackendSettings {
    match chip {
        Chip::NesApu if header.nes_apu_clock & 0x8000_0000 != 0 => ChipBackendSettings::NesApuFds,
        Chip::Ay8910 => ChipBackendSettings::Ay8910(Ay8910Settings::from(header)),
        Chip::Sn76489 => {
            let flags = header.sn76489_flags;
            let feedback = u16::from(header.sn76489_feedback);
            let width = u8::from(header.sn76489_shift_register_width);
            ChipBackendSettings::Sn76489(Sn76489Settings {
                shift_width: if width == 0 { 16 } else { width },
                feedback: if feedback == 0 { 0x0009 } else { feedback },
                negate: flags.output_negate,
                stereo: flags.gamegear_stereo,
                stereo_used: false,
                divider_8: flags.clock_divider_8,
                frequency_zero_is_1024: flags.frequency_0_is_400,
                xnor: flags.xnor_noise_mode,
                t6w28: header.misc().t6w28_detected == Some(true),
                reserved_flags: flags.reserved,
            })
        }
        Chip::Okim6258 => {
            let flags = header.okim6258_flags;
            ChipBackendSettings::Okim6258(Okim6258Settings {
                divider: match flags.clock_divider {
                    0 => Okim6258Divider::Div1024,
                    1 => Okim6258Divider::Div768,
                    _ => Okim6258Divider::Div512,
                },
                adpcm_3bit: flags.adpcm_3bit_select,
                output_12bit: flags.output_12bit,
                reserved_flags: flags.reserved,
            })
        }
        Chip::Okim6295 => ChipBackendSettings::Okim6295 {
            pin7_high: header.okim6295_clock & 0x8000_0000 != 0,
        },
        Chip::C140 => ChipBackendSettings::C140(match u8::from(header.c140_chip_type) {
            0 => C140Variant::System2,
            1 => C140Variant::System21,
            2 => C140Variant::Asic219,
            value => C140Variant::Unknown(value),
        }),
        Chip::C352 => ChipBackendSettings::C352 {
            divider: if header.c352_clock_divider == 0 {
                1
            } else {
                u32::from(header.c352_clock_divider) * 4
            },
            disable_rear: header.c352_clock & 0x8000_0000 != 0,
        },
        Chip::Es5503 => ChipBackendSettings::Es5503 {
            output_channels: header.es5503_output_channels,
        },
        Chip::K054539 => ChipBackendSettings::K054539 {
            reverse_stereo: header.k054539_flags.reverse_stereo,
            disable_reverb: header.k054539_flags.disable_reverb,
            update_at_keyon: header.k054539_flags.update_at_keyon,
            reserved_flags: header.k054539_flags.reserved,
        },
        Chip::Upd7759 => ChipBackendSettings::Upd7759 {
            slave: header.upd7759_clock & 0x8000_0000 != 0,
        },
        Chip::SegaPcm => ChipBackendSettings::SegaPcm {
            bank_shift: header.spcm_interface as u8,
            bank_mask: (header.spcm_interface >> 16) as u8,
        },
        Chip::Ym2203 => ChipBackendSettings::Ym2203 {
            ssg: header.ym2203_ay8910_flags.into(),
        },
        Chip::Ym2608 => ChipBackendSettings::Ym2608 {
            ssg: header.ym2608_ay8910_flags.into(),
        },
        Chip::Ym2610b => ChipBackendSettings::Ym2610 {
            is_b: header.misc().ym2610b_detected == Some(true),
        },
        _ => ChipBackendSettings::Standard,
    }
}

/// Map a supported chip to its VGM extra-header identifier.
pub(super) fn chip_id(chip: &Chip) -> ChipId {
    match chip {
        Chip::Sn76489 => ChipId::Sn76489,
        Chip::SegaPcm => ChipId::SegaPcm,
        Chip::Pwm => ChipId::Pwm,
        Chip::Rf5c68 => ChipId::Rf5c68,
        Chip::Rf5c164 => ChipId::Rf5c164,
        Chip::Saa1099 => ChipId::Saa1099,
        Chip::K051649 => ChipId::K051649,
        Chip::Huc6280 => ChipId::Huc6280,
        Chip::Pokey => ChipId::Pokey,
        Chip::Ga20 => ChipId::Ga20,
        Chip::GbDmg => ChipId::GbDmg,
        Chip::C352 => ChipId::C352,
        Chip::Es5503 => ChipId::Es5503,
        Chip::K053260 => ChipId::K053260,
        Chip::K054539 => ChipId::K054539,
        Chip::X1010 => ChipId::X1010,
        Chip::Ymz280b => ChipId::Ymz280b,
        Chip::MultiPcm => ChipId::MultiPcm,
        Chip::Mikey => ChipId::Mikey,
        Chip::NesApu => ChipId::NesApu,
        Chip::Upd7759 => ChipId::Upd7759,
        Chip::Ay8910 => ChipId::Ay8910,
        Chip::C140 => ChipId::C140,
        Chip::Ym2413 => ChipId::Ym2413,
        Chip::Ym2612 => ChipId::Ym2612,
        Chip::Ym2151 => ChipId::Ym2151,
        Chip::Ym2203 => ChipId::Ym2203,
        Chip::Ym2608 => ChipId::Ym2608,
        Chip::Ym2610b => ChipId::Ym2610,
        Chip::Ym3812 => ChipId::Ym3812,
        Chip::Ym3526 => ChipId::Ym3526,
        Chip::Y8950 => ChipId::Y8950,
        Chip::Ymf262 => ChipId::Ymf262,
        Chip::Ymf278b => ChipId::Ymf278b,
        Chip::Okim6258 => ChipId::Okim6258,
        Chip::Okim6295 => ChipId::Okim6295,
        _ => unreachable!("only supported chips have sequencer voices"),
    }
}
