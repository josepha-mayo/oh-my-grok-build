fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();

    if target.ends_with("-pc-windows-msvc") {
        println!("cargo:rustc-link-arg-bin=omgb=/DEBUG:NONE");
    }
}
