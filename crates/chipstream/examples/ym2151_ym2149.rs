//! Render a YM2151 + AY8910 VGM/VGZ using YM2151 and YM2149 into a WAV file.
//!
//! Run with `cargo run -p chipstream --example ym2151_ym2149 -- song.vgz output.wav`.
//! AY8910-to-YM2149 is an approximation; unsupported AY variants and output
//! flags are rejected. The output file must not already exist.

use std::error::Error;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Cursor, Read, Seek, Write};

use chipstream::chip::{ChipError, YmfmChip};
use chipstream::sequencer::{Chip, ChipFactoryRegistry, VgmSequencer};
use flate2::read::GzDecoder;
use soundlog::vgm::header::{Ay8910ChipType, VgmHeader};

const SAMPLE_RATE: u32 = 44_100;
const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_WAV_DATA_BYTES: u32 = u32::MAX - 36;

fn read_bounded(reader: impl Read) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(MAX_INPUT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INPUT_BYTES {
        return Err(io::Error::other("VGM input exceeds 64 MiB"));
    }
    Ok(bytes)
}

fn decode_vgm(bytes: Vec<u8>) -> io::Result<Vec<u8>> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        read_bounded(GzDecoder::new(Cursor::new(bytes)))
    } else {
        Ok(bytes)
    }
}

fn ym2149_clock_divider(header: &VgmHeader) -> Result<bool, ChipError> {
    let ym = matches!(
        header.ay_chip_type,
        Ay8910ChipType::Ym2149
            | Ay8910ChipType::Ym3439
            | Ay8910ChipType::Ymz284
            | Ay8910ChipType::Ymz294
    );
    if (!ym
        && !matches!(
            header.ay_chip_type,
            Ay8910ChipType::Ay8910 | Ay8910ChipType::Ay8912 | Ay8910ChipType::Ay8913
        ))
        || header.ay8910_flags.discrete_output
        || header.ay8910_flags.raw_output
        || header.ay8910_flags.reserved != 0
    {
        return Err(ChipError::InvalidConfiguration {
            chip: Chip::Ay8910,
            reason: "YM2149 emulation does not support this AY variant or output flags",
        });
    }
    Ok(ym && header.ay8910_flags.ym_pin26_low)
}

fn create_player(bytes: Vec<u8>) -> Result<VgmSequencer<'static>, Box<dyn Error>> {
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Ay8910, |config| {
        let divide_clock = ym2149_clock_divider(config.header())?;
        Ok(Box::new(YmfmChip::new_ym2149(
            config.clock(),
            divide_clock,
        )?))
    });

    // YM2151 is not registered, so the sequencer uses its built-in YMFM backend.
    let mut player = VgmSequencer::from_vgm_with_factories(bytes, SAMPLE_RATE, 512, factories)?;
    for chip in [Chip::Ym2151, Chip::Ay8910] {
        if !player.chip_instances().any(|(present, _)| present == chip) {
            return Err(io::Error::other(format!("VGM does not contain {chip:?}")).into());
        }
    }
    player.set_loop_count(Some(1));
    Ok(player)
}

