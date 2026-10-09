fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rustc-check-cfg=cfg(nab_sdk_patch)");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(&manifest_dir).join("Cargo.toml");
    let manifest = std::fs::read_to_string(&path).expect("read Cargo.toml");
    // `cargo publish` removes `[patch]` before it compiles the tarball.
    // The registry SDK has no ingress types, so that build must not see them.
    if manifest.contains("[patch.crates-io]") {
        println!("cargo:rustc-cfg=nab_sdk_patch");
    }
}
