//! Shared XDG user configuration for memo and optmem-mcp. The engine itself never
//! consults the environment; callers explicitly pass the resulting Config.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use xdg::BaseDirectories;

use crate::{
    Config, ConfigKey, ConfigOverrides, Error, Result,
    error::io,
    storage::{FileStorage, Storage},
};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Values {
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wake_lines: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    part_chars: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    part_lines: Option<usize>,
}

impl Values {
    fn sizes(&self) -> ConfigOverrides {
        [
            (ConfigKey::EntryChars, self.entry_chars),
            (ConfigKey::WakeLines, self.wake_lines),
            (ConfigKey::PartChars, self.part_chars),
            (ConfigKey::PartLines, self.part_lines),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect()
    }
    fn set_size(&mut self, key: ConfigKey, value: Option<usize>) {
        match key {
            ConfigKey::EntryChars => self.entry_chars = value,
            ConfigKey::WakeLines => self.wake_lines = value,
            ConfigKey::PartChars => self.part_chars = value,
            ConfigKey::PartLines => self.part_lines = value,
        }
    }
    fn fill_sizes(&mut self, config: &Config) {
        self.entry_chars = Some(config.entry_chars);
        self.wake_lines = Some(config.wake_lines);
        self.part_chars = Some(config.part_chars);
        self.part_lines = Some(config.part_lines);
    }
    fn validate(&self) -> Result<()> {
        if self
            .storage_dir
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(Error::Invalid(
                "storage_dir must not be empty; omit it to use the default.".into(),
            ));
        }
        Config::from_overrides(&self.sizes())?;
        Ok(())
    }
}

/// A loaded user settings snapshot. Loading never creates directories or a memory.
#[derive(Clone, Debug)]
pub struct AppSettings {
    path: PathBuf,
    default_storage_dir: Option<PathBuf>,
    home: Option<PathBuf>,
    values: Values,
    exists: bool,
}

