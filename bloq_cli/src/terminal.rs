use std::fmt::Display;

use anstyle::{AnsiColor, Color, Style};
use clap::builder::Styles;
use color_eyre::eyre::Report;

const NOTE_STYLE: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
    .bold();
const SUCCESS_STYLE: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .bold();
const WARN_STYLE: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
    .bold();
const ERROR_STYLE: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .bold();
const DETAIL_STYLE: Style = Style::new().dimmed();
const BORDER_STYLE: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Cyan)))
    .dimmed();

const BLOQ_ASCII_ART: &str = r#" ____    _        ___      ___
| __ )  | |      / _ \    / _ \
|  _ \  | |     | | | |  | | | |
| |_) | | |___  | |_| |  | |_| |
|____/  |_____|  \___/    \__\_\
"#;

fn paint(style: Style, text: impl Display) -> String {
    format!("{}{text}{}", style.render(), style.render_reset())
}

pub(crate) fn cli_styles() -> Styles {
    Styles::styled()
        .header(NOTE_STYLE)
        .usage(NOTE_STYLE)
        .literal(SUCCESS_STYLE)
        .placeholder(WARN_STYLE)
        .valid(SUCCESS_STYLE)
        .invalid(ERROR_STYLE)
        .error(ERROR_STYLE)
        .context(DETAIL_STYLE)
        .context_value(DETAIL_STYLE)
}

/// Section heading styled to match clap's help headings, in help text and in
/// the `gallery` table alike.
pub(crate) fn heading(text: &str) -> String {
    paint(NOTE_STYLE, text)
}

pub(crate) fn help_banner() -> String {
    paint(NOTE_STYLE, BLOQ_ASCII_ART)
}

pub(crate) fn table_border(text: &str) -> String {
    paint(BORDER_STYLE, text)
}

pub(crate) fn table_accent(text: &str) -> String {
    paint(SUCCESS_STYLE, text)
}

pub(crate) fn table_muted(text: &str) -> String {
    paint(DETAIL_STYLE, text)
}

/// Cargo-style status line: a right-aligned bold-green verb plus message.
pub(crate) fn status(verb: &str, message: impl Display) {
    anstream::eprintln!("{} {message}", paint(SUCCESS_STYLE, format!("{verb:>12}")));
}

pub(crate) fn note(message: impl Display) {
    anstream::eprintln!("{} {message}", paint(NOTE_STYLE, "note:"));
}

pub(crate) fn warning(message: impl Display) {
    anstream::eprintln!("{} {message}", paint(WARN_STYLE, "warning:"));
}

pub(crate) fn error(message: impl Display) {
    anstream::eprintln!("{} {message}", paint(ERROR_STYLE, "error:"));
}

/// Write a complete stdout payload, preserving I/O errors for the process
/// boundary to classify.
pub(crate) fn write_stdout(bytes: &[u8]) -> std::io::Result<()> {
    write_output(std::io::stdout().lock(), bytes)
}

fn write_output(mut output: impl std::io::Write, bytes: &[u8]) -> std::io::Result<()> {
    output.write_all(bytes)?;
    output.flush()
}

pub(crate) fn is_broken_pipe(error: &Report) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenPipe;

    impl std::io::Write for BrokenPipe {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn broken_pipe_is_recognized_through_wrapping() {
        use color_eyre::eyre::WrapErr as _;

        let error = write_output(BrokenPipe, b"output")
            .wrap_err("write command output")
            .expect_err("broken pipe is propagated");
        assert!(is_broken_pipe(&error));
    }
}
