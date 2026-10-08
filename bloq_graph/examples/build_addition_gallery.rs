//! Write an N-bit adder BLOG file. Arguments: BITS OUTPUT.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (bits, path) = match args.next() {
        Some(bits) => (
            bits.parse::<usize>()?,
            args.next().ok_or("expected BITS OUTPUT")?,
        ),
        None => (10, "bloq_graph/assets/ten_bit_adder.blog".into()),
    };
    let program = bloq_test::benchmark::controlled_adder(bits);
    std::fs::write(path, format!("{}\n", program.to_blog_text().trim_end()))?;
    Ok(())
}
