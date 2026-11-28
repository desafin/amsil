# AI 모델과 출처

[English](../en/ai-models.md)

## 모델 개요

AI 기능 네 가지는 모두 **다른 사람들이 만들어 공개한 모델을 그대로** 쓴다. 암실이 새로 학습시킨 모델은 없다. 누가 만든 무엇을 어디서 받아 쓰는지는 아래와 같다. 프로그램 안에서도 위쪽의 **[AI 정보]** 를 누르면 같은 내용이 나오고, 각 기능의 패널에도 모델 이름이 적혀 있다.

| 기능 | 모델 | 만든 곳 | 라이선스 | 크기 |
|---|---|---|---|---|
| 노이즈 제거 | RawNIND UtNet2 | Benoit Brummer (UCLouvain) | GPL-3.0 | 58MB |
| 마스크 | Segment Anything 2.1 (Hiera Small) | Meta | Apache-2.0 | 170MB |
| 프리셋 추천 | CLIP ViT-B/32 | OpenAI | MIT | 176MB |
| 피부 보정 | MediaPipe selfie multiclass 256×256 | Google | Apache-2.0 | 16MB |

- 모델은 모두 **이 컴퓨터 안에서만** 돈다. 사진이 밖으로 나가지 않는다.
- 모델 파일은 저장소와 릴리스 묶음에 들어 있지 않다. `fetch-models.sh`가 아래에 적은 고정된 위치에서 받아 SHA-256을 확인한 뒤 `~/.local/share/amsil/models`에 둔다. 체크섬이 다르면 설치하지 않는다.
- 모델이 없으면 그 기능만 꺼지고 나머지는 그대로 동작한다.

### 노이즈 제거: RawNIND UtNet2

- **만든 곳**: Benoit Brummer (UCLouvain). 저장소 <https://github.com/trougnouf/rawnind_jddc>, 논문 <https://arxiv.org/abs/2501.08924>
- **라이선스**: GPL-3.0. 학습에는 실제 카메라로 찍은 노이즈 있는 사진과 깨끗한 사진의 쌍(RawNIND, CC BY 4.0 / CC0)을 썼다.
- **받는 곳**: [darktable-ai](https://github.com/darktable-org/darktable-ai) 릴리스 `release-5.6.0`의 `rawdenoise-nind`. darktable 5.6이 쓰는 것과 같은 파일이다.
- **암실에서 하는 일**: 센서 데이터를 512화소 타일로 나눠 모델에 넣고, 결과를 정한 레벨만큼 원래 현상과 섞는다. GPU가 있으면 GPU로 돌린다.

### 마스크: Segment Anything 2.1 (Hiera Small)

- **만든 곳**: Meta. 저장소 <https://github.com/facebookresearch/sam2>, 논문 <https://arxiv.org/abs/2408.00714>
- **라이선스**: Apache-2.0. 다만 학습에 쓰인 데이터 가운데 SA-1B에는 Meta의 연구용 라이선스가 따로 있다 (SA-V는 CC BY 4.0).
- **받는 곳**: darktable-ai 릴리스 `release-5.6.0`의 `mask-object-sam21-small` (ONNX로 바꾼 것).
- **암실에서 하는 일**: 사진을 누른 자리를 모델에 주어 그 자리의 물체를 골라낸다. 모델이 내는 256×256 격자의 결과를 사진의 윤곽에 맞춰 원래 크기로 키우는 부분은 암실이 한다. CPU로 돌린다.

### 프리셋 추천: CLIP ViT-B/32

- **만든 곳**: OpenAI. 저장소 <https://github.com/openai/CLIP>, 논문 <https://arxiv.org/abs/2103.00020>
- **라이선스**: MIT.
- **받는 곳**: Hugging Face [`Xenova/clip-vit-base-patch32`](https://huggingface.co/Xenova/clip-vit-base-patch32)의 고정된 커밋. OpenAI의 모델을 ONNX로 바꾼 것이고, 그 가운데 이미지 인코더(반정밀도판)만 쓴다.
- **암실에서 하는 일**: 사진을 "a photo of food" 같은 문장들과 견주어 음식, 인물, 풍경, 야경, 건축, 기타 가운데 무엇인지 가늠한다. 문장 쪽의 계산 결과는 미리 구해 저장소에 넣어 두었다 ([`assets/clip/categories.txt`](../../assets/clip/categories.txt), 만드는 방법은 [`scripts/clip-text-embeddings.py`](../../scripts/clip-text-embeddings.py)). CPU로 돌린다 (장당 0.02초).

### 피부 보정: MediaPipe selfie multiclass 256×256

- **만든 곳**: Google. 모델 설명 <https://ai.google.dev/edge/mediapipe/solutions/vision/image_segmenter>
- **라이선스**: Apache-2.0.
- **받는 곳**: Hugging Face [`senty-au/selfie_multiclass_256x256-ONNX`](https://huggingface.co/senty-au/selfie_multiclass_256x256-ONNX)의 고정된 커밋. Google의 모델(TFLite)을 다른 사람이 ONNX로 바꾼 것이라, Google이 배포하는 원본과 같은 결과를 내는지 사진 25장으로 직접 견줘 확인했다.
- **암실에서 하는 일**: 사진에서 피부(얼굴과 몸)가 어디인지 찾는 데만 쓴다. 보정 자체는 신경망이 아니라 흐림 계산으로 한다. CPU로 돌린다 (0.03초).
