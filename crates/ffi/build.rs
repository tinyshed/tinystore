// Built as Python's extension module, the library links no libpython: the
// interpreter that loads it has the symbols. Linux leaves them to the loader
// by itself, and macOS's linker must be told, as maturin tells it.
fn main() {
    let python = std::env::var_os("CARGO_FEATURE_PYTHON").is_some();
    if python && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-cdylib-link-arg=-undefined");
        println!("cargo:rustc-cdylib-link-arg=dynamic_lookup");
    }
}
