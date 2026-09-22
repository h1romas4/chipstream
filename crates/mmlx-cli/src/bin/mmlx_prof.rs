//! Reference profiler for the complete MML-to-VGM streaming path.
//!
//! The MML fixture is embedded at compile time. The profiler parses the MML,
//! compiles it to MDX, creates a lazy VGM command generator, and consumes every
//! generated command up to a fixed budget without writing MDX or VGM files.
//! The fixture intentionally contains an `L` infinite-loop command, so the
//! budget keeps this reference profiler finite.
//!
//! Build the profiler in release mode with debug symbols:
//!
//! ```text
//! RUSTFLAGS="-C debuginfo=2" cargo build --release -p mmlx-cli --bin mmlx_prof
//! ```
//!
//! Profile heap usage with Massif:
//!
//! ```text
//! valgrind --tool=massif \
//!   --massif-out-file=massif-mmlx-prof.out \
//!   target/release/mmlx_prof
//! massif-visualizer massif-mmlx-prof.out
//! ```
//!
//! Profile allocation hotspots with heaptrack:
//!
//! ```text
//! heaptrack target/release/mmlx_prof
//! heaptrack_gui heaptrack.mmlx_prof.<pid>.zst
//! ```

use std::hint::black_box;

use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_stream_generator};
use soundlog::mdx::package::MdxPackage;

const MML_SOURCE: &str = include_str!("../../../mmlx/assets/mdx/readable.mml");
const MAX_COMMANDS: usize = 100_000;

fn main() {
    let mml = mmlx::mdx::parse(MML_SOURCE).expect("embedded MML must parse");
    let mdx = mmlx::mdx::compile(&mml).expect("embedded MML must compile");
    let package = MdxPackage::parse_owned(mdx.to_bytes(), None)
        .expect("compiled MDX must be a valid package");
    let options = MdxToVgmOptions {
        loop_count: Some(1),
        ..MdxToVgmOptions::default()
    };
    let mut generator =
        to_vgm_stream_generator(package, options).expect("VGM generator must initialize");

    let mut command_count = 0usize;
    while command_count < MAX_COMMANDS {
        let Some(command) = generator
            .next_command()
            .expect("VGM generator must produce valid commands")
        else {
            break;
        };
        black_box(command);
        command_count = command_count.saturating_add(1);
    }
    black_box(command_count);
}
