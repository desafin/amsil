# AI models and sources

[한국어](../ko/ai-models.md)

## Overview

All four AI features use **models that other people made and published, as they are**. Amsil has not trained any model of its own. Below is who made each model and where it is downloaded from. In the program, click **[AI info]** at the top to see the same information; each feature's panel also names its model.

| Feature | Model | Made by | Licence | Size |
|---|---|---|---|---|
| Noise reduction | RawNIND UtNet2 | Benoit Brummer (UCLouvain) | GPL-3.0 | 58 MB |
| Masks | Segment Anything 2.1 (Hiera Small) | Meta | Apache-2.0 | 170 MB |
| Preset suggestions | CLIP ViT-B/32 | OpenAI | MIT | 176 MB |
| Skin retouch | MediaPipe selfie multiclass 256×256 | Google | Apache-2.0 | 16 MB |

- The models run **only on your computer**. Your photos never leave it.
- The model files are not in the repository or the release archive. `fetch-models.sh` downloads them from the fixed locations listed below, checks their SHA-256, and puts them in `~/.local/share/amsil/models`. If a checksum does not match, the file is not installed.
- If a model is missing, only that feature is switched off. Everything else works as usual.

### Noise reduction: RawNIND UtNet2

- **Made by**: Benoit Brummer (UCLouvain). Repository <https://github.com/trougnouf/rawnind_jddc>, paper <https://arxiv.org/abs/2501.08924>
- **Licence**: GPL-3.0. Trained on pairs of noisy and clean photos taken with real cameras (RawNIND, CC BY 4.0 / CC0).
- **Downloaded from**: `rawdenoise-nind` in [darktable-ai](https://github.com/darktable-org/darktable-ai) release `release-5.6.0`. It is the same file darktable 5.6 uses.
- **What Amsil does with it**: splits the sensor data into 512-pixel tiles, runs them through the model, and blends the result with the regular development by the chosen level. Runs on the GPU if there is one.

### Masks: Segment Anything 2.1 (Hiera Small)

- **Made by**: Meta. Repository <https://github.com/facebookresearch/sam2>, paper <https://arxiv.org/abs/2408.00714>
- **Licence**: Apache-2.0. However, SA-1B, part of the training data, has a separate research licence from Meta (SA-V is CC BY 4.0).
- **Downloaded from**: `mask-object-sam21-small` in darktable-ai release `release-5.6.0` (converted to ONNX).
- **What Amsil does with it**: gives the model the point you clicked and gets back the object at that point. The model returns a 256×256 grid; Amsil scales it up to full size, following the edges in the photo. Runs on the CPU.

### Preset suggestions: CLIP ViT-B/32

- **Made by**: OpenAI. Repository <https://github.com/openai/CLIP>, paper <https://arxiv.org/abs/2103.00020>
- **Licence**: MIT.
- **Downloaded from**: a pinned commit of [`Xenova/clip-vit-base-patch32`](https://huggingface.co/Xenova/clip-vit-base-patch32) on Hugging Face. This is OpenAI's model converted to ONNX; only its image encoder (the half-precision version) is used.
- **What Amsil does with it**: compares the photo with sentences such as "a photo of food" to guess whether it shows food, a portrait, a landscape, a night scene, architecture or something else. The sentence side was computed in advance and is stored in the repository ([`assets/clip/categories.txt`](../../assets/clip/categories.txt), made with [`scripts/clip-text-embeddings.py`](../../scripts/clip-text-embeddings.py)). Runs on the CPU (0.02 s per photo).

### Skin retouch: MediaPipe selfie multiclass 256×256

- **Made by**: Google. Model description <https://ai.google.dev/edge/mediapipe/solutions/vision/image_segmenter>
- **Licence**: Apache-2.0.
- **Downloaded from**: a pinned commit of [`senty-au/selfie_multiclass_256x256-ONNX`](https://huggingface.co/senty-au/selfie_multiclass_256x256-ONNX) on Hugging Face. This is Google's model (TFLite) converted to ONNX by a third party, so it was compared on 25 photos against the original Google distributes and confirmed to give the same results.
- **What Amsil does with it**: only finds where the skin (face and body) is. The retouch itself is done with blurs, not a neural network. Runs on the CPU (0.03 s).
