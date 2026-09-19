use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const SETTINGS_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    #[default]
    Light,
    Dark,
    System,
}

impl UiTheme {
    pub fn class(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ReaderDefaults {
    pub font_size: u8,
    pub line_height: f32,
    pub margin: u16,
    pub theme: crate::model::ReaderTheme,
}

impl Default for ReaderDefaults {
    fn default() -> Self {
        Self {
            font_size: 18,
            line_height: 1.8,
            margin: 64,
            theme: crate::model::ReaderTheme::Light,
        }
    }
}

impl ReaderDefaults {
    fn validate(&self) -> Result<()> {
        if !(12..=32).contains(&self.font_size)
            || ![1.5, 1.8, 2.1].contains(&self.line_height)
            || ![32, 64, 96].contains(&self.margin)
        {
            bail!("阅读器默认值超出允许范围");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub schema_version: u32,
    pub ui_theme: UiTheme,
    pub auto_open_recent: bool,
    pub reader: ReaderDefaults,
    pub last_library: Option<PathBuf>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_VERSION,
            ui_theme: UiTheme::Light,
            auto_open_recent: true,
            reader: ReaderDefaults::default(),
            last_library: None,
        }
    }
}

impl AppSettings {
    fn validate(&self) -> Result<()> {
        if self.schema_version != SETTINGS_VERSION {
            bail!("不支持的设置版本：{}", self.schema_version);
        }
        self.reader.validate()
    }
}

pub fn config_dir() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("APROBOOK_CONFIG_DIR") {
        let directory = PathBuf::from(directory);
        if directory.is_absolute() {
            return Some(directory);
        }
    }
    dirs::config_dir().map(|directory| directory.join("AproBook"))
}

fn legacy_config_dir() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("THEEBOOKVIEWER_CONFIG_DIR") {
        let directory = PathBuf::from(directory);
        if directory.is_absolute() {
            return Some(directory);
        }
    }
    dirs::config_dir().map(|directory| directory.join("theEBookViewer"))
}

pub fn load() -> Result<AppSettings> {
    let directory = config_dir().context("无法定位应用设置目录")?;
    load_with_legacy(&directory, legacy_config_dir().as_deref())
}

fn load_with_legacy(directory: &Path, legacy_directory: Option<&Path>) -> Result<AppSettings> {
    if directory.join("settings.json").exists() || directory.join("last-library.txt").exists() {
        return load_from(directory);
    }
    if let Some(legacy_directory) = legacy_directory
        && legacy_directory != directory
        && (legacy_directory.join("settings.json").exists()
            || legacy_directory.join("last-library.txt").exists())
    {
        let settings = load_from(legacy_directory)?;
        save_to(directory, &settings)?;
        return Ok(settings);
    }
    load_from(directory)
}

fn load_from(directory: &Path) -> Result<AppSettings> {
    let path = directory.join("settings.json");
    if path.exists() {
        let bytes = fs::read(&path).with_context(|| format!("无法读取设置：{}", path.display()))?;
        let settings: AppSettings = serde_json::from_slice(&bytes).context("设置文件格式有误")?;
        settings.validate()?;
        return Ok(settings);
    }
    let mut settings = AppSettings::default();
    if let Ok(data) = fs::read_to_string(directory.join("last-library.txt")) {
        let path = PathBuf::from(data.trim());
        if path.is_dir() {
            settings.last_library = Some(path);
        }
    }
    Ok(settings)
}

pub fn save(settings: &AppSettings) -> Result<()> {
    let directory = config_dir().context("无法定位应用设置目录")?;
    save_to(&directory, settings)
}

fn save_to(directory: &Path, settings: &AppSettings) -> Result<()> {
    settings.validate()?;
    fs::create_dir_all(directory).context("无法创建应用设置目录")?;
    let target = directory.join("settings.json");
    let temporary = directory.join(format!("settings-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        serde_json::to_writer_pretty(&mut file, settings)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, &target)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("无法保存设置：{}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_atomic_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        assert_eq!(settings.reader.font_size, 18);
        assert!(settings.auto_open_recent);
        settings.ui_theme = UiTheme::Dark;
        settings.reader.margin = 96;
        save_to(temp.path(), &settings).unwrap();
        assert_eq!(load_from(temp.path()).unwrap(), settings);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn migrates_legacy_last_library_without_changing_it() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        fs::write(
            temp.path().join("last-library.txt"),
            library.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let settings = load_from(temp.path()).unwrap();
        assert_eq!(settings.last_library, Some(library));
        assert!(!temp.path().join("settings.json").exists());
    }

    #[test]
    fn clearing_recent_library_does_not_restore_legacy_record() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("library");
        fs::create_dir(&library).unwrap();
        fs::write(
            temp.path().join("last-library.txt"),
            library.to_string_lossy().as_bytes(),
        )
        .unwrap();
        let mut settings = load_from(temp.path()).unwrap();
        assert_eq!(settings.last_library, Some(library));
        settings.last_library = None;
        save_to(temp.path(), &settings).unwrap();
        assert_eq!(load_from(temp.path()).unwrap().last_library, None);
    }

    #[test]
    fn rejects_invalid_values_and_future_schema() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        settings.reader.font_size = 40;
        assert!(save_to(temp.path(), &settings).is_err());
        settings.reader.font_size = 18;
        settings.schema_version = 2;
        assert!(save_to(temp.path(), &settings).is_err());
        fs::write(temp.path().join("settings.json"), b"{broken").unwrap();
        assert!(load_from(temp.path()).is_err());
    }

    #[test]
    fn migrates_old_config_without_modifying_it() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("theEBookViewer");
        let new = temp.path().join("AproBook");
        let settings = AppSettings {
            ui_theme: UiTheme::Dark,
            ..Default::default()
        };
        save_to(&old, &settings).unwrap();
        let old_bytes = fs::read(old.join("settings.json")).unwrap();

        assert_eq!(load_with_legacy(&new, Some(&old)).unwrap(), settings);
        assert_eq!(load_from(&new).unwrap(), settings);
        assert_eq!(fs::read(old.join("settings.json")).unwrap(), old_bytes);
    }

    #[test]
    fn migrates_legacy_recent_library_into_new_settings() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("theEBookViewer");
        let new = temp.path().join("AproBook");
        let library = temp.path().join("library");
        fs::create_dir(&old).unwrap();
        fs::create_dir(&library).unwrap();
        fs::write(
            old.join("last-library.txt"),
            library.to_string_lossy().as_bytes(),
        )
        .unwrap();

        let settings = load_with_legacy(&new, Some(&old)).unwrap();
        assert_eq!(settings.last_library, Some(library));
        assert_eq!(load_from(&new).unwrap(), settings);
        assert!(old.join("last-library.txt").exists());
    }

    #[test]
    fn new_config_takes_precedence_over_old_config() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("theEBookViewer");
        let new = temp.path().join("AproBook");
        let old_settings = AppSettings {
            ui_theme: UiTheme::Dark,
            ..Default::default()
        };
        save_to(&old, &old_settings).unwrap();
        let new_settings = AppSettings {
            ui_theme: UiTheme::System,
            ..Default::default()
        };
        save_to(&new, &new_settings).unwrap();

        assert_eq!(load_with_legacy(&new, Some(&old)).unwrap(), new_settings);
    }

    #[test]
    fn invalid_old_config_does_not_create_new_config() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("theEBookViewer");
        let new = temp.path().join("AproBook");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("settings.json"), b"{broken").unwrap();

        assert!(load_with_legacy(&new, Some(&old)).is_err());
        assert!(!new.exists());
    }
}
