# 암실 (amsil)

RAW 사진을 고압축 HEIF(10bit HEVC)로 변환하는 프로그램.

지금은 RAW 한 장을 HEIF로 바꾸는 명령행 도구까지 만들어져 있다. 일괄 처리, GUI, AI 노이즈 제거 등 앞으로의 계획은 [docs/design.md](docs/design.md)에 있다.

## 빌드

Ubuntu 기준. 다른 배포판은 같은 역할의 패키지를 설치하면 된다.

```
sudo apt install build-essential cmake pkg-config libx265-dev libde265-dev libnuma-dev
cargo build --release --locked
```

Rust는 [rustup](https://rustup.rs)으로 설치한다. 컴파일러 버전은 `rust-toolchain.toml`에 고정되어 있어 자동으로 맞춰진다. libheif는 빌드 과정에서 소스로부터 함께 빌드되므로 따로 설치하지 않는다.

## 사용

```
target/release/amsil 입력.ARW 출력.heic
```

- 출력은 HEVC 4:2:0 10bit, sRGB.
- 촬영 정보(EXIF)를 그대로 옮기고, 세로로 찍은 사진은 바로 세워서 저장한다.
- 원본 RAW는 수정하지 않는다.

Sony DSLR-A900의 ARW로 확인했다. RAW 디코딩은 [rawler](https://github.com/dnglab/dnglab)가 하므로 rawler가 지원하는 다른 Bayer 센서 카메라도 읽을 수 있지만 확인하지는 않았다.

## 테스트

```
cargo test --locked
```

실제 RAW 파일이 필요한 테스트는 `DCIM/10161107/DSC04307.ARW`가 있을 때만 실행되고, 없으면 건너뛴다.

## 라이선스

GPL-3.0-or-later. [LICENSE](LICENSE) 참고.
