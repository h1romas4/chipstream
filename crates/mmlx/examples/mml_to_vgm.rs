//! Parse and compile an MML string, then print YM2151 writes and VGM waits.
//!
//! Run with `cargo run -p mmlx --example mml_to_vgm`. Separate callbacks print
//! YM2151 register writes with state events and VGM waits with their sample
//! positions as the stream is generated.

use std::error::Error;

use soundlog::chip::state::Ym2151State;
use soundlog::mdx::convert::{to_vgm_stream_generator, MdxToVgmOptions};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::command::Instance;
use soundlog::vgm::VgmCallbackStream;

fn main() -> Result<(), Box<dyn Error>> {
    let source = r#"
#title "Callback stream example"
@1 = {
  /* AR  D1R D2R RR D1L TL  KS MUL DT1 DT2 AME */
      28, 4,  0,  5, 1,  37, 2, 1,  7,  0,  0,
      22, 9,  1,  2, 1,  47, 2, 12, 0,  0,  0,
      29, 4,  3,  6, 1,  37, 1, 3,  3,  0,  0,
      15, 7,  0,  5, 10,  0, 2, 1,  0,  0,  1,
  /* CON FL OP */
      2,  7, 15
}
A t120 @1 [c4 d4 e4]2
B t120 @1 [e4 f4 g4]2
C t120 @1 [g4 a4 b4]2
"#;

    let parsed = mmlx::mdx::parse(source)?;
    let mdx = mmlx::mdx::compile(&parsed)?;
    let package = MdxPackage { mdx, pdx: None };
    let options = MdxToVgmOptions {
        loop_count: Some(1),
        ..MdxToVgmOptions::default()
    };
    let ym2151_clock = options.ym2151_clock as f32;
    let generator = to_vgm_stream_generator(package, options)?;

    let mut stream = VgmCallbackStream::from_generator(generator);
    stream.track_state::<Ym2151State>(Instance::Primary, ym2151_clock);

    println!("{:<12} {:<40} Events", "Samples", "Register Write");

    stream.on_wait(|wait, sample, _events| {
        let start_sample = sample.saturating_sub(wait.0 as usize);
        println!("{start_sample:<12} WaitSamples({})", wait.0);
    });
    stream.on_write(
        |instance, spec: soundlog::chip::Ym2151Spec, sample, events| {
            println!(
                "{:<12} Ym2151Write({instance:?}, 0x{:02X}=0x{:02X}) {:?}",
                sample,
                spec.register,
                spec.value,
                events.unwrap_or_default()
            );
        },
    );

    for result in stream {
        result?;
    }

    Ok(())
}
