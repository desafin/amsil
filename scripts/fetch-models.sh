#!/bin/sh
# Downloads the AI models into the model folder. Sources (release tag or commit) and SHA-256 sums are pinned.
# Requires: curl, sha256sum, unzip
set -eu

dir="${AMSIL_MODELS:-${XDG_DATA_HOME:-$HOME/.local/share}/amsil/models}"

# Verify the downloaded file's checksum; on mismatch, delete it and stop.
verify() { # file sha256 name
    if ! echo "$2  $1" | sha256sum -c - >/dev/null 2>&1; then
        rm -rf "$(dirname "$1")"
        echo "$3: checksum mismatch, not installed." >&2
        exit 1
    fi
}

# Download and unpack a model bundle (zip) from a darktable-ai release.
fetch_dtmodel() { # name sha256
    name=$1
    if [ -d "$dir/$name" ]; then
        echo "$name: already installed ($dir/$name)"
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
    echo "$name: installed ($dir/$name)"
}

# Download a single file into a subfolder of the model folder.
fetch_file() { # folder file url sha256
    if [ -f "$dir/$1/$2" ]; then
        echo "$1: already installed ($dir/$1/$2)"
        return
    fi
    tmp=$(mktemp -d)
    curl -fL --progress-bar -o "$tmp/$2" "$3"
    verify "$tmp/$2" "$4" "$1"
    mkdir -p "$dir/$1"
    mv "$tmp/$2" "$dir/$1/$2"
    rm -rf "$tmp"
    echo "$1: installed ($dir/$1/$2)"
}

# Denoise: RawNIND UtNet2 (GPL-3.0, 58MB)
fetch_dtmodel rawdenoise-nind d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af

# Masking: Segment Anything 2.1 Hiera Small (Apache-2.0, 170MB)
fetch_dtmodel mask-object-sam21-small 06a629ac1adf40daec47d2191cbdbdac609d93769584a05f65f4276bf784663f

# Photo classification (preset suggestions): OpenAI CLIP ViT-B/32 image encoder, fp16 (MIT, 176MB)
fetch_file clip-vit-b32 vision_model_fp16.onnx \
    "https://huggingface.co/Xenova/clip-vit-base-patch32/resolve/6b7c523703ae26e39be4b03774966f8571c7f523/onnx/vision_model_fp16.onnx" \
    35c4e0fb0aeee527dcde1693520b214a34424a786babd530f35366bad5844efd

# Skin detection (skin retouching): MediaPipe selfie multiclass 256x256 converted to ONNX (Apache-2.0, 16MB).
# Verified to produce the same output as Google's original TFLite model.
fetch_file selfie-multiclass model.onnx \
    "https://huggingface.co/senty-au/selfie_multiclass_256x256-ONNX/resolve/6db8421a7150ac20558f2c24675078eb3a1a04d0/onnx/model.onnx" \
    35ec1ecd9ee7f85073c99c00020b7f6751b69506eeacf683bc8665f6117f85b0
