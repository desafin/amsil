fn main() {
    // ONNX Runtime의 WebGPU 라이브러리(libwebgpu_dawn.so)는 빌드할 때 실행 파일 옆에 놓인다.
    // 실행 파일이 자기 폴더에서 그것을 찾도록 한다.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
    }
}
