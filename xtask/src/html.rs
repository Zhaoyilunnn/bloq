//! Minimal HTML escaping for report generation.

/// Escapes `&`, `<`, `>`, and `"` for safe inclusion in HTML text and
/// attributes.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_html_text_and_attributes() {
        assert_eq!(
            escape("<a title=\"A&B\">"),
            "&lt;a title=&quot;A&amp;B&quot;&gt;"
        );
    }
}
