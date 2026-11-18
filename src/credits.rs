//! AI 기능이 쓰는 모델의 출처. 화면의 "AI 정보" 창과 각 패널의 한 줄 표시가 이것을 쓴다.
//! README의 "AI 기능과 출처"와 같은 내용이어야 한다 (테스트가 확인한다).

/// 모델을 쓰는 기능.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Denoise,
    Mask,
    Recommend,
    Skin,
}

/// AI 모델 하나의 출처.
pub struct Credit {
    pub feature: Feature,
    /// 기능의 이름 (한국어, 영어).
    pub name: (&'static str, &'static str),
    pub model: &'static str,
    /// 만든 곳.
    pub by: &'static str,
    pub license: &'static str,
    /// 만든 곳이 공개한 저장소나 모델 설명.
    pub source: &'static str,
    pub paper: Option<&'static str>,
    /// 이 프로그램이 모델 파일을 받는 곳 (한국어, 영어).
    pub from: (&'static str, &'static str),
    /// 알아 둘 것 (한국어, 영어).
    pub note: (&'static str, &'static str),
}

pub const DENOISE: Credit = Credit {
    feature: Feature::Denoise,
    name: ("노이즈 제거", "Noise reduction"),
    model: "RawNIND UtNet2",
    by: "Benoit Brummer (UCLouvain)",
    license: "GPL-3.0",
    source: "https://github.com/trougnouf/rawnind_jddc",
    paper: Some("https://arxiv.org/abs/2501.08924"),
    from: (
        "darktable-ai 릴리스 release-5.6.0 (darktable 5.6이 쓰는 것과 같은 파일)",
        "darktable-ai release release-5.6.0 (the same file darktable 5.6 uses)",
    ),
    note: (
        "실제 카메라로 찍은 노이즈 있는 사진과 깨끗한 사진의 쌍(RawNIND, CC BY 4.0 / CC0)으로 학습한 모델입니다.",
        "Trained on RawNIND: pairs of noisy and clean photos taken with real cameras (CC BY 4.0 / CC0).",
    ),
};

pub const MASK: Credit = Credit {
    feature: Feature::Mask,
    name: ("마스크", "Masks"),
    model: "Segment Anything 2.1 (Hiera Small)",
    by: "Meta",
    license: "Apache-2.0",
    source: "https://github.com/facebookresearch/sam2",
    paper: Some("https://arxiv.org/abs/2408.00714"),
    from: (
        "darktable-ai 릴리스 release-5.6.0 (ONNX로 바꾼 것)",
        "darktable-ai release release-5.6.0 (converted to ONNX)",
    ),
    note: (
        "모델 가중치는 Apache-2.0이지만, 학습에 쓰인 데이터 가운데 SA-1B에는 Meta의 연구용 라이선스가 따로 있습니다.",
        "The weights are Apache-2.0, but SA-1B, part of the training data, has a separate research-only licence from Meta.",
    ),
};

pub const RECOMMEND: Credit = Credit {
    feature: Feature::Recommend,
    name: ("프리셋 추천", "Preset suggestions"),
    model: "CLIP ViT-B/32",
    by: "OpenAI",
    license: "MIT",
    source: "https://github.com/openai/CLIP",
    paper: Some("https://arxiv.org/abs/2103.00020"),
    from: (
        "Hugging Face Xenova/clip-vit-base-patch32 (ONNX로 바꾼 것. 이미지 인코더만 씁니다)",
        "Hugging Face Xenova/clip-vit-base-patch32 (converted to ONNX; only the image encoder is used)",
    ),
    note: (
        "사진을 \"a photo of food\" 같은 문장과 견주어 무엇을 찍은 것인지 가늠합니다.",
        "Guesses what a photo shows by comparing it with sentences such as \"a photo of food\".",
    ),
};

pub const SKIN: Credit = Credit {
    feature: Feature::Skin,
    name: ("피부 보정", "Skin retouch"),
    model: "MediaPipe selfie multiclass 256×256",
    by: "Google",
    license: "Apache-2.0",
    source: "https://ai.google.dev/edge/mediapipe/solutions/vision/image_segmenter",
    paper: None,
    from: (
        "Hugging Face senty-au/selfie_multiclass_256x256-ONNX (ONNX로 바꾼 것)",
        "Hugging Face senty-au/selfie_multiclass_256x256-ONNX (converted to ONNX)",
    ),
    note: (
        "피부를 찾는 데만 씁니다. 보정 자체는 신경망이 아니라 흐림 계산으로 합니다. 바꾼 파일이 Google의 원본과 같은 결과를 내는 것을 확인했습니다.",
        "Used only to find skin; the retouch itself is done with blurs, not a neural network. The converted file was checked to give the same results as Google's original.",
    ),
};

/// 모든 AI 기능의 출처.
pub const ALL: [&Credit; 4] = [&DENOISE, &MASK, &RECOMMEND, &SKIN];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_ai_feature_names_its_model_maker_licence_and_source() {
        assert_eq!(ALL.len(), 4, "노이즈 제거, 마스크, 프리셋 추천, 피부 보정");
        for credit in ALL {
            for text in [credit.model, credit.by, credit.license] {
                assert!(!text.trim().is_empty(), "{}", credit.model);
            }
            assert!(credit.source.starts_with("https://"), "{}", credit.model);
            assert!(
                credit
                    .paper
                    .is_none_or(|paper| paper.starts_with("https://arxiv.org/abs/")),
                "{}",
                credit.model
            );
        }
        let makers: Vec<&str> = ALL.iter().map(|credit| credit.by).collect();
        assert_eq!(
            makers,
            ["Benoit Brummer (UCLouvain)", "Meta", "OpenAI", "Google"]
        );
    }

    #[test]
    fn the_readme_gives_the_same_sources_as_the_program() {
        // 화면과 문서가 서로 다른 말을 하지 않도록, 화면에 나오는 출처가 README에도 그대로 있어야 한다.
        let readme = include_str!("../README.md");
        for credit in ALL {
            let mut wanted = vec![credit.model, credit.by, credit.license, credit.source];
            wanted.extend(credit.paper);
            for text in wanted {
                assert!(readme.contains(text), "README에 없다: {text}");
            }
        }
    }
}