fn write_wav(
    player: &mut VgmSequencer<'_>,
    output: impl Write + Seek,
) -> Result<(), Box<dyn Error>> {
    let mut writer = hound::WavWriter::new(
        BufWriter::new(output),
        hound::WavSpec {
            channels: 2,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    let mut data_bytes = 0_u32;
    while let Some(block) = player.render()? {
        data_bytes = block
            .frames()
            .len()
            .checked_mul(4)
            .and_then(|bytes| u32::try_from(bytes).ok())
            .and_then(|bytes| data_bytes.checked_add(bytes))
            .filter(|bytes| *bytes <= MAX_WAV_DATA_BYTES)
            .ok_or_else(|| io::Error::other("WAV output exceeds the RIFF size limit"))?;
        for &sample in block.frames().iter().flatten() {
            if !sample.is_finite() {
                return Err(io::Error::other("sequencer produced a non-finite sample").into());
            }
            let sample = (sample.clamp(-1.0, 1.0 - f32::EPSILON) * 32_768.0) as i16;
            writer.write_sample(sample)?;
        }
    }
    writer.finalize()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let (Some(input), Some(output), None) = (args.next(), args.next(), args.next()) else {
        return Err("Usage: ym2151_ym2149 <input.vgm|input.vgz> <output.wav>".into());
    };
    let bytes = decode_vgm(read_bounded(File::open(input)?)?)?;
    let mut player = create_player(bytes)?;
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    write_wav(&mut player, output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipstream::sequencer::Instance;
    use flate2::{Compression, write::GzEncoder};
    use soundlog::chip::Ay8910Spec;
    use soundlog::vgm::{VgmBuilder, VgmDocument};

    fn fixture() -> Vec<u8> {
        let fm =
            VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm")[..])
                .unwrap();
        let mut builder = VgmBuilder::new();
        builder.set_version(0x171);
        builder.register_chip(Chip::Ym2151, Instance::Primary, fm.header.ym2151_clock);
        builder.register_chip(Chip::Ay8910, Instance::Primary, 1_792_000);
        for (register, value) in [(0, 255), (1, 0), (7, 0xfe), (8, 15)] {
            builder.add_chip_write(Instance::Primary, Ay8910Spec { register, value });
        }
        for command in fm.commands {
            builder.add_vgm_command(command);
        }
        Vec::from(builder.finalize())
    }

    fn render(bytes: Vec<u8>, muted: Option<Chip>) -> (hound::WavSpec, Vec<i16>) {
        let mut player = create_player(bytes).unwrap();
        if let Some(chip) = muted {
            player.set_gain(chip, Instance::Primary, 0.0).unwrap();
        }
        let mut output = Cursor::new(Vec::new());
        write_wav(&mut player, &mut output).unwrap();
        output.set_position(0);
        let mut reader = hound::WavReader::new(output).unwrap();
        (
            reader.spec(),
            reader
                .samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
        )
    }

    #[test]
    fn renders_both_chips_to_stereo_wav() {
        let input = fixture();
        let (spec, mixed) = render(input.clone(), None);
        let (_, without_fm) = render(input.clone(), Some(Chip::Ym2151));
        let (_, without_ay) = render(input, Some(Chip::Ay8910));
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, SAMPLE_RATE);
        assert_eq!(spec.bits_per_sample, 16);
        assert!(mixed.len() > SAMPLE_RATE as usize);
        assert!(without_fm.iter().any(|&sample| sample != 0));
        assert!(without_ay.iter().any(|&sample| sample != 0));
        assert_ne!(mixed, without_fm);
        assert_ne!(mixed, without_ay);
    }

    #[test]
    fn handles_gzip_and_rejects_bad_input() {
        let input = fixture();
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&input).unwrap();
        assert_eq!(decode_vgm(encoder.finish().unwrap()).unwrap(), input);
        assert!(decode_vgm(vec![0x1f, 0x8b, 0]).is_err());
        assert!(create_player(b"not a VGM".to_vec()).is_err());
    }

    #[test]
    fn rejects_unsupported_ay_configuration_and_missing_chip() {
        let input = fixture();
        let mut document = VgmDocument::try_from(input.as_slice()).unwrap();
        document.header.ay8910_flags.raw_output = true;
        assert!(create_player(Vec::from(document)).is_err());
        assert!(
            create_player(include_bytes!("../../soundlog/assets/vgm/ym2151_a4.vgm").to_vec())
                .is_err()
        );
    }

    #[test]
    fn validates_ay_types_flags_and_ym_clock_divider() {
        let input = fixture();
        let mut header = VgmDocument::try_from(input.as_slice()).unwrap().header;
        header.ay_chip_type = Ay8910ChipType::Ym2149;
        assert!(!ym2149_clock_divider(&header).unwrap());
        header.ay8910_flags.ym_pin26_low = true;
        assert!(ym2149_clock_divider(&header).unwrap());
        header.ay_chip_type = Ay8910ChipType::Ay8910;
        assert!(!ym2149_clock_divider(&header).unwrap());
        header.ay_chip_type = Ay8910ChipType::Ay8930;
        assert!(ym2149_clock_divider(&header).is_err());
        header.ay_chip_type = Ay8910ChipType::Ym2149;
        header.ay8910_flags.discrete_output = true;
        assert!(ym2149_clock_divider(&header).is_err());
        header.ay8910_flags.discrete_output = false;
        header.ay8910_flags.reserved = 1;
        assert!(ym2149_clock_divider(&header).is_err());
    }
}
