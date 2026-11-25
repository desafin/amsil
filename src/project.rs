//! The project file (`name.amsil`) and the program settings file.
//!
//! The project file holds the imported folders, per-photo edits, and export settings. Nothing is
//! written to the photo folders.

use crate::Result;
use crate::capture::Capture;
use crate::edit::Params;
use crate::frame::Frame;
use crate::lang::{t, text};
use crate::mask::Mask;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Version of the project file format this program reads and writes.
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    version: u32,
    pub name: String,
    #[serde(default)]
    pub export: Export,
    /// Imported folders.
    #[serde(default, rename = "folder")]
    pub folders: Vec<Folder>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Export {
    /// Folder to export to. None if not chosen yet.
    pub folder: Option<PathBuf>,
    pub quality: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub path: PathBuf,
    /// Only photos that differ from the defaults are recorded. Keys are paths relative to `path`.
    #[serde(default, rename = "photo")]
    pub photos: BTreeMap<String, Edit>,
}

/// The edits of one photo. Fields equal to the default are not written to the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Edit {
    /// Noise reduction strength (0–1).
    #[serde(skip_serializing_if = "is_zero")]
    pub denoise: f32,
    /// Identifier of the preset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Strength of the preset. 1 is the preset as is.
    #[serde(skip_serializing_if = "is_one")]
    pub strength: f32,
    /// Manual adjustments added on top of the preset.
    #[serde(skip_serializing_if = "Params::is_neutral")]
    pub adjust: Params,
    /// LUT applied last.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lut: Option<LutRef>,
    /// Frame put around the photo.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<Frame>,
    /// Local adjustment masks.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<Mask>,
    /// Skin retouch strength (0–1).
    #[serde(skip_serializing_if = "is_zero")]
    pub skin: f32,
    /// Capture sharpening (RAW stage). Older files without this field read as the default (on).
    #[serde(skip_serializing_if = "Capture::is_default")]
    pub capture: Capture,
}

/// The LUT file to apply to a photo and its strength.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LutRef {
    pub file: PathBuf,
    /// 1 is the LUT as is, 0 is the original.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub strength: f32,
}

fn one() -> f32 {
    1.0
}

impl Default for Edit {
    fn default() -> Self {
        Self {
            denoise: 0.0,
            preset: None,
            strength: 1.0,
            adjust: Params::default(),
            lut: None,
            frame: None,
            masks: Vec::new(),
            skin: 0.0,
            capture: Capture::default(),
        }
    }
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

fn is_one(value: &f32) -> bool {
    *value == 1.0
}

impl Default for Export {
    fn default() -> Self {
        Self {
            folder: None,
            quality: crate::export::QUALITY,
        }
    }
}

impl Project {
    pub fn new(name: &str) -> Self {
        Self {
            version: VERSION,
            name: name.into(),
            export: Export::default(),
            folders: Vec::new(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        // Check the version first. Don't force-read a file whose format may have changed.
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }
        let header: Header = toml::from_str(&text).map_err(|e| {
            text!(
                format!("프로젝트 파일이 아닙니다 ({e})"),
                format!("Not a project file ({e})"),
            )
        })?;
        if header.version != VERSION {
            let found = header.version;
            return Err(text!(
                format!(
                    "이 프로젝트 파일은 버전 {found}이고, 이 프로그램은 버전 {VERSION}만 읽을 수 있습니다"
                ),
                format!(
                    "This project file is version {found}, and this program can only read version {VERSION}"
                ),
            )
            .into());
        }
        Ok(toml::from_str(&text).map_err(|e| {
            text!(
                format!("프로젝트 파일을 읽을 수 없습니다 ({e})"),
                format!("Cannot read the project file ({e})"),
            )
        })?)
    }

    /// Writes to a temp file, then renames it. The existing file survives even if power is lost
    /// while saving.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomically(path, &toml::to_string(self)?)
    }

    /// The edits of a photo. The default if there is no record.
    pub fn edit(&self, folder: usize, photo: &str) -> Edit {
        self.folders
            .get(folder)
            .and_then(|f| f.photos.get(photo))
            .cloned()
            .unwrap_or_default()
    }

    /// Changes the edits of a photo. Removes the record when it returns to the default.
    pub fn set_edit(&mut self, folder: usize, photo: &str, edit: Edit) {
        let Some(folder) = self.folders.get_mut(folder) else {
            return;
        };
        if edit == Edit::default() {
            folder.photos.remove(photo);
        } else {
            folder.photos.insert(photo.into(), edit);
        }
    }

