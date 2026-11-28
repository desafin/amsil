# User guide

[한국어](../ko/guide.md)

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
| Right | Presets, masks, skin retouch, frame, capture sharpening, noise reduction |
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
| See before and after side by side | [Compare] above the preview. Zoom or pan one side and the other follows. Choose the split with [Auto], [Side by side] or [Top and bottom], and what the left pane shows with [Unedited RAW] or [Camera JPEG] |
| Save the window as an image | [Screenshot] or F12 (saved in a `screenshots` folder next to the project file) |
| Change the language or light/dark mode | [한국어] [English] and [Light] [Dark] at the top |

Edits apply to **all selected photos**. Masks are the exception: they apply only to the photo you are looking at.

### Presets and fine adjustments

- Clicking a preset applies it right away. **Strength** sets how strong it is (0–150%).
- Open **[Details]** to set exposure, contrast, highlights, shadows, temperature, tint, saturation, vibrance, clarity, sharpen, vignette and monochrome by hand. **[By colour]** has saturation and luminance for eight colours, from red to magenta.
- **[Reset adjustments]** clears the preset, fine adjustments, LUT and skin retouch. Noise reduction, the frame, masks and capture sharpening stay.

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

Available keys: `exposure` (EV), `contrast`, `highlights`, `shadows`, `temp`, `tint`, `saturation`, `vibrance`, `clarity`, `sharpen`, `vignette` (-1 to 1), `mono` (0 to 1), and under `hsl`, `sat` and `lum` for each colour band (`red`, `orange`, `yellow`, `green`, `aqua`, `blue`, `purple`, `magenta`). For all of them, 0 means "no change". The built-in presets are in [`presets/`](../../presets) in the repository.

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

### Capture sharpening

A RAW file is softer than the camera's JPEG to begin with. The lens and the filter in front of the sensor (the low-pass filter that prevents moiré) spread the light a little, and the camera undoes that with sharpening when it makes its JPEG. Amsil does the same when it reads the RAW. It is RawTherapee's Capture Sharpening, ported: it undoes the blur in the luminance (Richardson–Lucy deconvolution) and blends it in only where there are clear edges. Flat areas such as sky and walls, and the surroundings of blown-out highlights, are left alone.

- It is on for every photo by default. The defaults (20 iterations, contrast threshold 0.4) were chosen by comparing five photos with the camera's JPEGs: after compression at quality 55, detail is about the same as in the camera's JPEG (about 0.7× with it off), and files grow by about 10%.
- Change it in the **Capture sharpening** group on the right. It applies to all selected photos.
  - **Iterations** (1–100): more iterations undo more of the blur.
  - **[More settings]**: radius (the size of the blur to undo, worked out from the photo by default), contrast threshold (higher sharpens only clear edges; on auto it is worked out from the photo, which comes out about 1.3× stronger than the camera's JPEG and makes files bigger), corner radius (adds to the radius towards the corners, for lenses that are softer there), and stop when it overshoots.
- **Show sharpened area**: shows where capture sharpening applies, as a [Red overlay] on the photo or as a [Black-and-white map] (white is sharpened). The frame is left out while you look.
- Judge sharpness at 100% zoom. Choose [Camera JPEG] in the before/after comparison to see the camera's JPEG with the same name alongside.
- [Reset adjustments] leaves capture sharpening alone.
- Projects made with a version that did not have this feature open with capture sharpening on. Exported again, they come out sharper and a little larger.

### Noise reduction

Set the level and click **[Apply]** to store it on the selected photos.

- Removes almost all the noise from high-ISO photos. In exchange, fine detail gets a little softer, so for low-ISO photos with little noise it is better to turn it off or lower the level.
- If you see `GPU` at the top, it is running on the GPU. If you see `CPU (slow)`, click it to see how to fix that.

### Quality

The default of 55 is recommended; it looks about as good as JPEG at 90. Here are the file sizes for two 24-megapixel photos (the camera's own JPEGs were 8.1 MB and 7.4 MB). They were measured before capture sharpening was added; the default capture sharpening makes files about 10% larger.

| Quality | Clean photo | Noisy photo |
|---|---|---|
| 55 | 3.6 MB | 7.1 MB |
| 65 | 6.8 MB | 11.1 MB |
| 75 | 10.6 MB | 15.1 MB |
| 85 | 13.7 MB | Over 16 MB, lowered to 75 |
| 95–100 | 15.6 MB | Lowered to 70–75 |

- Some viewers (those based on libde265) cannot open a file whose image data is over 16 MB. So when it is over, Amsil encodes the photo again at a quality 10 lower, and tells you it did. Each step down is one more encode, so it takes that much longer.
- 95 and above all give the same result.

## Output files

- HEVC 4:2:0 10-bit, sRGB. The extension is `.heic`.
- Shooting info (EXIF) is carried over, and photos taken in portrait orientation are saved upright.

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
