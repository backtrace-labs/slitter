fn main() {
    println!("cargo:rerun-if-changed=c/mag.c");
    println!("cargo:rerun-if-changed=c/mag.h");
    println!("cargo:rerun-if-changed=c/map.c");
    println!("cargo:rerun-if-changed=c/map.h");

    let mut build = cc::Build::new();

    // Match the override in magazine_impl.rs
    #[cfg(feature = "test_only_small_constants")]
    build.define("SLITTER__MAGAZINE_SIZE", "6");

    build
        .file("c/mag.c")
        .file("c/map.c")
        .opt_level(2)
        .compile("slitter_support")
}
