use std::cell::RefCell;
use std::rc::Rc;

use chipstream::sequencer::{Chip, Instance, VgmSequencer};
use soundlog::chip::{
    Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec, Ym2612Spec, Ym3526Spec,
    Ym3812Spec, Ymf262Spec, Ymf278bSpec,
};
use soundlog::vgm::command::WaitSamples;
use soundlog::vgm::{VgmBuilder, VgmDocument};

fn write(builder: &mut VgmBuilder, chip: &Chip, register: u8, value: u8) {
    let instance = Instance::Primary;
    match chip {
        Chip::Ym2151 => builder.add_chip_write(instance, Ym2151Spec { register, value }),
        Chip::Ym2203 => builder.add_chip_write(instance, Ym2203Spec { register, value }),
        Chip::Ym2413 => builder.add_chip_write(instance, Ym2413Spec { register, value }),
        Chip::Ym2608 => builder.add_chip_write(
            instance,
            Ym2608Spec {
                port: 0,
                register,
                value,
            },
        ),
        Chip::Ym2610b => builder.add_chip_write(
            instance,
            Ym2610Spec {
                port: 0,
                register,
                value,
            },
        ),
        Chip::Ym2612 => builder.add_chip_write(
            instance,
            Ym2612Spec {
                port: 0,
                register,
                value,
            },
        ),
        Chip::Ym3526 => builder.add_chip_write(instance, Ym3526Spec { register, value }),
        Chip::Y8950 => builder.add_chip_write(instance, Y8950Spec { register, value }),
        Chip::Ym3812 => builder.add_chip_write(instance, Ym3812Spec { register, value }),
        Chip::Ymf262 => builder.add_chip_write(
            instance,
            Ymf262Spec {
                port: 0,
                register,
                value,
            },
        ),
        Chip::Ymf278b => builder.add_chip_write(
            instance,
            Ymf278bSpec {
                port: 0,
                register,
                value,
            },
        ),
        _ => panic!("not a YMFM chip"),
    };
}

fn render(
    document: VgmDocument,
    chip: &Chip,
    rate: u32,
    chunk: usize,
) -> (Vec<[f32; 2]>, Vec<(usize, u32)>) {
    let mut player = VgmSequencer::new(document, rate, chunk).unwrap();
    let captured = Rc::new(RefCell::new(Vec::new()));
    macro_rules! listen {
        ($spec:ty) => {{
            let callback_events = Rc::clone(&captured);
            player.on_write(move |_, spec: $spec, sample, _| {
                callback_events
                    .borrow_mut()
                    .push((sample, u32::from(spec.value)));
            });
        }};
    }
    match chip {
        Chip::Ym2151 => listen!(Ym2151Spec),
        Chip::Ym2203 => listen!(Ym2203Spec),
        Chip::Ym2413 => listen!(Ym2413Spec),
        Chip::Ym2608 => listen!(Ym2608Spec),
        Chip::Ym2610b => listen!(Ym2610Spec),
        Chip::Ym2612 => listen!(Ym2612Spec),
        Chip::Ym3526 => listen!(Ym3526Spec),
        Chip::Y8950 => listen!(Y8950Spec),
        Chip::Ym3812 => listen!(Ym3812Spec),
        Chip::Ymf262 => listen!(Ymf262Spec),
        Chip::Ymf278b => listen!(Ymf278bSpec),
        _ => panic!("not a YMFM chip"),
    }
    let mut frames = Vec::new();
    while let Some(block) = player.render().unwrap() {
        frames.extend_from_slice(block.frames());
    }
    (frames, captured.borrow().clone())
}

