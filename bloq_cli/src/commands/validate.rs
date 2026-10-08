use std::path::Path;

use color_eyre::eyre::{self, WrapErr};

use crate::terminal;

/// Check a saved Bloq IR program against the full T2 well-formedness gate.
///
/// Ordinary compilation and emission do not run this full audit in either
/// build mode, and neither decoder — `from_text` / `from_binary` — runs it.
/// This command is how a program that was written to disk, hand-edited, or
/// received from elsewhere gets checked on demand.
///
/// This deliberate off-hot-path check includes per-node SEM-MERGE.
pub(crate) fn run(input: &Path, quiet: bool) -> eyre::Result<()> {
    let program = super::load_bloq_ir(input)?;
    program
        .validate()
        .wrap_err_with(|| format!("validate {}", input.display()))?;
    if !quiet {
        terminal::status(
            "Validated",
            format!(
                "{} ({} quantum node(s), {} template(s))",
                input.display(),
                program.quantum_node_count(),
                program.templates().len()
            ),
        );
    }
    Ok(())
}
