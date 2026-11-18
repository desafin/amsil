#!/usr/bin/env python3
"""사진 분류에 쓰는 문장 임베딩 표(assets/clip/categories.txt)를 만든다. 개발할 때 한 번만 돌린다.

프로그램은 사진을 CLIP 이미지 인코더에 넣어 얻은 벡터를 이 표의 벡터들과 견주어 분류한다.
그래서 실행할 때는 토크나이저와 텍스트 인코더가 필요 없다. 분류나 문장을 바꾸면 다시 만든다.

준비 (모델은 Hugging Face Xenova/clip-vit-base-patch32, 커밋 6b7c523703ae26e39be4b03774966f8571c7f523):

    python3 -m venv venv && venv/bin/pip install onnxruntime tokenizers numpy
    base=https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/6b7c523703ae26e39be4b03774966f8571c7f523
    mkdir clip && curl -fL -o clip/text_model.onnx $base/onnx/text_model.onnx -o clip/tokenizer.json $base/tokenizer.json
    echo "3f6571f5bad13a97c469c1622e1cfc4d9aef78b79fdbfcff804ca357bfada8cc  clip/text_model.onnx" | sha256sum -c -

실행:

    venv/bin/python scripts/clip-text-embeddings.py clip > assets/clip/categories.txt
"""
import os
import sys

import numpy as np
import onnxruntime
from tokenizers import Tokenizer

# 분류와 그 분류를 설명하는 문장들. 순서는 src/recommend.rs의 Category::ALL과 같아야 한다.
# 문장 여러 개의 임베딩을 평균하면 한 문장일 때보다 덜 흔들린다.
CATEGORIES = {
    "food": [
        "a photo of food",
        "a photo of a meal on a table",
        "a close-up photo of a dish",
        "a photo of a dessert or a drink",
    ],
    "portrait": [
        "a portrait photo of a person",
        "a photo of a person's face",
        "a full-body photo of a person standing",
        "a photo of a person posing for the camera",
        "a photo of people",
    ],
    "landscape": [
        "a landscape photo of nature",
        "a photo of mountains, forest or the sea",
        "a photo of trees and the sky",
        "a photo of a park or a garden",
    ],
    "night": [
        "a photo taken outdoors at night",
        "a night view of a city with lights",
        "a photo of the night sky",
    ],
    "architecture": [
        "a photo of a building",
        "a photo of city architecture",
        "a street photo of a city",
        "a photo of a traditional palace or temple building",
    ],
    "other": [
        "a photo of an object on a table",
        "a photo of an animal",
        "a photo of a document or a sign with text",
        "a photo of a screen",
    ],
}
CONTEXT = 77  # CLIP 텍스트 인코더의 입력 길이
END = 49407  # <|endoftext|>. 남는 자리도 이것으로 채운다.


def main(models):
    tokenizer = Tokenizer.from_file(os.path.join(models, "tokenizer.json"))
    session = onnxruntime.InferenceSession(
        os.path.join(models, "text_model.onnx"), providers=["CPUExecutionProvider"]
    )
    inputs = [i.name for i in session.get_inputs()]
    outputs = [o.name for o in session.get_outputs()]

    print("# CLIP ViT-B/32 문장 임베딩. scripts/clip-text-embeddings.py가 만든 파일이다. 손으로 고치지 않는다.")
    print("# 한 줄에 분류 이름과 길이 1인 512차원 벡터.")
    for category, prompts in CATEGORIES.items():
        vectors = []
        for prompt in prompts:
            ids = tokenizer.encode(prompt).ids
            assert len(ids) <= CONTEXT, prompt
            feed = {"input_ids": np.array([ids + [END] * (CONTEXT - len(ids))], np.int64)}
            if "attention_mask" in inputs:
                mask = [1] * len(ids) + [0] * (CONTEXT - len(ids))
                feed["attention_mask"] = np.array([mask], np.int64)
            embedding = dict(zip(outputs, session.run(None, feed)))["text_embeds"][0]
            vectors.append(embedding / np.linalg.norm(embedding))
        mean = np.mean(vectors, axis=0)
        mean /= np.linalg.norm(mean)
        print(category, " ".join(f"{value:.6f}" for value in mean))


if __name__ == "__main__":
    main(sys.argv[1])
