use std::path::Path;
use std::time::Duration;

use bloq_lassynth::{ComponentOptions, Port};
use bloq_utils::{Direction, UDirection};
use color_eyre::eyre::{self, WrapErr};
use glam::{IVec2, IVec3};

use super::atomic_write;
use crate::terminal;

pub(crate) fn run(
    input: &Path,
    output: Option<&Path>,
    print: bool,
    size: IVec3,
    time_limit: Duration,
    allow_spatial_hadamard: bool,
    quiet: bool,
) -> eyre::Result<()> {
    let qasm = std::fs::read_to_string(input)
        .wrap_err_with(|| format!("read OpenQASM circuit {}", input.display()))?;
    let parsed = bloq_lassynth::parse_component(&qasm)
        .wrap_err_with(|| format!("parse {}", input.display()))?;
    let inputs = default_positions(
        size,
        parsed.reset_inputs.iter().filter(|reset| !**reset).count(),
    )?;
    let outputs = default_positions(
        size,
        parsed
            .measured_outputs
            .iter()
            .filter(|measured| !**measured)
            .count(),
    )?;
    let options = ComponentOptions::new(
        size,
        inputs
            .iter()
            .map(|position| Port::new(position.extend(-1), Direction::ZPLUS, UDirection::Y)),
        outputs
            .iter()
            .map(|position| Port::new(position.extend(size.z), Direction::ZMINUS, UDirection::Y)),
        time_limit,
    )
    .with_spatial_hadamard(allow_spatial_hadamard);
    let graph = bloq_lassynth::synthesize_qasm(&qasm, &options)
        .wrap_err_with(|| format!("synthesize {}", input.display()))?;
    let blog = graph
        .with_inferred_interface()
        .wrap_err("build synthesized main module")?
        .to_blog_text();

    if print {
        terminal::write_stdout(blog.as_bytes()).wrap_err("write synthesized BLOG")?;
        return Ok(());
    }

    let output = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| input.with_extension("blog"));
    super::reject_input_output_alias(input, &output)?;
    atomic_write(&output, blog.as_bytes())
        .wrap_err_with(|| format!("write synthesized graph {}", output.display()))?;
    if !quiet {
        terminal::status(
            "Synthesized",
            format!("{} -> {}", input.display(), output.display()),
        );
    }
    Ok(())
}

fn default_positions(size: IVec3, qubits: usize) -> eyre::Result<Vec<IVec2>> {
    eyre::ensure!(size.min_element() > 0, "box dimensions must be positive");
    let capacity = i64::from(size.x) * i64::from(size.y);
    eyre::ensure!(
        i64::try_from(qubits).is_ok_and(|qubits| qubits <= capacity),
        "box face has too few data-port sites"
    );

    let preferred = [IVec2::new(1, 0), IVec2::new(0, 1)];
    let preferred_sites = preferred
        .into_iter()
        .filter(|position| position.x < size.x && position.y < size.y);
    let remaining_sites = (0..size.y)
        .flat_map(move |y| (0..size.x).map(move |x| IVec2::new(x, y)))
        .filter(move |position| !preferred.contains(position));
    Ok(preferred_sites
        .chain(remaining_sites)
        .take(qubits)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_positions_only_builds_requested_sites() {
        assert_eq!(
            default_positions(IVec3::new(i32::MAX, i32::MAX, 1), 2).unwrap(),
            [IVec2::new(1, 0), IVec2::new(0, 1)]
        );
        let _ = default_positions(IVec3::ONE, 2).expect_err("one site cannot hold two qubits");
    }
}
