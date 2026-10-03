//! Reference profiler for the complete MML-to-VGM streaming path.
//!
//! The MML input and optional PDX input are embedded at compile time. The profiler
//! parses the MML, compiles it to MDX, loads any PDX, and creates a lazy VGM generator.
//! It consumes generated commands up to a fixed budget without writing output files.
//! Parsing and compilation use the ordinary APIs without collecting source maps.
//! The budget keeps this reference profiler finite even when the input loops.
//!
//! Build the profiler in release mode with debug symbols:
//!
//! ```text
//! RUSTFLAGS="-C debuginfo=2" cargo build --release -p soundlog-cli --bin soundlog-mml-prof
//! ```
//!
//! Profile heap usage with Massif:
//!
//! ```text
//! valgrind --tool=massif \
//!   --massif-out-file=massif-soundlog-mml-prof.out \
//!   target/release/soundlog-mml-prof
//! massif-visualizer massif-soundlog-mml-prof.out
//! ```
//!
//! Profile allocation hotspots with heaptrack:
//!
//! ```text
//! heaptrack target/release/soundlog-mml-prof
//! heaptrack_gui heaptrack.soundlog_mml_prof.<pid>.zst
//! ```

use std::hint::black_box;

use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_stream_generator};
use soundlog::mdx::package::MdxPackage;

const MML_BYTES: &[u8] = include_bytes!("../../../mmlx/assets/mdx/readable.mml");
const PDX_BYTES: Option<&[u8]> = None;
const MAX_COMMANDS: usize = 100_000;

fn main() {
    let source = std::str::from_utf8(MML_BYTES).expect("embedded MML must be UTF-8");
    let mml = mmlx::mdx::parse(source).expect("embedded MML must parse");
    let mdx = mmlx::mdx::compile(&mml).expect("embedded MML must compile");
    drop(mml);
    let package = MdxPackage::parse_owned(
        mdx.to_bytes().expect("compiled MDX must serialize"),
        PDX_BYTES.map(<[u8]>::to_vec),
    )
    .expect("embedded MDX and PDX must be a valid package");
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
