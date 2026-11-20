# 선예도 측정 도구

암실이 만든 그림의 선예도를 카메라 JPEG와 견주는 도구들이다. 캡처 샤픈의 기본값, 프리셋의 샤픈, 디모자익 방식, 인코더 설정을 정할 때 썼다. 그림을 바꾸는 코드를 고친 뒤 다시 돌려 보면 결과가 어떻게 달라졌는지 숫자로 알 수 있다.

샘플 사진(`DCIM/`, 저장소에 없음)과 python3 + numpy + Pillow가 필요하다. 그림은 `/tmp/amsil-sharpness/`에 쓴다 (`AMSIL_SHARPNESS_DIR`로 바꾸거나, 파이썬 명령의 첫 인자로 폴더를 준다).

## 순서

```
# 1. 그림 만들기 (Rust의 무시되는 테스트들)
cargo test --release -- --ignored render_for_sharpness_measurement   # 캡처 샤픈 조건별 (몇 분)
cargo test --release -- --ignored encode_for_encoder_comparison      # 1번 뒤에. x265 설정별
cargo test --release -- --ignored render_for_demosaic_comparison     # 디모자익 방식별

# 2. 분석
python3 scripts/sharpness/measure.py        # 캡처 샤픈 조건별: 선예도·노이즈·넘침·용량
python3 scripts/sharpness/camera_curve.py   # 카메라 JPEG가 굵기마다 하는 일
python3 scripts/sharpness/fit_filter.py     # 그 곡선에 맞는 샤픈 필터 (camera_curve.py 뒤에)
python3 scripts/sharpness/encoder.py        # x265 설정별 용량과 세부 보존
python3 scripts/sharpness/demosaic.py       # RCD / PPG / AI 모델의 색 복원
```

## 파일

| 파일 | 하는 일 |
|---|---|
| `common.py` | 같이 쓰는 것: 16비트 그림 읽기·쓰기(`src/raw16.rs` 형식), 카메라 JPEG 자르기, 스펙트럼, 세부·평평한 구역 나누기, 샤픈 필터의 주파수 응답 |
| `measure.py` | 캡처 샤픈 조건마다 카메라 JPEG 대비 세부(압축 전·후), 평평한 곳의 노이즈, 경계 넘침, 용량, 시간 |
| `camera_curve.py` | 샤픈 없는 현상에 대한 카메라 JPEG의 진폭 비율을 주파수마다. 1보다 크면 카메라가 키운 것(샤픈), 작으면 깎은 것(노이즈 감소). `camera_curve.json`에 쓴다 |
| `fit_filter.py` | 카메라 곡선에 상자 흐림 언샤프·가우스·대역 통과 필터를 맞춘다. `--simulate 세기 문턱1 문턱2`로 경계에만 거는 샤픈을 흉내 낸다 |
| `encoder.py` | x265 설정(품질, tune, psy-rd, aq-mode, 4:4:4)마다 용량, 압축 뒤 남는 세부, PSNR |
| `demosaic.py` | 디모자익 방식마다 RCD에 대한 진폭 비율, 색 성분의 고주파(거짓 색), 100% 크롭 비교 그림 |

측정할 사진과 자를 곳은 `common.py`의 `PHOTOS`와 `src/raw16.rs`의 `PHOTOS`에 같이 적혀 있다 (바꿀 때는 둘 다).

## 재는 방법

사진마다 세부가 많은 2048×2048 영역을 256×256 구역으로 나누고, 캡처 샤픈을 끈 그림의 중간 대역(0.08~0.2 cycles/pixel) 에너지로 "세부" 구역(위 15%)과 "평평한" 구역(아래 25%)을 고른다. 구역마다 해닝 창을 씌운 FFT로 방사 평균 파워 스펙트럼을 구하고, 두 그림의 진폭 비율을 0.02~0.06 대역이 1이 되게 맞춘다. 그래서 밝기·대비의 차이는 빠지고 굵기마다의 선명함만 남는다.

| measure.py의 열 | 뜻 |
|---|---|
| 세부/카메라 | 세부 구역에서 0.15~0.35 대역의 진폭이 카메라 JPEG의 몇 배인가 (압축 전). 1이면 카메라와 같다 |
| 압축 후 | 같은 것을 품질 55로 압축했다 푼 그림에서 |
| 평평/끔 | 평평한 구역에서 0.25~0.45 대역의 진폭이 캡처 샤픈을 끈 그림의 몇 배인가 (노이즈가 얼마나 커졌나) |
| 넘침, 넘침% | 뚜렷한 경계 둘레에서 샤픈 전 5×5의 최댓값·최솟값을 넘은 정도 (평균 × 1000, 0.02 넘는 화소의 비율) |
| 용량 | 품질 55로 압축한 파일 크기, 캡처 샤픈을 끈 것에 대한 비율 |

고감도 사진(`common.HIGH_ISO`)은 카메라 JPEG가 노이즈를 강하게 지워서 "세부/카메라"가 선예도를 말해 주지 못한다. 평균에서 뺀다.

## 지금까지의 결과 (2026-11-20)

- 캡처 샤픈 끔: 압축 후 카메라 JPEG의 0.71배. 기본값(반복 20, 문턱 0.4): 1.02배, 용량 +10%. 문턱을 사진에서 구하면(RawTherapee의 자동) 1.27배, +28%.
- 디모자익: PPG는 RCD보다 가장 고운 대역(0.4~0.45)이 1.3~1.6배, 색 성분의 고주파가 1.4~1.6배 많다 (세부가 아니라 깨짐·거짓 색). RCD로 바꾸자 용량이 16~35% 줄었다.
- 인코더: 설정을 바꿔서 공짜로 나아지는 것은 없었다. psy-rd 2는 차이 없음, aq-mode 3은 품질 60과 같은 용량·결과, `tune=grain`은 용량이 24% 줄지만 PSNR이 1.5dB 낮다.
