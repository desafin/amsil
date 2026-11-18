fn main() {
    // ONNX Runtime의 WebGPU 라이브러리(libwebgpu_dawn.so)는 빌드할 때 실행 파일 옆에 놓인다.
    // 실행 파일이 자기 폴더에서 그것을 찾도록 한다.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
        // 릴리스 묶음에는 x265 같은 라이브러리도 실행 파일 옆에 넣는다. 그 라이브러리들이 찾는 것
        // (libnuma 등)도 같은 폴더에서 찾게 하려면, 새 방식(RUNPATH)이 아닌 옛 방식(RPATH)이어야 한다.
        println!("cargo:rustc-link-arg=-Wl,--disable-new-dtags");
    }
}
