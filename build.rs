fn main() {
    let mut build = cc::Build::new();

    // Match the override in magazine_impl.rs
    #[cfg(feature = "test_only_small_constants")]
    build.define("SLITTER__SMALL_CONSTANTS", "1");

    for file in ["cache", "constants", "mag", "map", "stack"].iter() {
        println!("cargo:rerun-if-changed=c/{}.c", file);
        println!("cargo:rerun-if-changed=c/{}.h", file);

        build.file(format!("c/{}.c", file));
    }

    // This forces a cdylib to include the routines, but does not
    // export the symbols...
    println!("cargo:rustc-cdylib-link-arg=-uslitter_allocate");
    println!("cargo:rustc-cdylib-link-arg=-uslitter_release");

    build
        .include("include")
        .opt_level(2)
        .flag_if_supported("-mcx16") // enable CMPXCHB16B
        .flag("-W")
        .flag("-Wall")
        .compile("slitter_support")
}
