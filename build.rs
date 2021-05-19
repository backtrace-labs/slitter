fn main() {
    println!("cargo:rerun-if-changed=c/map.c");
    println!("cargo:rerun-if-changed=c/map.h");

    cc::Build::new()
        .file("c/map.c")
        .opt_level(2)
        .compile("slitter_support")
}
