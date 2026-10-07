use std::{
    fs::{self, OpenOptions},
    io::Write,
};

use chrono::NaiveDate;
use optmem::{
    Block, ConfigKey, Error, OptMem,
    storage::{FileStorage, Storage},
};

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 7).unwrap()
}
fn block(id: &str) -> Block {
    id.parse().unwrap()
}
fn settle(memory: &OptMem<FileStorage>) {
    while let Some(request) = memory.next_compression(None).unwrap() {
        memory.submit_summary(request.block, "settled").unwrap();
    }
}

#[test]
fn missing_identity_is_not_silently_created() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("typo");
    assert!(matches!(
        FileStorage::open(&missing),
        Err(Error::MissingStore(_))
    ));
    assert!(!missing.exists());
}

#[test]
fn original_fixed_width_format_is_readable_and_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let store = FileStorage::initialize(temp.path()).unwrap();
    let mut record = "#0 2026-10-07 reunião com João".as_bytes().to_vec();
    record.resize(319, b' ');
    record.push(b'\n');
    fs::write(temp.path().join("LOG.txt"), &record).unwrap();
    let memory = OptMem::new(store);
    assert_eq!(
        memory.recall("João").unwrap().entries[0].text,
        "reunião com João"
    );
    memory.note(day(), "second note").unwrap();
    memory
        .submit_summary(block("0-1"), "summary of both")
        .unwrap();
    let log = fs::read(temp.path().join("LOG.txt")).unwrap();
    assert_eq!(log.len(), 640);
    assert_eq!(&log[..320], &record);
    assert!(log[320..].starts_with(b"#1 2026-10-07 second note"));
    let summary = fs::read(temp.path().join("TREE/2")).unwrap();
    assert_eq!(summary.len(), 288);
    assert!(summary.starts_with(b"summary of both"));
    assert_eq!(summary[287], b'\n');
    drop(memory);
    let reopened = OptMem::new(FileStorage::open(temp.path()).unwrap());
    assert_eq!(reopened.len().unwrap(), 2);
    assert_eq!(
        reopened.zoom(block("0-3")).unwrap()[0].to_string(),
        "#0-1 summary of both"
    );
}

#[test]
fn init_is_idempotent_and_forget_never_modifies_the_log() {
    let temp = tempfile::tempdir().unwrap();
    let memory = OptMem::new(FileStorage::initialize(temp.path()).unwrap());
    for i in 0..8 {
        memory.note(day(), &format!("item {i}")).unwrap();
    }
    settle(&memory);
    memory
        .configure(&[(ConfigKey::WakeLines, Some(4))])
        .unwrap();
    let log = fs::read(temp.path().join("LOG.txt")).unwrap();
    let config = fs::read(temp.path().join("config")).unwrap();
    let summary = fs::read(temp.path().join("TREE/8")).unwrap();
    FileStorage::initialize(temp.path()).unwrap();
    assert_eq!(fs::read(temp.path().join("LOG.txt")).unwrap(), log);
    assert_eq!(fs::read(temp.path().join("config")).unwrap(), config);
    assert_eq!(fs::read(temp.path().join("TREE/8")).unwrap(), summary);
    memory.forget(block("2-3")).unwrap();
    assert_eq!(fs::read(temp.path().join("LOG.txt")).unwrap(), log);
    assert_eq!(fs::metadata(temp.path().join("TREE/2")).unwrap().len(), 288);
    assert_eq!(fs::metadata(temp.path().join("TREE/4")).unwrap().len(), 0);
    assert_eq!(fs::metadata(temp.path().join("TREE/8")).unwrap().len(), 0);
    settle(&memory);
    assert_eq!(
        fs::metadata(temp.path().join("TREE/2")).unwrap().len(),
        4 * 288
    );
}

