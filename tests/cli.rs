use std::{
    fs,
    process::{Command, Output, Stdio},
};

fn run(dir: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_memo"))
        .env_remove("OPTMEM_CONFIG")
        .env("XDG_CONFIG_HOME", dir.join(".test-config"))
        .env("XDG_DATA_HOME", dir.join(".test-data"))
        .arg("--memory-dir")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
}
fn stdout(result: &Output) -> String {
    String::from_utf8(result.stdout.clone()).unwrap()
}
fn stderr(result: &Output) -> String {
    String::from_utf8(result.stderr.clone()).unwrap()
}

#[test]
fn fresh_cli_lifecycle_and_exit_codes() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("memory");
    let absent = run(&dir, &["wake"]);
    assert!(!absent.status.success());
    assert!(stderr(&absent).contains("No memory at"));
    assert!(!dir.exists());
    let init = run(&dir, &["init"]);
    assert!(init.status.success(), "{}", stderr(&init));
    assert!(stdout(&init).contains("## Memory"));
    assert!(stdout(&init).contains("`memo wake`"));
    assert!(!stdout(&init).contains("{memo}"));
    assert!(!stdout(&init).contains(env!("CARGO_BIN_EXE_memo")));
    assert!(
        stdout(&init).contains("You're a subagent") || stdout(&init).contains("You are a subagent")
    );
    assert!(stdout(&run(&dir, &["init"])).contains("Found"));
    assert!(stdout(&run(&dir, &["wake"])).ends_with("You are awake.\n"));
    assert!(stdout(&run(&dir, &["note", "first fact"])).contains("Saved as #0"));
    let note = run(&dir, &["note", "second fact"]);
    assert!(note.status.success());
    assert!(stdout(&note).contains("Compress memories #0-1"));
    assert!(!run(&dir, &["nap", "0-1"]).status.success());
    assert!(stdout(&run(&dir, &["nap", "0-1", "two facts"])).contains("0-1 saved"));
    assert!(stdout(&run(&dir, &["zoom", "0-3"])).contains("#0-1 two facts"));
    assert!(stdout(&run(&dir, &["recall", "FIRST"])).contains("1 match."));
    assert!(stdout(&run(&dir, &["forget", "0-1"])).contains("Forgot 1 summary"));
    assert!(stdout(&run(&dir, &["nap"])).contains("Compress memories #0-1"));
    for args in [
        &["note", ""][..],
        &["zoom", "5-6"],
        &["config", "ENTRY_CHARS=999"],
        &["recall", "["],
    ] {
        assert!(!run(&dir, args).status.success());
    }
}

#[test]
fn missing_default_store_and_memory_dir_environment() {
    let home = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_memo"))
        .env_remove("MEMORY_DIR")
        .env_remove("OPTMEM_CONFIG")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("HOME", home.path())
        .arg("wake")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!home.path().join(".optmem").exists());
    let result = Command::new(env!("CARGO_BIN_EXE_memo"))
        .env_remove("MEMORY_DIR")
        .env_remove("OPTMEM_CONFIG")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env("HOME", home.path())
        .arg("init")
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", stderr(&result));
    assert!(home.path().join(".config/optmem/config.toml").exists());
    assert!(
        home.path()
            .join(".local/share/optmem/memory/LOG.txt")
            .exists()
    );
    assert!(
        !home
            .path()
            .join(".local/share/optmem/memory/config")
            .exists()
    );
    let selected = home.path().join("selected");
    let result = Command::new(env!("CARGO_BIN_EXE_memo"))
        .env_remove("OPTMEM_CONFIG")
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("MEMORY_DIR", &selected)
        .arg("init")
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(selected.join("LOG.txt").exists());
}

