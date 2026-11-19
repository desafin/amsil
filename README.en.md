# Amsil (암실)

[한국어](README.md) | **English**

Batch-develops camera RAW photos and exports them as compact HEIF files.

*Amsil (암실) is Korean for "darkroom". The interface is available in Korean and English.*

![The Amsil workspace: photo list on the left, preview in the middle, presets and adjustment panels on the right](assets/screenshots/workspace.en.jpg)

## What it does

Take a lot of photos and the RAW files fill your disk fast. Amsil takes a whole folder of RAW files, develops them, and turns them into HEIF (10-bit HEVC).

- **Small files.** 309 photos at 24 megapixels (11 GB of RAW) come out as 1.1 GB. That is 40–45% smaller than JPEG at the same quality.
- **Fast.** Those 309 photos convert in 4 min 30 s (on a 16-core laptop). It uses every CPU core, and when memory runs short it works on fewer photos at once by itself.
- **Easy.** One click on a preset and the photo is done. It can also tell whether a photo is food, a portrait or a landscape, and pick the preset that fits.
- **Your originals are never touched.** Edits are stored in a single project file. Nothing is written to the RAW files or the photo folders.

A hobby project by one person. Tested with RAW files from a Sony DSLR-A900.

## Features

| Feature | Description |
|---|---|
| Whole-folder conversion | Imports and exports a folder, subfolders included. Shows a progress bar, the time left, and CPU, memory and GPU use |
| Presets | Food, Portrait, Landscape, Night, Black & white. Adjust the strength, or set exposure, contrast, temperature, saturation and more by hand |
| Before/after | Shows the unedited RAW and the edited photo side by side. Zoom or pan one and the other follows |
| Preset suggestions (AI) | Recognises what a photo shows and marks the matching preset with ★. Can apply them to many photos at once |
| Noise reduction (AI) | Removes noise from high-ISO photos. Runs on the GPU if there is one, otherwise on the CPU |
| Masks (AI) | Click an object in the photo, such as the sky, a building or a person, to select it, then adjust only that area |
| Skin retouch (AI, experimental) | Finds only the skin in portraits and reduces blotches and uneven tone |
| LUT | Applies a 3D LUT (`.cube`) file |
| Frame | Adds a border around the photo with the camera model, shooting settings and date. Can pad to social media ratios (1:1, 4:5, 9:16) |
| Command line | Converts folders without a window. Same executable |
| Interface | Korean / English, light / dark mode |

## AI features and their sources

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
- **What Amsil does with it**: compares the photo with sentences such as "a photo of food" to guess whether it shows food, a portrait, a landscape, a night scene, architecture or something else. The sentence side was computed in advance and is stored in the repository ([`assets/clip/categories.txt`](assets/clip/categories.txt), made with [`scripts/clip-text-embeddings.py`](scripts/clip-text-embeddings.py)). Runs on the CPU (0.02 s per photo).

### Skin retouch: MediaPipe selfie multiclass 256×256

