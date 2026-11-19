# 함께 들어가는 다른 사람들의 소프트웨어

암실의 릴리스 묶음에는 아래의 오픈소스가 들어 있다. 암실 자체는 GPL-3.0-or-later이고 소스는 <https://github.com/desafin/amsil>에 있다.

## 묶음 안에 파일로 들어가는 라이브러리

Ubuntu 24.04의 패키지에서 가져온 것이다. 정확한 버전은 묶음 안의 `BUNDLED-LIBS.txt`에 적혀 있고, 소스는 <https://packages.ubuntu.com>의 해당 소스 패키지나 아래의 원래 저장소에서 받을 수 있다.

| 파일 | 이름 | 라이선스 | 소스 |
|---|---|---|---|
| `libx265.so.*` | x265 (HEVC 인코더) | GPL-2.0 이상 | <https://bitbucket.org/multicoreware/x265_git/src/master/> |
| `libde265.so.*` | libde265 (HEVC 디코더) | LGPL-3.0 | <https://github.com/strukturag/libde265> |
| `libnuma.so.*` | numactl | LGPL-2.1 | <https://github.com/numactl/numactl> |
| `libwebgpu_dawn.so` | Dawn (WebGPU 구현, ONNX Runtime이 쓴다) | BSD-3-Clause | <https://dawn.googlesource.com/dawn> |

## 실행 파일 안에 들어가는 것

| 이름 | 라이선스 | 소스 |
|---|---|---|
| libheif (HEIF 읽기·쓰기) | LGPL-3.0 | <https://github.com/strukturag/libheif> |
| ONNX Runtime (AI 모델 실행) | MIT | <https://onnxruntime.ai>, Rust 쪽은 <https://github.com/pykeio/ort> |
| rawler (RAW 읽기) | LGPL-2.1 | <https://github.com/dnglab/dnglab> |
| egui, eframe (화면) | MIT 또는 Apache-2.0 | <https://github.com/emilk/egui> |
| 나눔고딕 (글꼴) | SIL Open Font License 1.1 | <https://hangeul.naver.com/font> |
| 그 밖의 Rust 크레이트 | 대부분 MIT 또는 Apache-2.0 | 목록과 버전은 저장소의 `Cargo.lock` |

LGPL 라이브러리(libheif, rawler)를 고친 것으로 바꿔 쓰려면 저장소의 소스에서 다시 빌드하면 된다 (`BUILD.md`).

## 묶음에 들어 있지 않고 따로 받는 것

AI 모델은 `fetch-models.sh`가 받는다. 출처와 라이선스는 README의 "AI 기능과 출처"에 있다.