impl AppSettings {
    /// `selected` is --config/OPTMEM_CONFIG, otherwise XDG_CONFIG_HOME/optmem/config.toml.
    pub fn load(selected: Option<PathBuf>) -> Result<Self> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let (default_config, default_storage_dir) = locations(home.as_deref());
        let path = match selected {
            Some(path) => resolve(&path, home.as_deref(), &current_dir()?)?,
            None => default_config
                .ok_or_else(|| Error::Invalid("Set HOME, XDG_CONFIG_HOME, or --config.".into()))?,
        };
        let (values, exists) = read(&path)?;
        Ok(Self {
            path,
            default_storage_dir,
            home,
            values,
            exists,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// User-configured sizes without consulting any backend, for file, database,
    /// and in-memory integrations. Unset values follow the tool defaults.
    pub fn config(&self) -> Result<Config> {
        Config::from_overrides(&self.values.sizes())
    }

    /// Explicit directory > user config > XDG data default. With no user config,
    /// adopt a recognizable old ~/.optmem/memory rather than losing its identity.
    pub fn storage_dir(&self, selected: Option<&Path>) -> Result<PathBuf> {
        if let Some(path) = selected.filter(|p| !p.as_os_str().is_empty()) {
            return resolve(path, self.home.as_deref(), &current_dir()?);
        }
        if let Some(path) = &self.values.storage_dir {
            return resolve(
                path,
                self.home.as_deref(),
                self.path.parent().expect("absolute config path"),
            );
        }
        let default = self.default_storage_dir.clone().ok_or_else(|| {
            Error::Invalid("Set HOME, XDG_DATA_HOME, or storage_dir in the user config.".into())
        })?;
        let explicit_data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .is_some_and(|p| p.is_absolute());
        if !self.exists
            && !explicit_data_home
            && let Some(home) = &self.home
        {
            let legacy = home.join(".optmem/memory");
            if legacy
                .join("LOG.txt")
                .try_exists()
                .map_err(|e| io(&legacy, e))?
            {
                if default
                    .join("LOG.txt")
                    .try_exists()
                    .map_err(|e| io(&default, e))?
                {
                    return Err(Error::Invalid("Both legacy and XDG memories exist. Set storage_dir or --memory-dir explicitly; no identity was selected.".into()));
                }
                return Ok(legacy);
            }
        }
        Ok(default)
    }

    /// Once a user config exists it is authoritative. Legacy store sizes are read
    /// only before that point, then copied to the new user config on init/update.
    pub fn memory_config(&self, dir: &Path) -> Result<Config> {
        if self.exists {
            return self.config();
        }
        if dir
            .join("config")
            .try_exists()
            .map_err(|e| io(dir.join("config"), e))?
        {
            return Config::from_overrides(&FileStorage::open(dir)?.transaction()?.read_config()?);
        }
        Ok(Config::default())
    }

    /// Persist the selected identity after explicit initialization, preserving
    /// existing user settings and preserving the memory's legacy files byte-for-byte.
    pub fn initialize(&self, dir: &Path, config: &Config) -> Result<Self> {
        self.modify(|values, exists| {
            if !exists {
                values.fill_sizes(config);
            }
            let current = values
                .storage_dir
                .as_ref()
                .map(|p| {
                    resolve(
                        p,
                        self.home.as_deref(),
                        self.path.parent().expect("config parent"),
                    )
                })
                .transpose()?;
            if current.as_deref() != Some(dir) {
                values.storage_dir = Some(dir.into());
            }
            Ok(())
        })
    }

    /// Edit user settings without requiring or creating a memory. Empty values
    /// restore defaults. Size names accept the legacy upper-case spelling too.
    pub fn update(&self, assignments: &[String], selected_dir: Option<&Path>) -> Result<Self> {
        let mut changes = Vec::new();
        for assignment in assignments {
            let (key, value) = assignment.split_once('=').ok_or_else(|| {
                Error::Invalid("usage: memo config [storage_dir=PATH | NAME=VALUE ...]".into())
            })?;
            let key = key.trim().to_ascii_uppercase();
            let value = value.trim();
            if key == "STORAGE_DIR" {
                let value = if value.is_empty() {
                    None
                } else {
                    Some(resolve(
                        Path::new(value),
                        self.home.as_deref(),
                        &current_dir()?,
                    )?)
                };
                changes.push(Change::Storage(value));
            } else {
                let key: ConfigKey = key.parse()?;
                let value = if value.is_empty() {
                    None
                } else {
                    if !value.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(Error::Invalid(format!(
                            "{key} must be a positive whole number, not '{value}'."
                        )));
                    }
                    let value = value
                        .parse()
                        .map_err(|_| Error::Invalid(format!("{key} is too large.")))?;
                    key.validate(value)?;
                    Some(value)
                };
                changes.push(Change::Size(key, value));
            }
        }
        let requested_storage = changes.iter().rev().find_map(|change| match change {
            Change::Storage(path) => Some(path.as_deref()),
            Change::Size(_, _) => None,
        });
        let dir = match requested_storage {
            Some(Some(path)) => path.to_path_buf(),
            Some(None) => self.default_storage_dir.clone().ok_or_else(|| {
                Error::Invalid("Set HOME or XDG_DATA_HOME before resetting storage_dir.".into())
            })?,
            None => self.storage_dir(selected_dir)?,
        };
        let config = self.memory_config(&dir)?;
        self.modify(|values, exists| {
            if !exists {
                values.storage_dir = Some(dir);
                values.fill_sizes(&config);
            }
            for change in changes {
                match change {
                    Change::Storage(path) => values.storage_dir = path,
                    Change::Size(key, value) => values.set_size(key, value),
                }
            }
            Ok(())
        })
    }

    fn modify(&self, edit: impl FnOnce(&mut Values, bool) -> Result<()>) -> Result<Self> {
        let parent = self.path.parent().expect("absolute config path");
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent).map_err(|e| io(parent, e))?;
        let lock_path = self.path.with_extension("toml.lock");
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| io(&lock_path, e))?;
        FileExt::lock_exclusive(&lock).map_err(|e| io(&lock_path, e))?;
        let (mut values, exists) = read(&self.path)?;
        let original = values.clone();
        edit(&mut values, exists)?;
        values.validate()?;
        if !exists || values != original {
            let serialized = toml::to_string_pretty(&values)
                .map_err(|e| Error::Invalid(format!("{}: {e}", self.path.display())))?;
            let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| io(parent, e))?;
            tmp.write_all(serialized.as_bytes())
                .map_err(|e| io(tmp.path(), e))?;
            tmp.as_file().sync_all().map_err(|e| io(tmp.path(), e))?;
            tmp.persist(&self.path)
                .map_err(|e| io(&self.path, e.error))?;
            #[cfg(unix)]
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(|e| io(parent, e))?;
        }
        drop(lock);
        Ok(Self {
            values,
            exists: true,
            ..self.clone()
        })
    }
}

#[cfg(unix)]
fn locations(_home: Option<&Path>) -> (Option<PathBuf>, Option<PathBuf>) {
    let dirs = BaseDirectories::with_prefix("optmem");
    (
        dirs.get_config_file("config.toml"),
        dirs.get_data_file("memory"),
    )
}

#[cfg(not(unix))]
fn locations(home: Option<&Path>) -> (Option<PathBuf>, Option<PathBuf>) {
    let root = |key: &str, fallback: &str| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.map(|h| h.join(fallback)))
            .map(|p| p.join("optmem"))
    };
    (
        root("XDG_CONFIG_HOME", ".config").map(|p| p.join("config.toml")),
        root("XDG_DATA_HOME", ".local/share").map(|p| p.join("memory")),
    )
}

enum Change {
    Storage(Option<PathBuf>),
    Size(ConfigKey, Option<usize>),
}

fn read(path: &Path) -> Result<(Values, bool)> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Values::default(), false));
        }
        Err(e) => return Err(io(path, e)),
    };
    let values: Values =
        toml::from_str(&text).map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
    values
        .validate()
        .map_err(|e| Error::Invalid(format!("{}: {e}", path.display())))?;
    Ok((values, true))
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir().map_err(|e| io(".", e))
}

fn resolve(path: &Path, home: Option<&Path>, base: &Path) -> Result<PathBuf> {
    let path = if let Ok(rest) = path.strip_prefix("~") {
        home.ok_or_else(|| Error::Invalid("Set HOME to expand '~' in settings.".into()))?
            .join(rest)
    } else {
        path.into()
    };
    Ok(if path.is_absolute() {
        path
    } else {
        base.join(path)
    })
}