    /// The name each folder uses inside the output folder. Folders with the same name get a number
    /// to tell them apart.
    pub fn export_names(&self) -> Vec<String> {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        self.folders
            .iter()
            .map(|folder| {
                let name = folder
                    .path
                    .file_name()
                    .map_or(t("사진", "photos").into(), |n| {
                        n.to_string_lossy().into_owned()
                    });
                let count = seen.entry(name.clone()).or_insert(0);
                *count += 1;
                if *count == 1 {
                    name
                } else {
                    format!("{name}-{count}")
                }
            })
            .collect()
    }
}

fn write_atomically(path: &Path, text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Program settings (`~/.config/amsil/config.toml`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Config {
    /// Language chosen on screen. Follows the system language if never chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<crate::lang::Lang>,
    /// Brightness mode chosen on screen. Follows the system setting if never chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    /// Direction the preview is split in the before/after comparison.
    #[serde(default)]
    pub compare_split: Split,
    /// What to show in the before pane of the before/after comparison.
    #[serde(default)]
    pub compare_before: Before,
    /// Recently opened projects. Newest first.
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

/// The screen's brightness mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
}

/// Direction the preview is split in the before/after comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    /// Whichever way the photo fits larger.
    #[default]
    Auto,
    /// Side by side (before on the left).
    Side,
    /// Top and bottom (before on top).
    Stacked,
}

/// What to show in the before pane of the before/after comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Before {
    /// The RAW with no adjustments at all.
    #[default]
    Raw,
    /// The camera JPEG with the same name in the same folder (the RAW if there is none).
    Jpeg,
}

/// Maximum length of the recent projects list.
const MAX_RECENT: usize = 10;

impl Config {
    /// The default if missing or unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomically(path, &toml::to_string(self)?)
    }

    /// Moves the project to the front of the recent list. The list holds up to 10.
    pub fn remember(&mut self, project: &Path) {
        self.recent.retain(|p| p != project);
        self.recent.insert(0, project.to_owned());
        self.recent.truncate(MAX_RECENT);
    }