#[test]
fn multiple_processes_append_without_id_collisions() {
    let temp = tempfile::tempdir().unwrap();
    assert!(run(temp.path(), &["init"]).status.success());
    let mut children: Vec<_> = (0..16)
        .map(|i| {
            Command::new(env!("CARGO_BIN_EXE_memo"))
                .env_remove("OPTMEM_CONFIG")
                .env("XDG_CONFIG_HOME", temp.path().join(".test-config"))
                .env("XDG_DATA_HOME", temp.path().join(".test-data"))
                .env("MEMORY_DIR", temp.path())
                .args(["note", &format!("parallel note {i}")])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in &mut children {
        assert!(child.wait().unwrap().success());
    }
    let bytes = fs::read(temp.path().join("LOG.txt")).unwrap();
    assert_eq!(bytes.len(), 16 * 320);
    for (i, record) in bytes.chunks_exact(320).enumerate() {
        assert!(record.starts_with(format!("#{i} ").as_bytes()));
    }
}

#[cfg(unix)]
#[test]
fn printed_command_uses_memo_on_path_and_quotes_the_directory_override() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().join("an identity's memories");
    assert!(run(&dir, &["init"]).status.success());
    run(&dir, &["note", "one"]);
    let result = run(&dir, &["note", "two"]);
    let text = stdout(&result);
    let command = text
        .lines()
        .find_map(|line| line.strip_prefix("Run: "))
        .unwrap()
        .replace("\"<your line>\"", "\"both\"");
    assert!(command.starts_with("memo "));
    assert!(!command.contains(env!("CARGO_BIN_EXE_memo")));
    let result = Command::new("/bin/sh")
        .args(["-c", &command])
        .env(
            "PATH",
            std::path::Path::new(env!("CARGO_BIN_EXE_memo"))
                .parent()
                .unwrap(),
        )
        .env_remove("OPTMEM_CONFIG")
        .env("XDG_CONFIG_HOME", dir.join(".test-config"))
        .env("XDG_DATA_HOME", dir.join(".test-data"))
        .env_remove("MEMORY_DIR")
        .output()
        .unwrap();
    assert!(result.status.success(), "{}", stderr(&result));
    assert!(stdout(&result).contains("0-1 saved"));
}

#[test]
fn cli_paging_refuses_missing_summaries_and_uses_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    run(temp.path(), &["init"]);
    let source = temp.path().join("seed.txt");
    let text: String = (0..8).map(|i| format!("2026-10-07 memory {i}\n")).collect();
    fs::write(&source, text).unwrap();
    assert!(
        run(temp.path(), &["import", source.to_str().unwrap()])
            .status
            .success()
    );
    run(temp.path(), &["config", "WAKE_LINES=4", "PART_LINES=2"]);
    let result = run(temp.path(), &["wake"]);
    assert!(!result.status.success());
    assert!(stdout(&result).contains("Cannot wake"));
    for id in ["0-1", "2-3", "4-5", "6-7", "0-3", "4-7", "0-7"] {
        assert!(run(temp.path(), &["nap", id, "settled"]).status.success());
    }
    let first = run(temp.path(), &["wake"]);
    assert!(stdout(&first).contains("wake 2 8"));
    assert!(!stdout(&first).contains("You are awake."));
    run(temp.path(), &["note", "arrived between pages"]);
    let second = run(temp.path(), &["wake", "2", "8"]);
    assert!(second.status.success());
    assert!(stdout(&second).ends_with("You are awake.\n"));
    assert!(!stdout(&second).contains("arrived between pages"));
    assert!(!run(temp.path(), &["wake", "3", "8"]).status.success());
}

#[test]
fn saved_note_is_acknowledged_even_if_next_prompt_is_corrupt() {
    let temp = tempfile::tempdir().unwrap();
    run(temp.path(), &["init"]);
    let memory = optmem::OptMem::new(optmem::storage::FileStorage::open(temp.path()).unwrap());
    for i in 0..32 {
        memory
            .note(
                chrono::NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(),
                &format!("fact {i}"),
            )
            .unwrap();
    }
    loop {
        let request = memory.next_compression(None).unwrap().unwrap();
        if request.block.size() == 32 {
            break;
        }
        memory.submit_summary(request.block, "settled").unwrap();
    }
    let mut bytes = fs::read(temp.path().join("TREE/16")).unwrap();
    bytes[..287].fill(b' ');
    fs::write(temp.path().join("TREE/16"), bytes).unwrap();
    let result = run(temp.path(), &["note", "this was still saved"]);
    assert!(!result.status.success());
    assert!(stdout(&result).contains("Saved as #32."));
    assert!(stderr(&result).contains("forget 0-15"));
    assert_eq!(memory.len().unwrap(), 33);
}
