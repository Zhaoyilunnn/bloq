use std::path::Path;

use color_eyre::eyre::{self, WrapErr};

use crate::terminal;

pub(crate) fn run(input: &Path) -> eyre::Result<()> {
    let program = super::load_bloq_ir(input)?;
    let stats = program.stats().wrap_err("inventory stored Bloq IR")?;
    terminal::write_stdout(format!("{stats}\n").as_bytes()).wrap_err("write IR statistics")
}