    /// Location of the settings file.
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_default();
        base.join("amsil/config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("amsil-project-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample() -> Project {
        let mut project = Project::new("가을 여행");
        project.export.folder = Some("/home/user/Pictures/가을여행".into());
        project.folders.push(Folder {
            path: "/home/user/Photos/DCIM".into(),
            photos: BTreeMap::new(),
        });
        project.set_edit(
            0,
            "10161107/DSC04307.ARW",
            Edit {
                denoise: 0.75,
                ..Edit::default()
            },
        );
        project
    }

    #[test]
    fn capture_sharpening_is_on_for_old_projects_and_saved_only_when_changed() {
        // A project file without this field
        let old: Edit = toml::from_str("denoise = 0.5\n").unwrap();
        assert_eq!(old.capture, Capture::default());
        assert!(old.capture.enabled);
        // Defaults are not written to the file
        let text = toml::to_string(&Edit::default()).unwrap();
        assert!(!text.contains("capture"), "{text}");
        let changed = Edit {
            capture: Capture {
                iterations: 35,
                radius: Some(0.8),
                ..Capture::default()
            },
            ..Edit::default()
        };
        let text = toml::to_string(&changed).unwrap();
        assert!(
            text.contains("iterations = 35") && text.contains("radius = 0.8"),
            "{text}"
        );
        let back: Edit = toml::from_str(&text).unwrap();
        assert_eq!(back, changed);
    }

    #[test]
    fn saved_project_loads_back_the_same() {
        let dir = temp("roundtrip");
        let path = dir.join("가을 여행.amsil");
        let project = sample();
        project.save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap(), project);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_is_readable_toml_with_photos_under_their_folder() {
        let dir = temp("format");
        let path = dir.join("p.amsil");
        sample().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(text.starts_with("version = 1\n"), "{text}");
        assert!(text.contains("name = \"가을 여행\""), "{text}");
        assert!(text.contains("[[folder]]"), "{text}");
        assert!(
            text.contains("[folder.photo.\"10161107/DSC04307.ARW\"]"),
            "{text}"
        );
        assert!(text.contains("denoise = 0.75"), "{text}");
    }

    #[test]
    fn saving_replaces_the_old_file_and_leaves_no_temporary_file() {
        let dir = temp("atomic");
        let path = dir.join("p.amsil");
        Project::new("처음").save(&path).unwrap();
        Project::new("나중").save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap().name, "나중");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_project_from_a_newer_version_is_refused() {
        let dir = temp("version");
        let path = dir.join("p.amsil");
        std::fs::write(&path, "version = 99\nname = \"미래\"\n").unwrap();
        let error = Project::load(&path).unwrap_err().to_string();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(error.contains("버전") && error.contains("99"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_a_project_is_an_error() {
        let dir = temp("garbage");
        let path = dir.join("p.amsil");
        std::fs::write(&path, [0xff, 0x00, 0x12]).unwrap();
        assert!(Project::load(&path).is_err());
        std::fs::write(&path, "그냥 글").unwrap();
        assert!(Project::load(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn photos_without_a_record_have_default_edits_and_defaults_are_not_stored() {
        let mut project = sample();
        assert_eq!(project.edit(0, "10161107/DSC04307.ARW").denoise, 0.75);
        assert_eq!(project.edit(0, "다른 사진.ARW"), Edit::default());
        assert_eq!(project.edit(5, "없는 폴더.ARW"), Edit::default());

        project.set_edit(0, "10161107/DSC04307.ARW", Edit::default());
        assert!(
            project.folders[0].photos.is_empty(),
            "returning to the default removes the record"
        );
    }

    #[test]
    fn preset_strength_and_manual_adjustments_are_saved_per_photo() {
        let dir = temp("edits");
        let path = dir.join("p.amsil");
        let mut project = sample();
        let mut adjust = crate::edit::Params {
            exposure: 0.3,
            ..Default::default()
        };
        adjust.hsl.orange.sat = 0.1;
        let edit = Edit {
            preset: Some("food".into()),
            strength: 0.8,
            adjust,
            ..Edit::default()
        };
        project.set_edit(0, "sub/b.ARW", edit.clone());
        project.save(&path).unwrap();

        let loaded = Project::load(&path).unwrap();
        assert_eq!(loaded.edit(0, "sub/b.ARW"), edit);
        assert_eq!(
            loaded.edit(0, "10161107/DSC04307.ARW").strength,
            1.0,
            "an unwritten strength is 1"
        );

        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            text.contains("preset = \"food\"") && text.contains("strength = 0.8"),
            "{text}"
        );
        assert!(text.contains("exposure = 0.3"), "{text}");
        assert!(
            !text.contains("contrast") && !text.contains("blue"),
            "zero values are not written:\n{text}"
        );
    }

    #[test]
    fn a_lut_and_its_strength_are_saved_per_photo() {
        let dir = temp("lut");
        let path = dir.join("p.amsil");
        let mut project = sample();
        let lut = LutRef {
            file: "/home/user/luts/따뜻한 필름.cube".into(),
            strength: 0.5,
        };
        project.set_edit(
            0,
            "a.ARW",
            Edit {
                lut: Some(lut.clone()),
                ..Edit::default()
            },
        );
        project.save(&path).unwrap();
        assert_eq!(
            Project::load(&path).unwrap().edit(0, "a.ARW").lut,
            Some(lut)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            text.contains("따뜻한 필름.cube") && text.contains("strength = 0.5"),
            "{text}"
        );
    }

    #[test]
    fn an_edit_with_full_strength_and_nothing_else_counts_as_default() {
        let mut project = sample();
        project.set_edit(
            0,
            "x.ARW",
            Edit {
                preset: Some("food".into()),
                ..Edit::default()
            },
        );
        assert_eq!(project.folders[0].photos.len(), 2);
        project.set_edit(
            0,
            "x.ARW",
            Edit {
                strength: 1.0,
                ..Edit::default()
            },
        );
        assert_eq!(
            project.folders[0].photos.len(),
            1,
            "becoming the default removes the record"
        );
    }

    #[test]
    fn folders_with_the_same_name_get_numbered_export_names() {
        let mut project = Project::new("p");
        for path in ["/a/DCIM", "/b/여행", "/c/DCIM", "/d/DCIM"] {
            project.folders.push(Folder {
                path: path.into(),
                photos: BTreeMap::new(),
            });
        }
        assert_eq!(project.export_names(), ["DCIM", "여행", "DCIM-2", "DCIM-3"]);
    }

    #[test]
    fn recent_projects_are_remembered_newest_first_without_duplicates() {
        let dir = temp("config");
        let path = dir.join("amsil/config.toml");
        let mut config = Config::load(&path);
        assert!(
            config.recent.is_empty(),
            "the default when there is no settings file"
        );
        for i in 0..12 {
            config.remember(Path::new(&format!("/p/{i}.amsil")));
        }
        config.remember(Path::new("/p/5.amsil"));
        assert_eq!(config.recent.len(), 10);
        assert_eq!(config.recent[0], Path::new("/p/5.amsil"));
        assert_eq!(config.recent[1], Path::new("/p/11.amsil"));
        assert_eq!(
            config
                .recent
                .iter()
                .filter(|p| p.ends_with("5.amsil"))
                .count(),
            1
        );

        config.save(&path).unwrap();
        assert_eq!(Config::load(&path), config);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_chosen_language_is_saved_and_absent_until_chosen() {
        let dir = std::env::temp_dir().join(format!("amsil-config-lang-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut config = Config::load(&path);
        assert_eq!(
            config.lang, None,
            "follows the system language before choosing"
        );
        config.lang = Some(crate::lang::Lang::En);
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = Config::load(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(text.contains("lang = \"en\""), "{text}");
        assert_eq!(loaded.lang, Some(crate::lang::Lang::En));
    }

    #[test]
    fn a_frame_is_saved_per_photo() {
        use crate::frame::{Frame, Layout};
        let dir = temp("frame");
        let path = dir.join("p.amsil");
        let mut project = sample();
        let frame = Frame {
            layout: Layout::Ratio(4, 5),
            dark: true,
        };
        project.set_edit(
            0,
            "a.ARW",
            Edit {
                frame: Some(frame),
                ..Edit::default()
            },
        );
        project.save(&path).unwrap();
        assert_eq!(
            Project::load(&path).unwrap().edit(0, "a.ARW").frame,
            Some(frame)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("layout = \"4:5\"") && text.contains("dark = true"),
            "{text}"
        );
        // If a hand-edited file has an absurd ratio, report it when opening.
        std::fs::write(&path, text.replace("4:5", "1:100")).unwrap();
        let error = Project::load(&path).unwrap_err().to_string();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(error.contains("1:100"), "{error}");
    }

    #[test]
    fn masks_are_saved_per_photo() {
        use crate::mask::{Mask, Point};
        let dir = temp("masks");
        let path = dir.join("p.amsil");
        let mut project = sample();
        let masks = vec![
            Mask {
                points: vec![
                    Point {
                        x: 0.25,
                        y: 0.5,
                        include: true,
                    },
                    Point {
                        x: 0.75,
                        y: 0.5,
                        include: false,
                    },
                ],
                invert: true,
                adjust: Params {
                    exposure: -0.5,
                    ..Params::default()
                },
            },
            Mask::default(),
        ];
        project.set_edit(
            0,
            "a.ARW",
            Edit {
                masks: masks.clone(),
                ..Edit::default()
            },
        );
        project.save(&path).unwrap();
        let loaded = Project::load(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert_eq!(loaded.edit(0, "a.ARW").masks, masks, "{text}");
        assert!(!toml::to_string(&Edit::default()).unwrap().contains("masks"));
    }

    #[test]
    fn the_skin_retouch_strength_is_saved_per_photo() {
        let dir = temp("skin");
        let path = dir.join("p.amsil");
        let mut project = sample();
        project.set_edit(
            0,
            "a.ARW",
            Edit {
                skin: 0.5,
                ..Edit::default()
            },
        );
        project.save(&path).unwrap();
        let loaded = Project::load(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(text.contains("skin = 0.5"), "{text}");
        assert_eq!(loaded.edit(0, "a.ARW").skin, 0.5);
        assert!(!toml::to_string(&Edit::default()).unwrap().contains("skin"));
    }

    #[test]
    fn the_chosen_theme_is_saved_and_absent_until_chosen() {
        let dir = std::env::temp_dir().join(format!("amsil-config-theme-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut config = Config::load(&path);
        assert_eq!(
            config.theme, None,
            "follows the system mode before choosing"
        );
        config.theme = Some(Theme::Light);
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = Config::load(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(text.contains("theme = \"light\""), "{text}");
        assert_eq!(loaded.theme, Some(Theme::Light));
    }

    #[test]
    fn the_compare_source_is_saved_and_raw_until_chosen() {
        let dir = std::env::temp_dir().join(format!("amsil-config-before-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut config = Config::load(&path);
        assert_eq!(
            config.compare_before,
            Before::Raw,
            "the unadjusted RAW before choosing"
        );
        config.compare_before = Before::Jpeg;
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = Config::load(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(text.contains("compare_before = \"jpeg\""), "{text}");
        assert_eq!(loaded.compare_before, Before::Jpeg);
        let old: Config = toml::from_str("recent = [\"/a.amsil\"]\n").unwrap();
        assert_eq!(old.compare_before, Before::Raw);
    }

    #[test]
    fn the_compare_split_is_saved_and_automatic_until_chosen() {
        let dir = std::env::temp_dir().join(format!("amsil-config-split-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut config = Config::load(&path);
        assert_eq!(
            config.compare_split,
            Split::Auto,
            "automatic before choosing"
        );
        config.compare_split = Split::Stacked;
        config.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let loaded = Config::load(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(text.contains("compare_split = \"stacked\""), "{text}");
        assert_eq!(loaded.compare_split, Split::Stacked);
        // Older settings files without this field still read fine (otherwise the recent projects
        // list would be lost).
        let old: Config = toml::from_str("recent = [\"/a.amsil\"]\n").unwrap();
        assert_eq!(old.compare_split, Split::Auto);
        assert_eq!(old.recent.len(), 1);
    }
}
