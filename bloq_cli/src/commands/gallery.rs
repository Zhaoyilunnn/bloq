use bloq_graph::GalleryItem;

use crate::terminal;

pub(crate) fn run() -> std::io::Result<()> {
    terminal::write_stdout(render_table().as_bytes())
}

fn render_table() -> String {
    let entries = GalleryItem::iter().collect::<Vec<_>>();
    let id_width = entries
        .iter()
        .map(|entry| entry.id().len())
        .chain(std::iter::once("Id".len()))
        .max()
        .unwrap_or(2);
    let category_width = entries
        .iter()
        .map(|entry| format_entry_categories(*entry))
        .map(|categories| categories.len())
        .chain(std::iter::once("Categories".len()))
        .max()
        .unwrap_or(10);
    let description_width = entries
        .iter()
        .map(|entry| entry.description().len())
        .chain(std::iter::once("Description".len()))
        .max()
        .unwrap_or(11);

    let mut out = String::new();
    out.push_str(&terminal::heading("Gallery Catalog"));
    out.push('\n');
    out.push_str(&terminal::table_muted(
        "Built-in block graphs available from `bloq --gallery <id>`.",
    ));
    out.push_str("\n\n");

    out.push_str(&table_rule(
        '┌',
        '┬',
        '┐',
        id_width,
        category_width,
        description_width,
    ));
    out.push('\n');
    out.push_str(&table_row(
        &terminal::heading(&format!("{:<id_width$}", "Id")),
        &terminal::heading(&format!("{:<category_width$}", "Categories")),
        &terminal::heading(&format!("{:<description_width$}", "Description")),
    ));
    out.push('\n');
    out.push_str(&table_rule(
        '├',
        '┼',
        '┤',
        id_width,
        category_width,
        description_width,
    ));
    out.push('\n');

    for (index, entry) in entries.iter().enumerate() {
        let id = entry.id();
        let categories = format_entry_categories(*entry);
        out.push_str(&table_row(
            &terminal::table_accent(&format!("{:<id_width$}", id)),
            &format!("{:<category_width$}", categories),
            &format!("{:<description_width$}", entry.description()),
        ));
        out.push('\n');
        let last = index + 1 == entries.len();
        out.push_str(&table_rule(
            if last { '└' } else { '├' },
            if last { '┴' } else { '┼' },
            if last { '┘' } else { '┤' },
            id_width,
            category_width,
            description_width,
        ));
        if !last {
            out.push('\n');
        }
    }

    out
}

fn table_rule(
    left: char,
    mid: char,
    right: char,
    id_width: usize,
    category_width: usize,
    description_width: usize,
) -> String {
    terminal::table_border(&format!(
        "{left}{}{mid}{}{mid}{}{right}",
        "─".repeat(id_width + 2),
        "─".repeat(category_width + 2),
        "─".repeat(description_width + 2)
    ))
}

fn table_row(id: &str, categories: &str, description: &str) -> String {
    format!("│ {id} │ {categories} │ {description} │")
}

fn format_entry_categories(entry: GalleryItem) -> String {
    entry
        .categories()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_table_lists_known_entries() {
        let rendered = render_table();
        assert!(rendered.contains("Gallery Catalog"));
        assert!(rendered.contains("cnot"));
        assert!(rendered.contains("three_cnots"));
        assert!(rendered.contains("clifford"));
        assert!(rendered.contains("┌"));
        assert!(rendered.contains("┘"));
    }
}
