fn main() {
    // Compile AOSP / AUR liblhdcv5 C source files for Windows x64 MSVC
    cc::Build::new()
        .cpp(false)
        .file("vendor/liblhdcv5/src/lhdcBT.c")
        .file("vendor/liblhdcv5/src/lhdcv5_util.c")
        .include("vendor/liblhdcv5/inc")
        .include("vendor/liblhdcv5/src")
        // MSVC compatibility definitions
        .define("_CRT_SECURE_NO_WARNINGS", None)
        .define("inline", "__inline")
        .opt_level(3)
        .flag_if_supported("/O2")
        .flag_if_supported("/Gy")
        .compile("lhdcv5_enc");

    println!("cargo:rerun-if-changed=vendor/liblhdcv5/");
}