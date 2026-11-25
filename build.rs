fn main() {
    // ONNX Runtime's WebGPU library (libwebgpu_dawn.so) is placed next to the executable at build
    // time. Make the executable look for it in its own folder.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        // The release bundle also puts libraries like x265 next to the executable. For what those
        // libraries look for (libnuma and so on) to be found in the same folder, the old style
        // (RPATH) is needed, not the new one (RUNPATH).
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
    }
}
