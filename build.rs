fn main() {
    println!("cargo:rerun-if-changed=c/cache.c");
    println!("cargo:rerun-if-changed=c/cache.h");
    println!("cargo:rerun-if-changed=c/mag.c");
    println!("cargo:rerun-if-changed=c/mag.h");
    println!("cargo:rerun-if-changed=c/map.c");
    println!("cargo:rerun-if-changed=c/map.h");
    println!("cargo:rerun-if-changed=c/stack.c");
    println!("cargo:rerun-if-changed=c/stack.h");

    let mut build = cc::Build::new();

    // Match the override in magazine_impl.rs
    #[cfg(feature = "test_only_small_constants")]
    build.define("SLITTER__MAGAZINE_SIZE", "6");

    build
        .file("c/cache.c")
        .file("c/mag.c")
        .file("c/map.c")
        .file("c/stack.c")
        .opt_level(2)
        .flag_if_supported("-mcx16") // enable CMPXCHB16B
        .flag("-W")
        .flag("-Wall")
        .compile("slitter_support")
}
