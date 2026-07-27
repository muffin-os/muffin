fn main() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-arg=-T{dir}/../../../../kernel/linker-{arch}.ld");
    println!("cargo:rerun-if-changed={dir}/../../../../kernel/linker-{arch}.ld");
}
