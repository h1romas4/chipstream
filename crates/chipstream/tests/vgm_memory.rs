use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use chipstream::sequencer::VgmSequencer;
use soundlog::chip::Chip;
use soundlog::vgm::command::{DataBlock, Instance};
use soundlog::vgm::{VgmBuilder, VgmHeader};

struct MeasuringAllocator;

static MEASURING: AtomicBool = AtomicBool::new(false);
static MAX_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static TOTAL_ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for MeasuringAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.load(Ordering::Relaxed) {
            MAX_ALLOCATION.fetch_max(layout.size(), Ordering::Relaxed);
            TOTAL_ALLOCATIONS.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if MEASURING.load(Ordering::Relaxed) {
            MAX_ALLOCATION.fetch_max(new_size, Ordering::Relaxed);
            TOTAL_ALLOCATIONS.fetch_add(new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: MeasuringAllocator = MeasuringAllocator;

#[test]
fn owned_vgm_with_large_block_is_not_copied_at_initialization() {
    const BLOCK_SIZE: usize = 4 * 1024 * 1024;
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0,
        size: BLOCK_SIZE as u32,
        data: vec![0; BLOCK_SIZE],
    });
    let bytes: Vec<u8> = builder.finalize().into();
    MAX_ALLOCATION.store(0, Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
    let result = VgmSequencer::from_vgm(bytes, 48_000, 256);
    MEASURING.store(false, Ordering::Relaxed);
    let largest = MAX_ALLOCATION.load(Ordering::Relaxed);
    assert!(result.is_ok());
    assert!(largest < 1024 * 1024, "largest allocation: {largest}");

    let mut bytes: Vec<u8> = VgmBuilder::new().finalize().into();
    let header = VgmHeader::from_bytes(&bytes).unwrap();
    let start = VgmHeader::command_start(header.version, header.data_offset);
    bytes.resize(start + 300_000, 0x70);
    bytes[start] = 0x70;
    bytes.push(0x66);
    TOTAL_ALLOCATIONS.store(0, Ordering::Relaxed);
    MEASURING.store(true, Ordering::Relaxed);
    let result = VgmSequencer::from_vgm(bytes, 48_000, 256);
    MEASURING.store(false, Ordering::Relaxed);
    let total = TOTAL_ALLOCATIONS.load(Ordering::Relaxed);
    assert!(result.is_ok());
    assert!(total <= 128 * 1024, "total allocated: {total}");
}