- **Made by**: Google. Model description <https://ai.google.dev/edge/mediapipe/solutions/vision/image_segmenter>
- **Licence**: Apache-2.0.
- **Downloaded from**: a pinned commit of [`senty-au/selfie_multiclass_256x256-ONNX`](https://huggingface.co/senty-au/selfie_multiclass_256x256-ONNX) on Hugging Face. This is Google's model (TFLite) converted to ONNX by a third party, so it was compared on 25 photos against the original Google distributes and confirmed to give the same results.
- **What Amsil does with it**: only finds where the skin (face and body) is. The retouch itself is done with blurs, not a neural network. Runs on the CPU (0.03 s).

## Installation

### Download

There is no need to build from source. Download `amsil-<version>-linux-x86_64.tar.gz` from the [releases page](https://github.com/desafin/amsil/releases), extract it, and run it.

```
tar xf amsil-*-linux-x86_64.tar.gz
cd amsil-*-linux-x86_64
./fetch-models.sh     # download the AI models (once, about 420 MB)
./amsil
```

- For 64-bit Linux. Works on Ubuntu 24.04 or later, or distributions of a similar age. Windows and macOS are not supported yet.
- The libraries it needs (the HEVC encoder and others) are included in the archive. Just keep the files in the folder together.
- The program works without the AI models; only the features whose model is missing are switched off. `fetch-models.sh` needs `curl` and `unzip`.
- To run noise reduction on the GPU you need a Vulkan driver (the proprietary driver for NVIDIA, `mesa-vulkan-drivers` for AMD and Intel). Without one it runs on the CPU, and the program tells you it will be slow.

### Building from source

See [BUILD.md](BUILD.md) (in Korean).

## Usage

### Getting started

1. Run `./amsil` and create a project file (`name.amsil`) with **[New project]**.
2. Pick a folder of RAW files with **[Import folder]**. The photo list appears on the left.
3. Click a photo to preview it in the middle. On the right, click a **preset**.
4. At the bottom, set the **[Output folder…]** and click **[Export all]**.

That's all. Use the other features when you need them. Edits are saved to the project file as you make them, so there is nothing to save by hand.

### Layout

The picture at the top shows the workspace.

| Area | Contents |
|---|---|
| Top bar | Project name, [Import folder], [Screenshot], [Close project], AI backend (`GPU`) and [AI info], [Open log], light/dark mode, language |
| Left | The imported photos. Next to each name is the kind of photo it is (landscape, food, and so on) |
| Middle | Preview of the selected photo. Edits show up immediately. Turn on [Compare] above it to see the unedited photo alongside |
| Right | Presets, masks, skin retouch, frame, noise reduction |
| Bottom bar | Output folder, quality, [Export selected], [Export all] |
| Very bottom | Notices, and during export the resource use, progress bar and time left |

When the window is narrow, the buttons in the top and bottom bars wrap onto the next line. When it is tall, the right panel moves below the preview.

### Controls

| To | Do this |
|---|---|
| Select several photos | Hold Ctrl or Shift and click. Ctrl+A selects all |
| Go to the previous or next photo | Up and down arrow keys |
| Zoom the preview | Mouse wheel (centred on the cursor, up to 400%) |
| Pan the preview | Drag |
| Switch between fit-to-window and 100% | Double-click the preview |
| See before and after side by side | [Compare] above the preview. Zoom or pan one side and the other follows. Choose the split with [Auto], [Side by side] or [Top and bottom] |
| Save the window as an image | [Screenshot] or F12 (saved in a `screenshots` folder next to the project file) |
| Change the language or light/dark mode | [한국어] [English] and [Light] [Dark] at the top |

Edits apply to **all selected photos**. Masks are the exception: they apply only to the photo you are looking at.

### Presets and fine adjustments

- Clicking a preset applies it right away. **Strength** sets how strong it is (0–150%).
- Open **[Details]** to set exposure, contrast, highlights, shadows, temperature, tint, saturation, vibrance, clarity, sharpen, vignette and monochrome by hand. **[By colour]** has saturation and luminance for eight colours, from red to magenta.
- **[Reset adjustments]** clears the preset, fine adjustments, LUT and skin retouch. Noise reduction, the frame and masks stay.

**Preset suggestions.** When you import photos, each one is classified as food, portrait, landscape, night, architecture or other. Only when the confidence is 60% or more is the kind written next to the photo's name, and the matching preset button gets a ★. **[Apply recommended presets]** gives each selected photo its matching preset in one go. Architecture and other have no matching preset, so nothing is suggested for them. A suggestion is only a suggestion: after applying it you can still change or remove it.

**Making your own preset.** A preset is a single TOML file. Put it in `~/.config/amsil/presets/` and it appears after the built-in presets.

```toml
name = "Warm afternoon"
category = "landscape"   # suggested for this kind of photo (optional)

[params]
temp = 0.2          # temperature: + is warmer
contrast = 0.1
vibrance = 0.2

[params.hsl]
orange = { sat = 0.1 }
```

`name` is shown in both languages. To show a different name in the English interface, add `name_en`; `name` then becomes the Korean name.

Available keys: `exposure` (EV), `contrast`, `highlights`, `shadows`, `temp`, `tint`, `saturation`, `vibrance`, `clarity`, `sharpen`, `vignette` (-1 to 1), `mono` (0 to 1), and under `hsl`, `sat` and `lum` for each colour band (`red`, `orange`, `yellow`, `green`, `aqua`, `blue`, `purple`, `magenta`). For all of them, 0 means "no change". The built-in presets are in [`presets/`](presets) in the repository.

### LUT

Pick a 3D LUT (`.cube`) file with **[Load LUT…]** and it is applied after the other adjustments. **LUT strength** sets how much it blends with the unLUTed image.

- It is applied to the finished sRGB image, so "look" LUTs made for sRGB (Rec.709) displays are the right kind. Conversion LUTs for log footage (S-Log → Rec.709 and the like) are not.
- The project file stores only the LUT file's location. If you move or delete the file, photos that use that LUT are marked as failed on export.
- 1D LUTs are not supported.

### Masks

Adjust part of a photo on its own.

1. Click **[New mask]**.
2. Click what you want to select in the photo (the sky, a building, a person, …). The object is selected and shown in red.
3. Right-click areas you want to leave out. **[Undo point]** removes the last click.
4. Set the exposure, contrast, temperature, tint and saturation for just the selected area. Turn on **[Invert]** to adjust everything outside it instead.

- A photo can have several masks.
- The first click on each photo takes about a second. After that it responds immediately.
- While you are editing a mask, and while [Compare] is on, the frame is left out of the preview (so your clicks land in the right place and both halves are the same size). It is still included on export.

### Skin retouch (experimental)

Raise the amount and Amsil finds the skin (face and body) in portraits by itself, and reduces blotches and uneven tone within it.

- It does not redraw faces or change their shape. Fine texture (pores, fine hair) and broad shading stay; only the blotches in between are reduced. Clear outlines such as eyes, lips and hair are left alone.
- Works best on close-up portraits. It may miss with people far away, or with things like light-coloured wigs.

### Frame

Adds a margin around the photo and writes the shooting info in two lines below it.

```
Sony DSLR-A900
70mm  f/11  1/200s  ISO 200  ·  2026.11.10
```

| Layout | What it does |
|---|---|
| Bottom band | Adds only a band below the photo |
| Border | A thin border at the top and sides, and a wider margin at the bottom for the text |
| 1:1, 4:5, 9:16 | Adds a border, then pads it out to that ratio (so social media does not crop it) |

- Choose white or black.
- Values the photo does not record (such as the lens name) are left out.
- Padding to a ratio makes the image bigger (a landscape photo padded to 9:16 has nearly three times the pixels), and it takes that much more memory.

### Noise reduction

Set the level and click **[Apply]** to store it on the selected photos.

- Removes almost all the noise from high-ISO photos. In exchange, fine detail gets a little softer, so for low-ISO photos with little noise it is better to turn it off or lower the level.
- If you see `GPU` at the top, it is running on the GPU. If you see `CPU (slow)`, click it to see how to fix that.

### Quality

The default of 55 is recommended; it looks about as good as JPEG at 90. Here are the file sizes for two 24-megapixel photos (the camera's own JPEGs were 8.1 MB and 7.4 MB):

| Quality | Clean photo | Noisy photo |
|---|---|---|
| 55 | 3.6 MB | 7.1 MB |
| 65 | 6.8 MB | 11.1 MB |
| 75 | 10.6 MB | 15.1 MB |
| 85 | 13.7 MB | Over 16 MB, lowered to 75 |
| 95–100 | 15.6 MB | Lowered to 70–75 |

- Some viewers (those based on libde265) cannot open a file whose image data is over 16 MB. So when it is over, Amsil encodes the photo again at a quality 10 lower, and tells you it did. Each step down is one more encode, so it takes that much longer.
- 95 and above all give the same result.

### Command line

```
amsil input.ARW output.heic     # one photo
amsil photos out                # a whole folder (subfolders included)
```

- The subfolder structure is recreated in the output folder (`photos/a/1.ARW` → `out/a/1.heic`).
- If some files cannot be converted, the rest still go ahead, and at the end it lists the failed files and why.
- Output files that already exist are skipped. If it stopped partway, run the same command again to carry on.

| Option | What it does |
|---|---|
| `--preset=NAME` | Applies a preset: `food`, `portrait`, `landscape`, `night`, `mono`, or one of your own. `auto` picks the one that suits each photo |
| `--strength=X` | Preset strength (0 to 1.5, default 1) |
| `--denoise[=LEVEL]` | AI noise reduction (0 to 1; 1 when no level is given) |
| `--lut=FILE` | Applies a 3D LUT (`.cube`) |
| `--lut-strength=X` | LUT strength (0 to 1, default 1) |
| `--frame=LAYOUT` | Frame with the shooting info: `band` (strip below), `border`, or a ratio such as `4:5` |
| `--frame-black` | Black frame (white by default) |
| `--skin[=AMOUNT]` | Evens out skin in portraits (experimental; 0 to 1, 0.5 when no amount is given) |
| `--quality=N` | Output quality (40 to 100, default 55) |
| `--overwrite` | Redoes output files that already exist |
| `--jobs=N` | Photos to process at once (default: number of CPU cores) |
| `--memory=GB` | Memory limit (default: 70% of the available memory) |

Masks can only be made in the window.

## Output files

- HEVC 4:2:0 10-bit, sRGB. The extension is `.heic`.
- Shooting info (EXIF) is carried over, and photos taken in portrait orientation are saved upright.

## Open-source components

| Job | Name | Licence |
|---|---|---|
| Reading RAW, EXIF | [rawler](https://github.com/dnglab/dnglab) | LGPL-2.1 |
| Writing HEIF | [libheif](https://github.com/strukturag/libheif) | LGPL-3.0 |
| HEVC encoder | [x265](https://www.x265.org) | GPL-2.0 or later |
| HEVC decoder | [libde265](https://github.com/strukturag/libde265) | LGPL-3.0 |
| Running AI models | [ONNX Runtime](https://onnxruntime.ai) ([ort](https://ort.pyke.io)) | MIT |
| Interface | [egui](https://github.com/emilk/egui) | MIT / Apache-2.0 |
| Font | [Nanum Gothic](https://hangeul.naver.com/font) | SIL OFL 1.1 |

The full list of what ships in the release archive is in [THIRD-PARTY.md](THIRD-PARTY.md) (in Korean).

## Where files go

| What | Where |
|---|---|
| Project | A single `name.amsil` file, wherever you put it |
| Settings (language, light/dark mode, recent projects) | `~/.config/amsil/config.toml` |
| Your own presets | `~/.config/amsil/presets/*.toml` |
| AI models | `~/.local/share/amsil/models/` (change it with `AMSIL_MODELS`) |
| Log | `~/.local/state/amsil/log.txt` |

**Log.** So that you can find out what went wrong when something does, Amsil records what happens in one file. Click **[Open log]** in the window to open it. The command line writes to the same file. The log is always in English, whatever language the interface is in, so it is easy to share and to search for an error message.

```
2026-11-18 15:19:49 [info] Amsil 0.2.0 started: photos out
2026-11-18 15:19:50 [info] Export started: 3 photos (3 at a time, quality 55)
2026-11-18 15:19:52 [error] Failed: photos/broken.ARW: Unsupported or damaged RAW file (…)
2026-11-18 15:19:58 [info] Export finished: done 2, skipped 0, failed 1 (8.1 s)
```

- Once it is over 1 MB, the next start moves it to `log.old.txt` and begins a new one.
- It contains the paths of your photo files. Look through it before you send it to anyone.

## Good to know

- Tested only with ARW files from a Sony DSLR-A900. RAW reading is done by rawler, so other Bayer-sensor cameras that rawler supports can be read as well, but they have not been tested. Colours are tuned to match the A900's in-camera JPEGs.
- Linux only.
- If you have never picked a language, the interface follows the system language (Korean for a Korean locale, English otherwise).

## Licence

GPL-3.0-or-later. See [LICENSE](LICENSE).

The bundled font, Nanum Gothic, is under the SIL Open Font License ([assets/fonts/OFL.txt](assets/fonts/OFL.txt)).
