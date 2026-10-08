fn main() {
    println!("cargo:rerun-if-changed=assets/icons/bloq.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/icons/bloq.ico")
            .set("ProductName", "Bloq Editor")
            .set("FileDescription", "Bloq Editor")
            .compile()
            .expect("compile the Bloq Editor Windows icon resource");
    }
}
