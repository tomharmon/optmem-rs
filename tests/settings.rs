use std::{
    fs,
    path::Path,
    process::{Command, Output},
    sync::Arc,
};

use optmem::{
    Config, ConfigKey, OptMem,
    settings::AppSettings,
    storage::{FileStorage, MemoryStorage},
};

fn cli(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memo"));
    command
        .env("HOME", home)
        .env_remove("OPTMEM_CONFIG")
        .env_remove("MEMORY_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
    command
}
fn text(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[test]
fn load_is_read_only_and_initialization_writes_an_idempotent_user_config() {
    let temp = tempfile::tempdir().unwrap();
    let config_path = temp.path().join("user-config/optmem/config.toml");
    let dir = temp.path().join("memory");
    let settings = AppSettings::load(Some(config_path.clone())).unwrap();
    assert!(!config_path.exists());
    assert!(!config_path.parent().unwrap().exists());
    assert_eq!(settings.storage_dir(Some(&dir)).unwrap(), dir);
    FileStorage::initialize(&dir).unwrap();
    let settings = settings.initialize(&dir, &Config::default()).unwrap();
    let before = fs::read(&config_path).unwrap();
    let values: toml::Value = toml::from_str(std::str::from_utf8(&before).unwrap()).unwrap();
    assert_eq!(
        values["storage_dir"].as_str().unwrap(),
        dir.to_str().unwrap()
    );
    assert_eq!(values["entry_chars"].as_integer(), Some(280));
    assert!(!dir.join("config").exists());
    settings.initialize(&dir, &Config::default()).unwrap();
    assert_eq!(fs::read(&config_path).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(config_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn user_settings_are_authoritative_and_resets_do_not_restore_legacy_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let dir = temp.path().join("memory");
    let old = OptMem::new(FileStorage::initialize(&dir).unwrap());
    old.configure(&[(ConfigKey::EntryChars, Some(50))]).unwrap();
    let settings = AppSettings::load(Some(path.clone())).unwrap();
    assert_eq!(settings.memory_config(&dir).unwrap().entry_chars, 50);
    let settings = settings
        .initialize(&dir, &settings.memory_config(&dir).unwrap())
        .unwrap();
    let settings = settings.update(&["entry_chars=23".into()], None).unwrap();
    assert_eq!(settings.memory_config(&dir).unwrap().entry_chars, 23);
    let settings = settings.update(&["ENTRY_CHARS=".into()], None).unwrap();
    assert_eq!(settings.memory_config(&dir).unwrap().entry_chars, 280);
    assert_eq!(old.config().unwrap().entry_chars, 50);
    fs::write(dir.join("config"), "ENTRY_CHARS=999\n").unwrap();
    assert_eq!(
        AppSettings::load(Some(path))
            .unwrap()
            .memory_config(&dir)
            .unwrap()
            .entry_chars,
        280
    );
}

#[test]
fn edits_validate_as_a_batch_and_concurrent_edits_preserve_other_keys() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let dir = temp.path().join("memory");
    let settings = AppSettings::load(Some(path.clone())).unwrap();
    assert!(
        settings
            .update(
                &["wake_lines=10".into(), "entry_chars=999".into()],
                Some(&dir)
            )
            .is_err()
    );
    assert!(!path.exists());
    let settings = settings.initialize(&dir, &Config::default()).unwrap();
    let before = fs::read(&path).unwrap();
    for assignment in [
        "entry_chars=0",
        "wake_lines=x",
        "nope=1",
        "entry_chars",
        "part_lines=-1",
    ] {
        assert!(settings.update(&[assignment.into()], None).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let handles: Vec<_> = ["entry_chars=23", "wake_lines=7"]
        .into_iter()
        .map(|assignment| {
            let settings = settings.clone();
            std::thread::spawn(move || settings.update(&[assignment.into()], None).unwrap())
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let actual = AppSettings::load(Some(path))
        .unwrap()
        .memory_config(&dir)
        .unwrap();
    assert_eq!((actual.entry_chars, actual.wake_lines), (23, 7));
}

#[test]
fn malformed_toml_names_the_config_file_and_does_not_fall_back() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    for source in [
        "entry_chars=999\n",
        "entry_chars=-1\n",
        "storage_dir=\"\"\n",
        "entry_chrs=12\n",
        "wake_lines = [\n",
    ] {
        fs::write(&path, source).unwrap();
        let error = AppSettings::load(Some(path.clone()))
            .unwrap_err()
            .to_string();
        assert!(error.contains(path.to_str().unwrap()), "{error}");
    }
}

#[test]
fn explicit_engine_configuration_works_without_user_or_backend_config() {
    let old = OptMem::new(MemoryStorage::new());
    old.configure(&[(ConfigKey::EntryChars, Some(5))]).unwrap();
    let storage = Arc::new(old.into_storage());
    let config = Config {
        entry_chars: 20,
        ..Config::default()
    };
    let memory = OptMem::with_config(storage.clone(), config).unwrap();
    memory
        .note(
            chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
            "longer than five",
        )
        .unwrap();
    assert_eq!(memory.config().unwrap().entry_chars, 20);
    assert!(
        memory
            .configure(&[(ConfigKey::EntryChars, Some(10))])
            .is_err()
    );
    assert_eq!(OptMem::new(storage).config().unwrap().entry_chars, 5);
    assert!(
        OptMem::with_config(
            MemoryStorage::new(),
            Config {
                entry_chars: 281,
                ..Config::default()
            }
        )
        .is_err()
    );
}

#[test]
fn xdg_homes_and_plain_followup_commands_work_from_the_config() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let config_home = temp.path().join("configuration");
    let data_home = temp.path().join("data");
    let command = || {
        let mut c = cli(&home);
        c.env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_DATA_HOME", &data_home);
        c
    };
    let inspected = command().arg("config").output().unwrap();
    assert!(inspected.status.success());
    assert!(text(&inspected).contains(config_home.join("optmem/config.toml").to_str().unwrap()));
    assert!(!config_home.exists());
    assert!(!data_home.exists());
    let initialized = command().arg("init").output().unwrap();
    assert!(initialized.status.success());
    assert!(config_home.join("optmem/config.toml").exists());
    assert!(data_home.join("optmem/memory/LOG.txt").exists());
    let edited = command()
        .args(["config", "entry_chars=12"])
        .output()
        .unwrap();
    assert!(edited.status.success());
    assert!(
        !command()
            .args(["note", "this is too long"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        command()
            .args(["note", "one"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let note = command().args(["note", "two"]).output().unwrap();
    assert!(text(&note).contains("Run: memo nap 0-1 \"<your line>\""));
    assert!(!text(&note).contains("--memory-dir"));
    assert!(!text(&note).contains(env!("CARGO_BIN_EXE_memo")));
}

#[test]
fn relative_xdg_variables_are_ignored_and_profiles_override_environment() {
    let temp = tempfile::tempdir().unwrap();
    let result = cli(temp.path())
        .env("XDG_CONFIG_HOME", "relative")
        .env("XDG_DATA_HOME", "relative")
        .arg("config")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(
        text(&result).contains(
            temp.path()
                .join(".config/optmem/config.toml")
                .to_str()
                .unwrap()
        )
    );
    assert!(
        text(&result).contains(
            temp.path()
                .join(".local/share/optmem/memory")
                .to_str()
                .unwrap()
        )
    );
    let environment_profile = temp.path().join("env.toml");
    let explicit_profile = temp.path().join("explicit.toml");
    fs::write(&environment_profile, "entry_chars=12\n").unwrap();
    fs::write(&explicit_profile, "entry_chars=23\n").unwrap();
    let result = cli(temp.path())
        .env("OPTMEM_CONFIG", &environment_profile)
        .arg("--config")
        .arg(&explicit_profile)
        .arg("config")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(text(&result).contains(explicit_profile.to_str().unwrap()));
    assert!(text(&result).contains("23"));
}

#[test]
fn legacy_identity_and_limits_are_adopted_without_rewriting_memory() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".optmem/memory");
    let memory = OptMem::new(FileStorage::initialize(&dir).unwrap());
    memory
        .note(
            chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
            "old original",
        )
        .unwrap();
    memory
        .configure(&[(ConfigKey::EntryChars, Some(24))])
        .unwrap();
    let log = fs::read(dir.join("LOG.txt")).unwrap();
    let old_config = fs::read(dir.join("config")).unwrap();
    let init = cli(home.path()).arg("init").output().unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    assert!(text(&init).contains("Found"));
    let path = home.path().join(".config/optmem/config.toml");
    let settings = AppSettings::load(Some(path.clone())).unwrap();
    assert_eq!(settings.storage_dir(None).unwrap(), dir);
    assert_eq!(settings.memory_config(&dir).unwrap().entry_chars, 24);
    assert_eq!(fs::read(dir.join("LOG.txt")).unwrap(), log);
    assert_eq!(fs::read(dir.join("config")).unwrap(), old_config);
    let first_config = fs::read(&path).unwrap();
    assert!(
        cli(home.path())
            .arg("init")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&path).unwrap(), first_config);
    assert!(!home.path().join(".local/share/optmem/memory").exists());
}

#[test]
fn configuring_storage_does_not_create_an_identity_and_relative_paths_are_stable() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config.toml");
    fs::write(&config, "storage_dir=\"relative-memory\"\nentry_chars=23\n").unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let result = cli(temp.path())
        .arg("--config")
        .arg(&config)
        .arg("config")
        .current_dir(elsewhere.path())
        .output()
        .unwrap();
    let dir = temp.path().join("relative-memory");
    assert!(text(&result).contains(dir.to_str().unwrap()));
    assert!(!dir.exists());
    let result = cli(temp.path())
        .arg("--config")
        .arg(&config)
        .arg("wake")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!dir.exists());
    let result = cli(temp.path())
        .arg("--config")
        .arg(&config)
        .arg("init")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(dir.join("LOG.txt").exists());
    let other = temp.path().join("another-memory");
    let setting = format!("storage_dir={}", other.display());
    let result = cli(temp.path())
        .arg("--config")
        .arg(&config)
        .args(["config", &setting])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(!other.exists());
}

#[test]
fn ambiguous_legacy_and_xdg_identities_can_be_resolved_by_config() {
    let home = tempfile::tempdir().unwrap();
    let legacy = home.path().join(".optmem/memory");
    let modern = home.path().join(".local/share/optmem/memory");
    FileStorage::initialize(&legacy).unwrap();
    FileStorage::initialize(&modern).unwrap();
    let refused = cli(home.path()).arg("wake").output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("Both legacy and XDG"));
    let setting = format!("storage_dir={}", legacy.display());
    let fixed = cli(home.path())
        .args(["config", &setting])
        .output()
        .unwrap();
    assert!(
        fixed.status.success(),
        "{}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    assert!(
        cli(home.path())
            .arg("wake")
            .output()
            .unwrap()
            .status
            .success()
    );
}