#[test]
fn all_ymfm_chips_pace_fifo_writes_on_native_timeline() {
    for (chip, divisor, interval) in [
        (Chip::Ym2151, 64, 1),
        (Chip::Ym2203, 4, 18),
        (Chip::Ym2413, 72, 1),
        (Chip::Ym2608, 8, 18),
        (Chip::Ym2610b, 16, 9),
        (Chip::Ym2612, 144, 1),
        (Chip::Ym3526, 72, 1),
        (Chip::Y8950, 72, 1),
        (Chip::Ym3812, 72, 1),
        (Chip::Ymf262, 288, 1),
        (Chip::Ymf278b, 768, 1),
    ] {
        let document = |spaced: bool| {
            let mut builder = VgmBuilder::new();
            builder.register_chip(chip.clone(), Instance::Primary, divisor * 44_100);
            for value in 1..=3 {
                write(&mut builder, &chip, 0x20, value);
                if spaced && value != 3 {
                    builder.add_vgm_command(WaitSamples(interval));
                }
            }
            builder.add_vgm_command(WaitSamples(if spaced { 80 - 2 * interval } else { 80 }));
            builder.finalize()
        };
        let burst = render(document(false), &chip, 176_400, 7);
        assert_eq!(
            burst.1,
            vec![
                (0, 1),
                (4 * interval as usize, 2),
                (8 * interval as usize, 3)
            ],
            "{chip:?}"
        );
        assert_eq!(
            burst,
            render(document(true), &chip, 176_400, 127),
            "{chip:?}"
        );
    }
}

#[test]
fn pending_writes_at_eof_are_reported_without_extending_audio() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 64 * 44_100);
    for value in 1..=4 {
        write(&mut builder, &Chip::Ym2151, 0x20, value);
    }
    builder.add_vgm_command(WaitSamples(1));
    let (frames, events) = render(builder.finalize(), &Chip::Ym2151, 44_100, 1);
    assert_eq!(frames.len(), 1);
    assert_eq!(events, vec![(0, 1), (1, 2), (1, 3), (1, 4)]);
}

#[test]
fn paced_event_positions_remain_close_to_the_native_timeline() {
    let clock = 4_000_000;
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, clock);
    for value in 0..128 {
        write(&mut builder, &Chip::Ym2151, 0x20, value);
    }
    builder.add_vgm_command(WaitSamples(256));
    let document = builder.finalize();

    for rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
        let (_, events) = render(document.clone(), &Chip::Ym2151, rate, 17);
        assert_eq!(events.len(), 128);
        for (index, (position, value)) in events.iter().enumerate() {
            let expected =
                (index as f64 * 64.0 * f64::from(rate) / f64::from(clock) + 0.5).floor() as usize;
            assert!(
                position.abs_diff(expected) <= 1,
                "rate {rate}, write {index}: {position} vs {expected}"
            );
            assert_eq!(*value, index as u32);
        }
        assert_eq!(events, render(document.clone(), &Chip::Ym2151, rate, 257).1);
    }
}

#[test]
fn oversampled_opn_chips_retrigger_adjacent_notes() {
    for (chip, clock, channel) in [
        (Chip::Ym2203, 4_000_000, 0),
        (Chip::Ym2608, 8_000_000, 0),
        (Chip::Ym2610b, 8_000_000, 2),
    ] {
        let mut builder = VgmBuilder::new();
        builder.register_chip(chip.clone(), Instance::Primary, clock);
        for offset in [0, 4, 8, 12] {
            for (register, value) in [
                (0x30 + channel + offset, 1),
                (0x40 + channel + offset, 0),
                (0x50 + channel + offset, 31),
                (0x60 + channel + offset, 0),
                (0x70 + channel + offset, 15),
                (0x80 + channel + offset, 15),
            ] {
                write(&mut builder, &chip, register, value);
            }
        }
        for (register, value) in [
            (0xb0 + channel, 7),
            (0xb4 + channel, 0xc0),
            (0xa4 + channel, 0x24),
            (0xa0 + channel, 0x3b),
            (0x28, 0xf0 | channel),
        ] {
            write(&mut builder, &chip, register, value);
        }
        builder.add_vgm_command(WaitSamples(22_050));
        write(&mut builder, &chip, 0x28, channel);
        write(&mut builder, &chip, 0x28, 0xf0 | channel);
        builder.add_vgm_command(WaitSamples(4_410));
        let (frames, events) = render(builder.finalize(), &chip, 192_000, 257);
        let key_events: Vec<_> = events
            .iter()
            .filter(|(_, value)| *value == u32::from(0xf0 | channel))
            .collect();
        assert_eq!(key_events.len(), 2, "{chip:?}");
        let peak = |start: usize| {
            frames[start..start + 4_000]
                .iter()
                .flatten()
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()))
        };
        let initial = peak(key_events[0].0 + 500);
        let retriggered = peak(key_events[1].0 + 500);
        assert!(
            initial > 0.001 && retriggered > initial * 0.5,
            "{chip:?}: initial {initial}, retriggered {retriggered}"
        );
    }
}
