#!/bin/sh
# AI 모델을 받아 모델 폴더에 둔다. 출처(릴리스 태그나 커밋)와 SHA-256은 고정되어 있다.
# Downloads the AI models into the model folder. Sources and SHA-256 sums are pinned.
# 필요한 도구 / needs: curl, sha256sum, unzip
set -eu

dir="${AMSIL_MODELS:-${XDG_DATA_HOME:-$HOME/.local/share}/amsil/models}"

# 받은 파일의 체크섬을 확인한다. 다르면 지우고 멈춘다.
verify() { # 파일 SHA-256 이름
    if ! echo "$2  $1" | sha256sum -c - >/dev/null 2>&1; then
        rm -rf "$(dirname "$1")"
        echo "$3: 받은 파일의 체크섬이 다릅니다. 설치하지 않습니다. / checksum mismatch, not installed." >&2
        exit 1
    fi
}

# darktable-ai 릴리스의 모델 묶음(zip)을 받아 푼다.
fetch_dtmodel() { # 이름 SHA-256
    name=$1
    if [ -d "$dir/$name" ]; then
        echo "$name: 이미 있음 / already installed ($dir/$name)"
        return
    fi
    tmp=$(mktemp -d)
    curl -fL --progress-bar -o "$tmp/model.zip" \
        "https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/$name.dtmodel"
    verify "$tmp/model.zip" "$2" "$name"
    unzip -q "$tmp/model.zip" -d "$tmp/unpacked"
    mkdir -p "$dir"
    mv "$tmp/unpacked/$name" "$dir/$name"
    rm -rf "$tmp"
    echo "$name: 설치함 / installed ($dir/$name)"
}

# 파일 하나를 받아 모델 폴더의 하위 폴더에 둔다.
fetch_file() { # 폴더 파일 URL SHA-256
    if [ -f "$dir/$1/$2" ]; then
        echo "$1: 이미 있음 / already installed ($dir/$1/$2)"
        return
    fi
    tmp=$(mktemp -d)
    curl -fL --progress-bar -o "$tmp/$2" "$3"
    verify "$tmp/$2" "$4" "$1"
    mkdir -p "$dir/$1"
    mv "$tmp/$2" "$dir/$1/$2"
    rm -rf "$tmp"
    echo "$1: 설치함 / installed ($dir/$1/$2)"
}

# 노이즈 제거: RawNIND UtNet2 (GPL-3.0, 58MB)
fetch_dtmodel rawdenoise-nind d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af

# 마스크: Segment Anything 2.1 Hiera Small (Apache-2.0, 170MB)
fetch_dtmodel mask-object-sam21-small 06a629ac1adf40daec47d2191cbdbdac609d93769584a05f65f4276bf784663f

# 사진 분류(프리셋 추천): OpenAI CLIP ViT-B/32 이미지 인코더, 반정밀도 (MIT, 176MB)
fetch_file clip-vit-b32 vision_model_fp16.onnx \
    "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/6b7c523703ae26e39be4b03774966f8571c7f523/onnx/vision_model_fp16.onnx" \
    35c4e0fb0aeee527dcde1693520b214a34424a786babd530f35366bad5844efd
