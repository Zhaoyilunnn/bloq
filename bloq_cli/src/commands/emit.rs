use std::path::{Path, PathBuf};

use color_eyre::eyre::{self, WrapErr};

use crate::BuiltInBackend;
use crate::terminal;

use super::atomic_write;

#[derive(Debug)]
pub(crate) struct EmitCommandOptions {
    pub(crate) backend: BuiltInBackend,
    pub(crate) output: Option<PathBuf>,
    pub(crate) print: bool,
    /// Run the full well-formedness check when `--validate` was requested.
    pub(crate) validate: bool,
    /// Align and merge Clifford circuit moments within each z layer.
    pub(crate) align_moments: bool,
    pub(crate) quiet: bool,
}

/// Emit a backend target from a saved Bloq IR program.
///
/// The counterpart to `compile`, which goes from a `.blog` block graph to the
/// same targets in one step. Splitting the two lets a compiled program be
/// stored once and re-emitted — and makes IR-to-IR codec conversion fall out
/// for free.
pub(crate) fn run(input: &Path, options: EmitCommandOptions) -> eyre::Result<()> {
    if options.align_moments && options.backend != BuiltInBackend::Stim {
        eyre::bail!("--align-moments requires --backend stim");
    }
    let program = super::load_bloq_ir(input)?;
    let trust = if options.validate {
        bloq_stim::InputTrust::Checked
    } else {
        bloq_stim::InputTrust::Trusted
    };
    // The Stim backend checks only when requested through `trust`. IR exchange
    // codecs do not check, so request the same full gate here explicitly.
    if options.validate && options.backend != BuiltInBackend::Stim {
        program
            .validate()
            .wrap_err_with(|| format!("validate {}", input.display()))?;
    }

    let emitted = options.backend.emit(
        &program,
        &bloq_stim::BloqStimOptions::new()
            .with_trust(trust)
            .with_align_moments(options.align_moments),
    )?;

    if options.print {
        return emitted.write_stdout();
    }

    let path = options
        .output
        .unwrap_or_else(|| input.with_extension(options.backend.file_extension()));
    super::reject_input_output_alias(input, &path)?;
    atomic_write(&path, emitted.bytes())
        .wrap_err_with(|| format!("write output file {}", path.display()))?;
    if !options.quiet {
        terminal::status(
            "Emitted",
            format!("{} -> {}", input.display(), path.display()),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn align_moments_rejects_ir_backend_before_loading() {
        let options = EmitCommandOptions {
            backend: BuiltInBackend::IrText,
            output: None,
            print: false,
            validate: true,
            align_moments: true,
            quiet: true,
        };

        let error = run(Path::new("missing.bloq"), options)
            .expect_err("moment alignment only changes Stim emission");

        assert!(error.to_string().contains("requires --backend stim"));
    }

    #[test]
    fn ir_exchange_runs_full_validation_only_when_requested() {
        let dir = tempdir().expect("create tempdir");
        let input = dir.path().join("invalid.bloqir");
        std::fs::write(&input, "BLOQIR 1\ngraph {\n  n0 compute in0\n}\n")
            .expect("write structurally invalid IR");
        let options = |validate| EmitCommandOptions {
            backend: BuiltInBackend::IrText,
            output: Some(dir.path().join("roundtrip.bloqir")),
            print: false,
            validate,
            align_moments: false,
            quiet: true,
        };

        run(&input, options(false)).expect("IR exchange does not run a full audit");
        assert!(dir.path().join("roundtrip.bloqir").exists());
        let error = run(&input, options(true)).expect_err("explicit audit rejects invalid IR");
        assert!(format!("{error:?}").contains("WF-11"));
    }
}
