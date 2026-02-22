// chipstream/crates/soundlog-debuger/src/cui/test.rs
use std::convert::TryInto;
use std::path::Path;

use anyhow::Result;

use soundlog::VgmDocument;

/// Test command: parse, serialize, re-parse roundtrip test and compare binary bytes.
/// Prints detailed diagnostics including a compact field-by-field comparison.
///
/// The comparison is semantic: a roundtrip is considered successful if either the
/// serialized bytes match exactly, or the parsed documents match except for
/// placement-only differences (GD3/data offset).
pub fn test_roundtrip(path: &Path, data: Vec<u8>, dry_run: bool) -> Result<()> {
    // Prepare quoted full-path string for one-line outputs. Try to canonicalize to get absolute path,
    // but fall back to the provided path if canonicalize fails.
    let file_str = match path.canonicalize() {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    };

    // Parse original bytes, but on parse error print filename + parse error and continue.
    let doc_orig_res: Result<VgmDocument, _> = (&data[..]).try_into();
    let doc_orig = match doc_orig_res {
        Ok(d) => d,
        Err(e) => {
            eprintln!("\"{}\": parse error: {}", file_str, e);
            return Ok(());
        }
    };

    // Round-trip: serialize parsed doc back to bytes and re-parse
    let rebuilt: Vec<u8> = (&doc_orig).into();
    let doc_reparsed_res: Result<VgmDocument, _> = (&rebuilt[..]).try_into();

    match doc_reparsed_res {
        Ok(doc_reparsed) => {
            // Use the diagnostic helpers from the parent `vgm` module.
            let semantic_match =
                crate::cui::vgm::docs_equal_allow_gd3_offset(&doc_orig, &doc_reparsed);
            if rebuilt == data || semantic_match {
                // Default behavior: print diagnostics unless user requested a dry-run.
                if !dry_run {
                    crate::cui::vgm::print_diag_table(&doc_orig, &doc_reparsed);
                    if rebuilt == data {
                        println!(
                            " roundtrip: serialized matches original ({} bytes)",
                            rebuilt.len()
                        );
                    }
                }
            } else {
                // One-line error with filename as requested. Exit code remains zero.
                // Inform user how to see detailed diagnostics: re-run without --dry-run.
                println!(
                    "\"{}\": roundtrip: MISMATCH (original {} bytes, serialized {} bytes) — re-run without --dry-run to see detailed diagnostics",
                    file_str,
                    data.len(),
                    rebuilt.len()
                );
                if !dry_run {
                    crate::cui::vgm::print_diag_compact(&doc_orig, &doc_reparsed, &data, &rebuilt);
                }
            }
        }
        Err(e) => {
            // One-line error with filename; re-parse failed after serialization.
            // Advise re-running without --dry-run to see serialized bytes/diagnostics.
            eprintln!(
                "\"{}\": roundtrip: serialization produced bytes (len={}), but re-parse failed: {} — re-run without --dry-run to see serialized bytes and diagnostics",
                file_str,
                rebuilt.len(),
                e
            );
        }
    }

    Ok(())
}
