use clap::Command;
use clap_complete::{Shell, generate};

use crate::terminal;

pub(crate) fn run(shell: Shell, mut command: Command) -> std::io::Result<()> {
    let bin_name = command.get_name().to_string();
    let mut output = Vec::new();
    generate(shell, &mut command, bin_name, &mut output);
    terminal::write_stdout(&output)
}
