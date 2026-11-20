//! Preparation of per-instance chip settings for sequencer initialization.

use soundlog::chip::Chip;
use soundlog::vgm::command::Instance;
use soundlog::vgm::header::{ChipId, VgmExtraHeader, VgmHeader};

use super::timing::Timing;

pub(super) struct VoiceConfig {
    pub(super) chip: Chip,
    pub(super) instance: Instance,
    pub(super) clock: u32,
    pub(super) dual: bool,
    pub(super) sn76489_stereo_used: bool,
    pub(super) okim6258_dc_blocker: bool,
}

pub(in crate::sequencer) struct ChipConfiguration<'a> {
    pub(super) header: &'a VgmHeader,
    pub(super) extra: Option<&'a VgmExtraHeader>,
    pub(super) timing: Timing,
    pub(super) voices: Vec<VoiceConfig>,
}

impl<'a> ChipConfiguration<'a> {
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
                    sn76489_stereo_used: false,
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

    pub(super) fn set_psg_stereo(&mut self, stereo: [bool; 2]) {
        for voice in &mut self.voices {
            if voice.chip == Chip::Sn76489 {
                voice.sn76489_stereo_used = stereo[voice.instance as usize];
            }
        }
    }

    pub(in crate::sequencer) fn observe_clock(
        &mut self,
        chip: &Chip,
        instance: Instance,
        clock: u32,
    ) {
        self.timing.observe_clock(chip, instance, clock);
    }

    fn supported_clocks(header: &VgmHeader) -> [(Chip, u32); 18] {
        let misc = header.misc();
        [
            (Chip::Sn76489, header.sn76489_clock),
            (Chip::SegaPcm, header.sega_pcm_clock),
            (Chip::Pwm, header.pwm_clock),
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

pub(super) fn chip_id(chip: &Chip) -> ChipId {
    match chip {
        Chip::Sn76489 => ChipId::Sn76489,
        Chip::SegaPcm => ChipId::SegaPcm,
        Chip::Pwm => ChipId::Pwm,
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
