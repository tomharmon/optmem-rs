//! Optional differential tests against an unmodified original OptMem checkout.
//! OPTMEM_PYTHON=/path/to/OptMem/memo cargo test --test python_compat -- --ignored

use std::{fs, io::Cursor, process::Command};

use optmem::{ConfigKey, OptMem, Wake, cover, storage::FileStorage};

fn original() -> std::path::PathBuf {
    std::env::var_os("OPTMEM_PYTHON")
        .expect("set OPTMEM_PYTHON to the original memo script")
        .into()
}

#[test]
#[ignore = "requires Python 3 and OPTMEM_PYTHON"]
fn cover_matches_original() {
    let script = r#"import sys
from importlib.machinery import SourceFileLoader
m = SourceFileLoader('memo', sys.argv[1]).load_module()
for t in list(range(1,400)) + [1000,4096,10000,65536,100003]:
    for budget in [16,96,320]:
        print(t, budget, ','.join('%d-%d' % (lo,hi-1) for lo,hi in m.cover(t,budget)))
"#;
    let result = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(original())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for line in String::from_utf8(result.stdout).unwrap().lines() {
        let mut fields = line.splitn(3, ' ');
        let count = fields.next().unwrap().parse().unwrap();
        let budget = fields.next().unwrap().parse().unwrap();
        let rust = cover(count, budget)
            .unwrap()
            .iter()
            .map(|b| format!("{}-{}", b.lo(), b.hi() - 1))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(rust, fields.next().unwrap(), "T={count}, budget={budget}");
    }
}

#[test]
#[ignore = "requires Python 3 and OPTMEM_PYTHON"]
fn python_and_rust_share_log_tree_and_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let output = Command::new("python3")
            .arg(original())
            .args(args)
            .env("MEMORY_DIR", temp.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init"]);
    run(&["note", "a Python memory with café"]);
    let memory = OptMem::new(FileStorage::open(temp.path()).unwrap());
    assert_eq!(memory.recall("café").unwrap().total_matches, 1);
    let date = memory.recall("Python").unwrap().entries[0].date;
    let seed: String = (1..128)
        .map(|i| format!("{date} Rust memory {i}\n"))
        .collect();
    memory.import(Cursor::new(seed)).unwrap();
    while let Some(request) = memory.next_compression(None).unwrap() {
        if request.block.lo() % 4 == 0 {
            run(&["nap", &request.block.to_string(), "summary by Python"]);
        } else {
            memory
                .submit_summary(request.block, "summary by Rust")
                .unwrap();
        }
    }
    memory
        .configure(&[(ConfigKey::WakeLines, Some(16))])
        .unwrap();
    assert!(run(&["config"]).contains("16"));
    let Wake::Ready(page) = memory.wake(1, None).unwrap() else {
        panic!();
    };
    let rust: Vec<_> = page.entries.iter().map(ToString::to_string).collect();
    let python: Vec<_> = run(&["wake"])
        .lines()
        .filter(|l| l.starts_with('#'))
        .map(str::to_owned)
        .collect();
    assert_eq!(rust, python);
    assert!(run(&["recall", "^#127 "]).contains("Rust memory 127"));
    let before = fs::read(temp.path().join("LOG.txt")).unwrap();
    run(&["forget", "16-31"]);
    assert!(memory.pending_count(None).unwrap() > 0);
    while let Some(request) = memory.next_compression(None).unwrap() {
        memory
            .submit_summary(request.block, "rebuilt in Rust")
            .unwrap();
    }
    assert!(run(&["wake"]).contains("You are awake."));
    assert_eq!(fs::read(temp.path().join("LOG.txt")).unwrap(), before);
}
