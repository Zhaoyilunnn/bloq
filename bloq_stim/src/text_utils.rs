use std::fmt::Write;

/// Append a bracketed Stim instruction tag, escaping delimiters and line breaks.
/// Shared by circuit emission and tools that rewrite existing Stim text.
pub fn write_stim_tag(output: &mut String, tag: &str) {
    output.push('[');
    for character in tag.chars() {
        match character {
            ']' => output.push_str("\\C"),
            '\\' => output.push_str("\\B"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            other => output.push(other),
        }
    }
    output.push(']');
}

/// Append an integer's decimal digits. Uses `itoa` instead of `write!` because
/// emission spends a measurable share of its time formatting qubit indices,
/// measurement lookbacks, and coordinates, and the `core::fmt` machinery costs
/// several times more than `itoa`'s direct digit loop.
pub(crate) fn push_int<T: itoa::Integer>(output: &mut String, value: T) {
    output.push_str(itoa::Buffer::new().format(value));
}

/// Append a coordinate/argument, preferring the integer spelling (`1`, not
/// `1.0`) whenever the value is integral. Stim writes detector and qubit
/// coordinates as plain integers, so an integral `f64` must render without a
/// fractional part; genuinely fractional values fall back to `f64` `Display`.
pub(crate) fn push_number_prefer_int(output: &mut String, value: f64) {
    // i64::MAX rounds up to 2^63 as f64; exclude that endpoint before casting.
    if value >= i64::MIN as f64 && value < -(i64::MIN as f64) {
        let as_int = value as i64;
        if as_int as f64 == value {
            return push_int(output, as_int);
        }
    }
    write!(output, "{value}").expect("writing to a String is infallible");
}

#[cfg(test)]
mod tests {
    use super::push_number_prefer_int;

    #[test]
    fn integral_float_at_i64_upper_boundary_keeps_its_value() {
        let mut output = String::new();
        push_number_prefer_int(&mut output, 9_223_372_036_854_775_808.0);
        assert_eq!(output, "9223372036854776000");
    }
}
