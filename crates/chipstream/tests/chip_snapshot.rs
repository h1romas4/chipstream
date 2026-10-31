use chipstream::chip::{SoundChip, YmfmChip};
use soundlog::chip::Chip;

#[cfg(feature = "okim6258")]
use chipstream::chip::Okim6258;
#[cfg(feature = "okim6295")]
use chipstream::chip::Okim6295;
#[cfg(feature = "pwm")]
use chipstream::chip::Pwm;
#[cfg(feature = "segapcm")]
use chipstream::chip::SegaPcm;
#[cfg(feature = "sn76496")]
use chipstream::chip::Sn76496;
#[cfg(feature = "c140")]
use chipstream::chip::{C140, C140Type};

fn assert_restore_replays_identically(chip: &mut dyn SoundChip) {
    let mut warmup = vec![0; chip.channels() * 37];
    chip.generate(&mut warmup).unwrap();

    let snapshot = chip.save_state().unwrap();
    let mut expected = vec![0; chip.channels() * 53];
    chip.generate(&mut expected).unwrap();

    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut actual = vec![0; chip.channels() * 53];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn ymfm_state_can_be_restored() {
    let mut chip = YmfmChip::new(Chip::Ym2151, 4_000_000).unwrap();
    assert_restore_replays_identically(&mut chip);
}

#[cfg(feature = "c140")]
#[test]
fn c140_state_can_be_restored() {
    let mut chip = C140::new(12_288_000, C140Type::System2).unwrap();
    assert_restore_replays_identically(&mut chip);
}

#[cfg(feature = "sn76496")]
#[test]
fn sn76496_state_can_be_restored() {
    let mut chip = Sn76496::new(3_579_545).unwrap();
    assert_restore_replays_identically(&mut chip);
}

#[cfg(feature = "segapcm")]
#[test]
fn sega_pcm_state_can_be_restored() {
    let mut chip = SegaPcm::new(4_000_000).unwrap();
    assert_restore_replays_identically(&mut chip);
}

#[cfg(feature = "okim6258")]
#[test]
fn okim6258_state_can_be_restored() {
    let mut chip = Okim6258::new(8_000_000).unwrap();
    assert_restore_replays_identically(&mut chip);
}

#[cfg(feature = "okim6295")]
#[test]
fn okim6295_state_can_be_restored() {
    let mut chip = Okim6295::new(1_000_000).unwrap();
    assert_restore_replays_identically(&mut chip);
}

#[cfg(feature = "pwm")]
#[test]
fn pwm_state_can_be_restored() {
    let mut chip = Pwm::new(44_100).unwrap();
    assert_restore_replays_identically(&mut chip);
}
