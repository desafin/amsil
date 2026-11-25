//! Presets: named bundles of adjustment values. One TOML file is one preset.

use crate::Result;
use crate::edit::Params;
use crate::lang::{t, text};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// Identifier written in the project file (the file name without its extension).
    #[serde(skip)]
    pub id: String,
    /// Name shown on screen.
    pub name: String,
    /// Name shown on the English screen. Falls back to `name` if absent.
    #[serde(default)]
    pub name_en: Option<String>,
    /// Category used for automatic suggestions (food, portrait, landscape, night, etc.).
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub params: Params,
}

impl Preset {
    /// Name in the current language.
    pub fn label(&self) -> &str {
        t(&self.name, self.name_en.as_ref().unwrap_or(&self.name))
    }
}

/// Default presets built into the program.
pub fn builtin() -> Vec<Preset> {
    const FILES: [(&str, &str); 5] = [
        ("food", include_str!("../presets/food.toml")),
        ("portrait", include_str!("../presets/portrait.toml")),
        ("landscape", include_str!("../presets/landscape.toml")),
        ("night", include_str!("../presets/night.toml")),
        ("mono", include_str!("../presets/mono.toml")),
    ];
    FILES
        .iter()
        .map(|(id, text)| parse(id, text).expect("a built-in preset file is invalid"))
        .collect()
}

/// Reads the `*.toml` files in a folder as presets. An empty list if the folder is missing. Files
/// that could not be read are returned separately with the reason.
pub fn load_dir(dir: &Path) -> (Vec<Preset>, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut files: Vec<_> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();
    let (mut presets, mut problems) = (Vec::new(), Vec::new());
    for path in files {
        let id = path.file_stem().unwrap_or_default().to_string_lossy();
        match std::fs::read_to_string(&path)
            .map_err(Into::into)
            .and_then(|text| parse(&id, &text))
        {
            Ok(preset) => presets.push(preset),
            Err(error) => problems.push(error.to_string()),
        }
    }
    (presets, problems)
}

/// Reads one preset from TOML text.
pub fn parse(id: &str, text: &str) -> Result<Preset> {
    let mut preset: Preset = toml::from_str(text).map_err(|e| {
        text!(
            format!("프리셋 {id}을(를) 읽을 수 없습니다: {e}"),
            format!("Cannot read preset {id}: {e}"),
        )
    })?;
    preset.id = id.to_owned();
    Ok(preset)
}

/// The final adjustment values to apply to a photo: preset × strength + manual adjustments.
/// If the preset is not found, only the manual adjustments are applied.
pub fn resolve(presets: &[Preset], preset: Option<&str>, strength: f32, adjust: &Params) -> Params {
    match preset.and_then(|id| presets.iter().find(|p| p.id == id)) {
        Some(preset) => preset.params.scaled(strength).plus(adjust),
        None => *adjust,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_are_the_five_looks() {
        let presets = builtin();
        let names: Vec<&str> = presets.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["음식", "인물", "풍경", "야경", "흑백"]);
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["food", "portrait", "landscape", "night", "mono"]);
        assert!(
            presets.iter().all(|p| !p.params.is_neutral()),
            "no preset is without effect"
        );
        assert_eq!(presets[4].params.mono, 1.0);
        assert_eq!(presets[0].category.as_deref(), Some("food"));
    }

    #[test]
    fn a_typo_in_a_preset_file_is_reported_with_the_file_name() {
        let error = parse("mine", "name = \"내 것\"\n[params]\ncontrsat = 0.2\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("mine") && error.contains("contrsat"),
            "{error}"
        );
    }

    #[test]
    fn user_presets_are_read_from_a_folder_and_bad_files_are_listed() {
        let dir = std::env::temp_dir().join(format!("amsil-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("b-warm.toml"),
            "name = \"따뜻하게\"\n[params]\ntemp = 0.3\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("a-cool.toml"),
            "name = \"차갑게\"\n[params]\ntemp = -0.3\n",
        )
        .unwrap();
        std::fs::write(dir.join("broken.toml"), "name = 3\n").unwrap();
        std::fs::write(dir.join("메모.txt"), "프리셋이 아님").unwrap();

        let (presets, problems) = load_dir(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["a-cool", "b-warm"], "file name order");
        assert_eq!(presets[1].params.temp, 0.3);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("broken"), "{problems:?}");

        assert_eq!(load_dir(Path::new("/없는/폴더")), (vec![], vec![]));
    }

    #[test]
    fn final_params_are_preset_times_strength_plus_manual_adjustment() {
        let presets = builtin();
        let manual = Params {
            exposure: 0.3,
            vibrance: -0.1,
            ..Params::default()
        };
        let food = &presets[0].params;

        let full = resolve(&presets, Some("food"), 1.0, &manual);
        assert_eq!(full.exposure, 0.3);
        assert!((full.vibrance - (food.vibrance - 0.1)).abs() < 1e-6);
        assert_eq!(full.temp, food.temp);

        let half = resolve(&presets, Some("food"), 0.5, &Params::default());
        assert_eq!(half.temp, food.temp * 0.5);

        assert_eq!(
            resolve(&presets, None, 1.0, &manual),
            manual,
            "without a preset, only the manual adjustments"
        );
        assert_eq!(
            resolve(&presets, Some("지워진 프리셋"), 1.0, &manual),
            manual
        );
    }

    #[test]
    fn presets_are_labelled_in_the_current_language() {
        let builtin = builtin();
        let labels = |presets: &[Preset]| -> Vec<String> {
            presets
                .iter()
                .map(|preset| preset.label().to_owned())
                .collect()
        };
        assert_eq!(labels(&builtin), ["음식", "인물", "풍경", "야경", "흑백"]);
        // A user preset without an English name keeps its original name.
        let mine = parse("mine", "name = \"따뜻한 오후\"").unwrap();
        let named = parse(
            "named",
            "name = \"따뜻한 오후\"\nname_en = \"Warm afternoon\"",
        )
        .unwrap();
        crate::lang::set(crate::lang::Lang::En);
        assert_eq!(
            labels(&builtin),
            ["Food", "Portrait", "Landscape", "Night", "Black & white"]
        );
        assert_eq!(mine.label(), "따뜻한 오후");
        assert_eq!(named.label(), "Warm afternoon");
    }
}
