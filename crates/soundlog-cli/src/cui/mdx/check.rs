use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};

use super::mml::{parse_input, parse_reader};

/// Parse an MML file and optionally print its typed syntax tree.
pub fn check(input: &Path, verbose: bool) -> Result<()> {
    check_with_stdin(input, verbose, false)
}

/// Parse an MML file or stdin and optionally print its typed syntax tree.
pub fn check_with_stdin(input: &Path, verbose: bool, stdin: bool) -> Result<()> {
    let document = if stdin || input == Path::new("-") {
        parse_reader(input, io::stdin().lock())?
    } else {
        parse_input(input)?
    };
    if verbose {
        let tree = mmlx::mdx::format_tree(&document);
        let mut stdout = io::BufWriter::new(io::stdout().lock());
        match stdout
            .write_all(tree.as_bytes())
            .and_then(|()| stdout.write_all(b"\n"))
            .and_then(|()| stdout.flush())
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error).context("failed to write verbose output"),
        }
    }
    Ok(())
}
