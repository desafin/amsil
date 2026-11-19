# 소스에서 빌드하기

받아서 쓰기만 할 거라면 빌드할 필요가 없다. [README](README.md#설치)의 릴리스 묶음을 쓰면 된다. 이 문서는 직접 빌드하거나 코드를 고치려는 사람을 위한 것이다.

## 빌드

Ubuntu 24.04 기준. 다른 배포판은 같은 역할의 패키지를 설치하면 된다.

```
sudo apt install build-essential cmake pkg-config libx265-dev libde265-dev libnuma-dev
cargo build --release --locked
target/release/amsil
```

Rust는 [rustup](https://rustup.rs)으로 설치한다. 컴파일러 버전은 `rust-toolchain.toml`에 고정되어 있어 자동으로 맞춰진다.

- 의존성은 Cargo가 빌드할 때 받는다. 깃 서브모듈은 쓰지 않는다. 버전은 `Cargo.lock`에 고정되어 있으므로 `--locked`로 빌드한다.
- libheif는 빌드 과정에서 소스로부터 함께 빌드된다. 따로 설치하지 않는다.
- ONNX Runtime(AI 모델 실행)은 빌드할 때 자동으로 내려받는다. 인터넷 연결이 필요하다.
- 빌드하면 `target/release/`에 `amsil`과 `libwebgpu_dawn.so`가 함께 생긴다. 실행 파일은 자기 옆의 라이브러리를 먼저 찾는다.

AI 모델은 따로 받는다 (약 420MB, `~/.local/share/amsil/models`에 설치).

```
scripts/fetch-models.sh
```

## 테스트

```
cargo test --locked
```

- 실제 RAW 파일이 필요한 테스트는 `DCIM/` 아래의 샘플 사진이 있을 때만, 모델이 필요한 테스트는 모델을 받아 두었을 때만 실행된다. 없으면 건너뛴다. 샘플 사진은 개인 사진이라 저장소에 없다.
- 화면 테스트는 창을 띄우지 않고 돈다 (`egui_kittest`).
- 눈으로 확인할 화면 그림은 `cargo test -- --ignored render`로 `target/ui/`에 만든다 (샘플 사진 필요).
- README의 화면 그림(한국어 `assets/screenshots/workspace.jpg`, 영어 `workspace.en.jpg`)은 `cargo test -- --ignored render_readme_screenshot`으로 다시 만든다. 공개 저장소에 올라가는 그림이라, 쓸 사진(사람이 나오지 않는 것)의 목록이 그 테스트 안에 적혀 있다.

커밋을 올리면 GitHub Actions가 깨끗한 Ubuntu 24.04에서 같은 절차로 빌드하고 테스트한다 ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)).

## 릴리스 묶음 만들기

```
cargo build --release --locked
scripts/package.sh            # dist/amsil-<버전>-linux-x86_64.tar.gz
```

묶음에는 실행 파일, 실행 파일 옆에 두는 라이브러리(`libwebgpu_dawn.so`, `libx265`, `libde265`, `libnuma`), 모델 받기 스크립트, 안내 문서가 들어간다. x265는 배포판마다 버전 이름이 달라서(`libx265.so.199`, `.215` 등) 함께 넣지 않으면 다른 배포판에서 실행되지 않는다.

`v0.1.0`처럼 `v`로 시작하는 태그를 올리면 GitHub Actions가 Ubuntu 24.04에서 빌드해 묶음을 릴리스에 올린다 ([`.github/workflows/release.yml`](.github/workflows/release.yml)). 태그의 버전은 `Cargo.toml`의 `version`과 같아야 한다.

```
git tag v0.1.0
git push origin v0.1.0
```

묶음은 빌드한 배포판(Ubuntu 24.04)과 그보다 새로운 배포판에서 동작한다. ONNX Runtime의 미리 빌드된 라이브러리가 그 시기의 glibc를 요구하기 때문이다.

## 코드 구조

크레이트 하나다. 화면과 로직은 Model-View-Update 구조로 나뉘어 있다: 상태를 바꾸는 곳은 `App::handle` 하나뿐이고, 화면은 상태를 읽어 그리기만 하며, 오래 걸리는 일은 백그라운드 스레드가 한다.

| 파일 | 하는 일 |
|---|---|
| `main.rs` | 인자가 없으면 창을 띄우고, 있으면 명령행 도구로 동작 |
| `app.rs` | 프로그램 상태와 동작(`Action`). 화면 라이브러리를 모른다 |
| `ui.rs` | 그리기. 상태를 읽어 그리고, 사용자가 한 일을 `Action`으로 돌려준다 |
| `gui.rs` | 창을 띄우고 `app`·`ui`·`worker`를 잇는다. 파일 대화 상자, 화면 갈무리 |
| `worker.rs` | 백그라운드 작업(썸네일, 미리보기, 내보내기)을 실행하고 결과를 `Action`으로 보낸다 |
| `cli.rs` | 명령행 인자와 출력 |
| `project.rs` | 프로젝트 파일과 설정 파일 |
| `export.rs` | 사진 한 장의 변환 과정, 여러 장의 동시 실행, 자원 측정 |
| `job.rs` | 배치 실행기, 메모리 예산, 남은 시간, 폴더 탐색 |
| `raw.rs` | RAW 읽기 |
| `develop.rs` | 화이트밸런스, 색행렬, 톤 매핑, sRGB 변환, 공간 필터 |
| `edit.rs` | 보정 값과 색 보정 (Oklab) |
| `preset.rs` | 프리셋 파일 |
| `lut.rs` | `.cube` LUT |
| `frame.rs` | 촬영 정보를 적은 프레임 |
| `heif.rs` | HEIF 인코딩 |
| `gpu.rs` | AI 모델 실행 (ONNX Runtime) |
| `denoise.rs` | 노이즈 제거의 타일 처리 |
| `recommend.rs` | 사진 분류와 프리셋 추천 (CLIP) |
| `mask.rs` | 눌러서 고르는 마스크 (Segment Anything) |
| `skin.rs` | 인물 피부 보정 |
| `preview.rs` | 썸네일과 미리보기 이미지 |
| `lang.rs` | 화면과 메시지의 언어 |
| `log.rs` | 로그 파일 |

## 고칠 때 지킬 것

- **문구는 두 언어로.** 화면이나 메시지에 나오는 글은 쓰는 자리에 `t("한국어", "English")`로 나란히 적는다. 영어 화면에 한글이 남아 있으면 실패하는 테스트가 있다.
- **로그는 영어로.** 오류와 알림처럼 화면에도 나오고 로그에도 남는 글은 `t(…)` 대신 `text!("한국어", "English")`로 만들어 `Text`로 넘긴다. 화면에는 고른 언어로, 로그에는 영어로 나온다. `t(…)`로 만든 `String`은 그 순간의 언어로 굳어서 로그에 한글이 남는다. 로그에만 쓰는 글은 처음부터 영어로 적는다.
- **README는 두 언어로.** `README.md`(한국어)를 고치면 `README.en.md`(영어)도 같이 고친다. AI 모델의 출처는 테스트가 두 파일 모두에서 확인한다.
- **모델은 고정해서.** 새 모델을 더할 때는 출처(릴리스 태그나 커밋)와 SHA-256을 `scripts/fetch-models.sh`에 고정하고, README 두 개(`README.md`, `README.en.md`)의 "AI 기능과 출처"에 적는다.
- **개인 사진은 올리지 않는다.** `DCIM/`은 깃이 무시한다.
