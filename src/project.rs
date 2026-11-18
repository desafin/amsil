//! 프로젝트 파일(`이름.amsil`)과 프로그램 설정 파일.
//!
//! 프로젝트 파일에는 가져온 폴더, 사진별 편집, 내보내기 설정이 들어간다. 사진 폴더에는 아무것도 쓰지 않는다.

use crate::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 이 프로그램이 읽고 쓰는 프로젝트 파일 형식의 버전.
const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    version: u32,
    pub name: String,
    #[serde(default)]
    pub export: Export,
    /// 가져온 폴더들.
    #[serde(default, rename = "folder")]
    pub folders: Vec<Folder>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Export {
    /// 내보낼 폴더. 아직 정하지 않았으면 None.
    pub folder: Option<PathBuf>,
    pub quality: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Folder {
    pub path: PathBuf,
    /// 기본값과 다른 사진만 기록한다. 키는 `path` 기준 상대 경로.
    #[serde(default, rename = "photo")]
    pub photos: BTreeMap<String, Edit>,
}

/// 사진 한 장의 편집 내용.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Edit {
    /// 노이즈 제거 강도 (0~1).
    #[serde(default)]
    pub denoise: f32,
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
        // 버전부터 본다. 형식이 달라졌을 수 있는 파일을 억지로 읽지 않는다.
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }
        let header: Header =
            toml::from_str(&text).map_err(|e| format!("프로젝트 파일이 아닙니다 ({e})"))?;
        if header.version != VERSION {
            return Err(format!(
                "이 프로젝트 파일은 버전 {}이고, 이 프로그램은 버전 {VERSION}만 읽을 수 있습니다",
                header.version
            )
            .into());
        }
        Ok(toml::from_str(&text).map_err(|e| format!("프로젝트 파일을 읽을 수 없습니다 ({e})"))?)
    }

    /// 임시 파일에 쓴 뒤 이름을 바꾼다. 저장 도중 꺼져도 기존 파일이 깨지지 않는다.
    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomically(path, &toml::to_string(self)?)
    }

    /// 사진의 편집 내용. 기록이 없으면 기본값.
    pub fn edit(&self, folder: usize, photo: &str) -> Edit {
        self.folders
            .get(folder)
            .and_then(|f| f.photos.get(photo))
            .copied()
            .unwrap_or_default()
    }

    /// 사진의 편집 내용을 바꾼다. 기본값으로 돌아가면 기록을 지운다.
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

    /// 폴더마다 출력 폴더 안에서 쓸 이름. 이름이 같은 폴더가 있으면 번호를 붙여 구분한다.
    pub fn export_names(&self) -> Vec<String> {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        self.folders
            .iter()
            .map(|folder| {
                let name = folder
                    .path
                    .file_name()
                    .map_or("사진".into(), |n| n.to_string_lossy().into_owned());
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

/// 프로그램 설정 (`~/.config/amsil/config.toml`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Config {
    /// 최근에 연 프로젝트. 최신이 맨 앞.
    #[serde(default)]
    pub recent: Vec<PathBuf>,
}

/// 최근 프로젝트 목록의 최대 길이.
const MAX_RECENT: usize = 10;

impl Config {
    /// 없거나 읽을 수 없으면 기본값.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomically(path, &toml::to_string(self)?)
    }

    /// 프로젝트를 최근 목록의 맨 앞에 올린다. 목록은 10개까지.
    pub fn remember(&mut self, project: &Path) {
        self.recent.retain(|p| p != project);
        self.recent.insert(0, project.to_owned());
        self.recent.truncate(MAX_RECENT);
    }

    /// 설정 파일의 위치.
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
        project.set_edit(0, "10161107/DSC04307.ARW", Edit { denoise: 0.75 });
        project
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
            "기본값으로 돌아가면 기록을 지운다"
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
        assert!(config.recent.is_empty(), "설정 파일이 없으면 기본값");
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
}
