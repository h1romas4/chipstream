use std::cell::RefCell;
use std::rc::Rc;

use chipstream::chip::{ChipError, ChipRegisterWrite, SoundChip, YmfmChip};
use soundlog::chip::Chip;

struct RamChip(Rc<RefCell<[u8; 8]>>);

impl SoundChip for RamChip {
    fn chip_type(&self) -> &Chip {
        &Chip::NesApu
    }

    fn clock(&self) -> u32 {
        1
    }

    fn sample_rate(&self) -> u32 {
        1
    }

    fn channels(&self) -> usize {
        1
    }

    fn apply_register_write(&mut self, _: ChipRegisterWrite) -> Result<(), ChipError> {
        Ok(())
    }

    fn write_ram(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let mut memory = self.0.borrow_mut();
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= memory.len());
        let end = end.ok_or(ChipError::RamOutOfRange {
            chip: Chip::NesApu,
            start,
            length: data.len(),
        })?;
        memory[start..end].copy_from_slice(data);
        Ok(())
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        buffer.fill(0);
        Ok(())
    }

    fn reset(&mut self) {}
}

#[test]
fn boxed_chip_forwards_ram_writes_and_errors() {
    let memory = Rc::new(RefCell::new([0; 8]));
    let mut chip: Box<dyn SoundChip> = Box::new(RamChip(memory.clone()));
    chip.write_ram(2, &[1, 2, 3]).unwrap();
    assert_eq!(*memory.borrow(), [0, 0, 1, 2, 3, 0, 0, 0]);
    assert_eq!(
        chip.write_ram(7, &[4, 5]),
        Err(ChipError::RamOutOfRange {
            chip: Chip::NesApu,
            start: 7,
            length: 2,
        })
    );
    assert_eq!(*memory.borrow(), [0, 0, 1, 2, 3, 0, 0, 0]);
}

#[test]
fn chip_without_ram_reports_unsupported() {
    let mut chip = YmfmChip::new(Chip::Ym2151, 4_000_000).unwrap();
    assert_eq!(
        chip.write_ram(0, &[1]),
        Err(ChipError::UnsupportedRam(Chip::Ym2151))
    );
}
