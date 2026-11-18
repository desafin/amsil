#!/bin/sh
# AI 모델을 받아 모델 폴더에 푼다. 출처(릴리스 태그)와 SHA-256은 고정되어 있다.
# 필요한 도구: curl, sha256sum, unzip
set -eu

dir="${AMSIL_MODELS:-${XDG_DATA_HOME:-$HOME/.local/share}/amsil/models}"
base="https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0"

fetch() { # 이름 SHA-256
    name=$1
    sum=$2
    if [ -d "$dir/$name" ]; then
        echo "$name: 이미 있음 ($dir/$name)"
        return
    fi
    tmp=$(mktemp -d)
    curl -fL --progress-bar -o "$tmp/model.zip" "$base/$name.dtmodel"
    if ! echo "$sum  $tmp/model.zip" | sha256sum -c - >/dev/null 2>&1; then
        rm -rf "$tmp"
        echo "$name: 받은 파일의 체크섬이 다릅니다. 설치하지 않습니다." >&2
        exit 1
    fi
    unzip -q "$tmp/model.zip" -d "$tmp/unpacked"
    mkdir -p "$dir"
    mv "$tmp/unpacked/$name" "$dir/$name"
    rm -rf "$tmp"
    echo "$name: 설치함 ($dir/$name)"
}

# 노이즈 제거: RawNIND UtNet2 (GPL-3.0)
fetch rawdenoise-nind d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af
