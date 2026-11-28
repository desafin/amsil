# Command line

[한국어](../ko/cli.md)

## Usage

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
| `--no-capture` | Skip capture sharpening (on by default) |
| `--capture-iterations=N` | Capture sharpening iterations (1 to 100, default 20) |
| `--capture-radius=R` | Radius of the blur to undo (0.4 to 2; worked out from the photo if not given) |
| `--capture-threshold=T` | Capture sharpening contrast threshold (0 to 2, default 0.4; `auto` works it out from the photo) |
| `--capture-corner=X` | Radius added towards the corners (-0.5 to 0.5, default 0) |
| `--capture-no-auto-stop` | Keep iterating even when overshoot appears |

Masks can only be made in the window.