#[test]
fn torn_log_and_summary_records_are_repaired_before_appending() {
    let temp = tempfile::tempdir().unwrap();
    let memory = OptMem::new(FileStorage::initialize(temp.path()).unwrap());
    memory.note(day(), "first").unwrap();
    OpenOptions::new()
        .append(true)
        .open(temp.path().join("LOG.txt"))
        .unwrap()
        .write_all(b"#99 partial")
        .unwrap();
    assert_eq!(memory.note(day(), "after crash").unwrap().id, 1);
    assert_eq!(
        fs::metadata(temp.path().join("LOG.txt")).unwrap().len(),
        640
    );
    fs::write(temp.path().join("TREE/2"), b"partial summary").unwrap();
    memory.submit_summary(block("0-1"), "both").unwrap();
    assert_eq!(fs::metadata(temp.path().join("TREE/2")).unwrap().len(), 288);
    assert_eq!(
        memory.zoom(block("0-3")).unwrap()[0].to_string(),
        "#0-1 both"
    );
}

#[test]
fn corrupt_summaries_report_a_recoverable_block() {
    let temp = tempfile::tempdir().unwrap();
    let memory = OptMem::new(FileStorage::initialize(temp.path()).unwrap());
    for i in 0..4 {
        memory.note(day(), &format!("item {i}")).unwrap();
    }
    settle(&memory);
    memory
        .configure(&[(ConfigKey::WakeLines, Some(2))])
        .unwrap();
    let mut level = fs::read(temp.path().join("TREE/2")).unwrap();
    level[..287].fill(b' ');
    fs::write(temp.path().join("TREE/2"), &level).unwrap();
    assert!(
        matches!(memory.wake(1, None), Err(Error::CorruptSummary { block: b, .. }) if b == block("0-1"))
    );
    assert!(
        memory
            .wake(1, None)
            .unwrap_err()
            .to_string()
            .contains("forget 0-1")
    );
    memory.forget(block("0-1")).unwrap();
    settle(&memory);
    level[0] = 0xff;
    fs::write(temp.path().join("TREE/2"), &level).unwrap();
    assert!(matches!(
        memory.zoom(block("0-3")),
        Err(Error::CorruptSummary { .. })
    ));
}

#[test]
fn corrupt_half_prevents_large_summary_submission() {
    let temp = tempfile::tempdir().unwrap();
    let memory = OptMem::new(FileStorage::initialize(temp.path()).unwrap());
    for i in 0..32 {
        memory.note(day(), &format!("item {i}")).unwrap();
    }
    while memory.next_compression(None).unwrap().unwrap().block != block("0-31") {
        let b = memory.next_compression(None).unwrap().unwrap().block;
        memory.submit_summary(b, "settled").unwrap();
    }
    let mut level = fs::read(temp.path().join("TREE/16")).unwrap();
    level[..287].fill(b' ');
    fs::write(temp.path().join("TREE/16"), level).unwrap();
    assert!(matches!(
        memory.next_compression(None),
        Err(Error::CorruptSummary { .. })
    ));
    assert!(matches!(
        memory.submit_summary(block("0-31"), "bad parent"),
        Err(Error::CorruptSummary { .. })
    ));
    assert!(!temp.path().join("TREE/32").exists());
}

#[test]
fn bad_config_names_the_file_and_line() {
    let temp = tempfile::tempdir().unwrap();
    let memory = OptMem::new(FileStorage::initialize(temp.path()).unwrap());
    fs::write(temp.path().join("config"), "# comment\nWAKE_LNES=100\n").unwrap();
    let error = memory.config().unwrap_err().to_string();
    assert!(error.contains("config line 2"));
    assert!(error.contains("WAKE_LNES"));
    fs::write(temp.path().join("config"), "ENTRY_CHARS=999\n").unwrap();
    assert!(memory.note(day(), "test").is_err());
    assert_eq!(memory.len().unwrap(), 0);
}

#[test]
fn backend_rejects_overwrite_and_skipped_summary_slots() {
    let temp = tempfile::tempdir().unwrap();
    let storage = FileStorage::initialize(temp.path()).unwrap();
    let mut tx = storage.transaction().unwrap();
    assert!(tx.append_summary(block("2-3"), "skip").is_err());
    tx.append_summary(block("0-1"), "original").unwrap();
    assert!(tx.append_summary(block("0-1"), "overwrite").is_err());
    assert_eq!(tx.read_summary(block("0-1")).unwrap().unwrap(), "original");
}
