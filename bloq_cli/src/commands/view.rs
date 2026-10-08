use color_eyre::eyre::{self, WrapErr};

use bloq_graph::{BlockGraph, Direction, GltfFaceSelector};

use super::atomic_write_with;
use super::compile::CompileSource;
use crate::terminal;

pub(crate) fn run_ir(
    input: &std::path::Path,
    output: Option<&std::path::Path>,
    include_classical: bool,
    quiet: bool,
) -> eyre::Result<()> {
    let default_output = input.with_extension("svg");
    let output = output.unwrap_or(&default_output);
    super::reject_input_output_alias(input, output)?;
    let program = super::load_bloq_ir(input)?;
    super::atomic_write(output, program.to_svg(include_classical).as_bytes())
        .wrap_err("write IR SVG")?;
    if !quiet {
        terminal::status("Generated", format!("IR graph -> {}", output.display()));
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct ViewCommandOptions {
    pub(crate) gltf: bool,
    pub(crate) html: bool,
    pub(crate) module_view: bool,
    pub(crate) pipe_length: f32,
    pub(crate) pop_faces_at_directions: Vec<Direction>,
    pub(crate) quiet: bool,
}

pub(crate) fn run(
    source: &CompileSource,
    graph: &BlockGraph,
    options: ViewCommandOptions,
) -> eyre::Result<()> {
    let outputs = [(options.gltf, "gltf"), (options.html, "html")]
        .into_iter()
        .filter(|(enabled, _)| *enabled)
        .map(|(_, extension)| source.default_output_path_with_extension(extension));
    if let CompileSource::InputFile(input) = source {
        for output in outputs {
            super::reject_input_output_alias(input, &output)?;
        }
    }
    let mut failures = 0usize;
    let popped_faces = options
        .pop_faces_at_directions
        .into_iter()
        .map(GltfFaceSelector::All)
        .collect::<Vec<_>>();
    if options.gltf {
        let output = source.default_output_path_with_extension("gltf");
        if let Err(error) = atomic_write_with(&output, |temporary| {
            let result = if options.module_view {
                graph.write_module_gltf_file(options.pipe_length, temporary, &popped_faces)
            } else {
                graph.write_to_gltf_file(options.pipe_length, temporary, None, &popped_faces)
            };
            result.map_err(std::io::Error::other)
        })
        .wrap_err("write glTF file")
        {
            failures += 1;
            terminal::error(format!(
                "generate glTF view at {}: {error:#}",
                output.display()
            ));
        } else if !options.quiet {
            terminal::status("Generated", format!("glTF model -> {}", output.display()));
        }
    }

    if options.html {
        let output = source.default_output_path_with_extension("html");
        if let Err(error) = atomic_write_with(&output, |temporary| {
            let result = if options.module_view {
                graph.write_module_html_viewer(options.pipe_length, temporary, &popped_faces)
            } else {
                graph.write_to_gltf_html_viewer(options.pipe_length, temporary, None, &popped_faces)
            };
            result.map_err(std::io::Error::other)
        })
        .wrap_err("write HTML viewer")
        {
            failures += 1;
            terminal::error(format!(
                "generate HTML viewer at {}: {error:#}",
                output.display()
            ));
        } else if !options.quiet {
            terminal::status("Generated", format!("HTML viewer -> {}", output.display()));
        }
    }

    if failures == 0 {
        Ok(())
    } else {
        eyre::bail!("{failures} view output(s) failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_view_outputs_are_checked_before_writing() {
        for extension in ["gltf", "html"] {
            for alias in ["direct", "hardlink", "symlink"] {
                let dir = tempfile::tempdir().unwrap();
                let input = dir.path().join(if alias == "direct" {
                    format!("source.{extension}")
                } else {
                    "source.blog".into()
                });
                let original = bloq_graph::GalleryItem::CNOT.entry().blog();
                std::fs::write(&input, original).unwrap();
                let output = input.with_extension(extension);
                match alias {
                    "hardlink" => std::fs::hard_link(&input, &output).unwrap(),
                    #[cfg(unix)]
                    "symlink" => std::os::unix::fs::symlink(&input, &output).unwrap(),
                    "direct" => {}
                    _ => continue,
                }
                let error = run(
                    &CompileSource::InputFile(input.clone()),
                    &bloq_graph::GalleryItem::CNOT.build(),
                    ViewCommandOptions {
                        gltf: true,
                        html: true,
                        module_view: false,
                        pipe_length: 1.0,
                        pop_faces_at_directions: Vec::new(),
                        quiet: true,
                    },
                )
                .unwrap_err();
                assert!(error.to_string().contains("aliases the input file"));
                assert_eq!(std::fs::read_to_string(&input).unwrap(), original);
                let other = input.with_extension(if extension == "html" { "gltf" } else { "html" });
                assert!(!other.exists(), "preflight must precede either export");
            }
        }
    }
}
