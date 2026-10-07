use std::{io::Cursor, sync::Arc};

use chrono::NaiveDate;
use optmem::{
    Block, ConfigKey, Error, MemoryView, OptMem, SummaryStatus, Wake, cover,
    storage::{FileStorage, MemoryStorage, Storage},
};

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 7).unwrap()
}
fn block(id: &str) -> Block {
    id.parse().unwrap()
}

fn settle<S: Storage>(memory: &OptMem<S>) -> usize {
    let mut settled = 0;
    while let Some(request) = memory.next_compression(None).unwrap() {
        assert_eq!(
            memory
                .submit_summary(request.block, "lasting facts")
                .unwrap(),
            SummaryStatus::Saved
        );
        settled += 1;
    }
    settled
}

fn exercise_backend<S: Storage>(store: S) {
    let memory = OptMem::new(store);
    assert!(memory.is_empty().unwrap());
    assert!(matches!(memory.wake(1, None).unwrap(), Wake::Ready(page) if page.entries.is_empty()));
    for i in 0..32 {
        assert_eq!(
            memory.note(day(), &format!("memory {i}: café")).unwrap().id,
            i
        );
    }
    memory
        .configure(&[(ConfigKey::WakeLines, Some(8))])
        .unwrap();
    assert!(matches!(
        memory.wake(1, None).unwrap(),
        Wake::NeedsCompression { .. }
    ));
    let request = memory.next_compression(None).unwrap().unwrap();
    assert_eq!(request.block, block("0-1"));
    assert_eq!(request.sources.len(), 2);
    assert_eq!(request.remaining, 30);
    assert!(matches!(
        memory.submit_summary(block("0-3"), "out of order"),
        Err(Error::WrongBlock { .. })
    ));
    assert_eq!(
        memory.submit_summary(block("0-1"), "first pair").unwrap(),
        SummaryStatus::Saved
    );
    assert_eq!(
        memory.submit_summary(block("0-1"), "overwrite").unwrap(),
        SummaryStatus::AlreadySettled
    );
    assert_eq!(settle(&memory), 30);
    let Wake::Ready(page) = memory.wake(1, None).unwrap() else {
        panic!("settled tree failed to wake");
    };
    assert_eq!(page.entries.len(), 8);
    assert_eq!(page.entries.last().unwrap().block().lo(), 31);
    assert_eq!(memory.recall("^#7 .*CAFÉ$").unwrap().total_matches, 1);
    assert_eq!(memory.recall("2026-10-07").unwrap().total_matches, 32);
    assert_eq!(
        memory.zoom(block("0-1")).unwrap()[0].to_string(),
        "#0 2026-10-07 memory 0: café"
    );
    assert_eq!(
        memory.zoom(block("0-3")).unwrap()[0].to_string(),
        "#0-1 first pair"
    );
    assert_eq!(
        memory.submit_summary(block("0-1"), "overwrite").unwrap(),
        SummaryStatus::NothingPending
    );
    let gone = memory.forget(block("0-7")).unwrap();
    assert_eq!(gone.len(), 7);
    assert_eq!(memory.len().unwrap(), 32);
    assert!(matches!(
        memory.zoom(block("0-15")).unwrap()[0],
        MemoryView::Summary { text: None, .. }
    ));
    assert!(matches!(
        memory.wake(1, None).unwrap(),
        Wake::NeedsCompression { .. }
    ));
    assert_eq!(settle(&memory), 7);
    assert!(matches!(memory.wake(1, None).unwrap(), Wake::Ready(_)));
    assert!(memory.forget(block("1024-1025")).is_err());
    assert!(memory.zoom(block("1024-2047")).is_err());
    memory.note(day(), "new tail").unwrap();
    assert_eq!(memory.zoom(block("32-33")).unwrap().len(), 1);
    assert_eq!(
        memory.zoom(block("32-63")).unwrap()[0].to_string(),
        "#32-47 not compressed yet"
    );
    assert!(memory.wake(1, Some(34)).is_err());
    assert_eq!(memory.pending_count(Some(4)).unwrap(), 0);
}

#[test]
fn in_memory_backend_conformance() {
    exercise_backend(MemoryStorage::new());
}

#[test]
fn file_backend_conformance() {
    let temp = tempfile::tempdir().unwrap();
    exercise_backend(FileStorage::initialize(temp.path()).unwrap());
}

#[test]
fn runtime_selected_storage_is_object_safe() {
    let backend: Box<dyn Storage> = Box::new(MemoryStorage::new());
    let memory = OptMem::new(backend);
    memory.note(day(), "no files involved").unwrap();
    assert_eq!(memory.len().unwrap(), 1);
}

#[test]
fn cover_invariants_across_shapes_and_budgets() {
    for count in (1..400).chain([1000, 4096, 10000, 65536, 100003, u64::MAX]) {
        for budget in [count.count_ones() as usize, 16, 96, 320] {
            if budget < count.count_ones() as usize {
                assert!(cover(count, budget).is_err());
                continue;
            }
            let blocks = cover(count, budget).unwrap();
            assert!(blocks.len() <= budget, "{count}, {budget}");
            assert_eq!(blocks[0].lo(), 0);
            assert_eq!(blocks.last().unwrap().hi(), count);
            for pair in blocks.windows(2) {
                assert_eq!(pair[0].hi(), pair[1].lo());
                assert!(
                    pair[0].size() >= pair[1].size(),
                    "detail reversed: {count}, {budget}"
                );
            }
            for b in blocks {
                assert!(b.size().is_power_of_two());
                assert_eq!(b.lo() % b.size(), 0);
            }
        }
    }
    assert!(cover(1, 0).is_err());
    assert!(matches!(
        cover(3, 1),
        Err(Error::InsufficientBudget { minimum: 2, .. })
    ));
    assert!(cover(0, 1).unwrap().is_empty());
}

#[test]
fn block_ids_are_validated_without_overflow() {
    for id in [
        "",
        "0",
        "0-0",
        "3-9",
        "5-6",
        "9-3",
        "0--1",
        "-1-0",
        "+0-1",
        "0-18446744073709551615",
        "0-1x",
    ] {
        assert!(id.parse::<Block>().is_err(), "{id}");
    }
    assert!(Block::new(2, 1).is_err());
    assert!(Block::new(0, 0).is_err());
    assert!(Block::new(0, 1).unwrap().halves().is_err());
    assert_eq!(block("16-31").hi(), 32);
}

#[test]
fn utf8_and_single_line_limits_are_byte_based() {
    let memory = OptMem::new(MemoryStorage::new());
    for text in [
        "",
        "   ",
        "one\ntwo",
        "one\rtwo",
        &"x".repeat(281),
        &"ã".repeat(150),
    ] {
        assert!(memory.note(day(), text).is_err());
    }
    assert_eq!(
        memory.note(day(), &"ã".repeat(140)).unwrap().text.len(),
        280
    );
    assert_eq!(memory.note(day(), "  café  ").unwrap().text, "café");
    assert!(memory.submit_summary(block("0-1"), "two\nlines").is_err());
    assert_eq!(memory.pending_count(None).unwrap(), 1);
    memory
        .configure(&[(ConfigKey::EntryChars, Some(10))])
        .unwrap();
    assert!(memory.note(day(), "longer than ten").is_err());
    assert!(
        memory
            .configure(&[(ConfigKey::EntryChars, Some(281))])
            .is_err()
    );
    assert!(
        memory
            .configure(&[(ConfigKey::WakeLines, Some(0))])
            .is_err()
    );
    memory.configure(&[(ConfigKey::EntryChars, None)]).unwrap();
    assert_eq!(memory.config().unwrap().entry_chars, 280);
}

#[test]
fn import_is_validated_before_any_append() {
    let memory = OptMem::new(MemoryStorage::new());
    for source in [
        "",
        "2027-99-99 impossible\n",
        "2026-10-07 good\n2026-10-06 backwards\n",
        "2026-10-07 good\n2026-10-08 \n",
        "2026-1-01 bad\n",
        "0000-01-01 bad\n",
    ] {
        assert!(memory.import(Cursor::new(source)).is_err());
        assert!(memory.is_empty().unwrap());
    }
    assert!(memory.import(Cursor::new(b"2026-10-07 caf\xe9")).is_err());
    let result = memory
        .import(Cursor::new(
            "\n2026-10-07 first\n2026-10-07 second\n2026-10-08 third\n",
        ))
        .unwrap();
    assert_eq!((result.first_id, result.count, result.pending), (0, 3, 1));
    assert!(memory.import(Cursor::new("2026-10-07 too old\n")).is_err());
    assert_eq!(memory.len().unwrap(), 3);
    assert!(
        memory
            .import(Cursor::new("2026-10-09 valid\n2026-10-10 no\rinternal\n"))
            .is_err()
    );
    assert_eq!(memory.len().unwrap(), 3);
}

#[test]
fn synthetic_life_and_snapshot_pagination() {
    let memory = OptMem::new(MemoryStorage::new());
    let source: String = (0..2000)
        .map(|i| format!("2026-10-07 memory number {i}: {}\n", "x".repeat(220)))
        .collect();
    memory.import(Cursor::new(source)).unwrap();
    let expected: u64 = (1..12).map(|i| 2000 / (1 << i)).sum();
    assert_eq!(memory.pending_count(None).unwrap(), expected);
    assert_eq!(settle(&memory) as u64, expected);
    memory
        .configure(&[
            (ConfigKey::PartLines, Some(5)),
            (ConfigKey::PartChars, Some(1500)),
        ])
        .unwrap();
    let Wake::Ready(first) = memory.wake(1, None).unwrap() else {
        panic!();
    };
    assert!(first.total_parts > 1);
    assert!(first.next_compression.is_none());
    memory.note(day(), "arrived during reading").unwrap();
    settle(&memory);
    let Wake::Ready(again) = memory.wake(1, Some(first.snapshot)).unwrap() else {
        panic!();
    };
    assert_eq!(first, again);
    let mut entries = Vec::new();
    for part in 1..=first.total_parts {
        let Wake::Ready(page) = memory.wake(part, Some(first.snapshot)).unwrap() else {
            panic!();
        };
        assert!(page.entries.len() <= 5);
        assert!(
            page.entries
                .iter()
                .map(|e| e.to_string().len() + 1)
                .sum::<usize>()
                <= 1500
        );
        entries.extend(page.entries);
    }
    assert_eq!(entries.len(), 96);
    assert_eq!(entries.last().unwrap().block().lo(), 1999);
    assert!(memory.wake(first.total_parts + 1, None).is_err());
    assert!(memory.wake(0, None).is_err());
}

#[test]
fn large_compressions_use_two_summaries_small_ones_use_raw() {
    let memory = OptMem::new(MemoryStorage::new());
    for i in 0..32 {
        memory.note(day(), &format!("raw {i}")).unwrap();
    }
    while let Some(request) = memory.next_compression(None).unwrap() {
        if request.block.size() <= 16 {
            assert_eq!(request.sources.len() as u64, request.block.size());
            assert!(
                request
                    .sources
                    .iter()
                    .all(|v| matches!(v, MemoryView::Raw(_)))
            );
        } else {
            assert_eq!(request.sources.len(), 2);
            assert!(
                request
                    .sources
                    .iter()
                    .all(|v| matches!(v, MemoryView::Summary { text: Some(_), .. }))
            );
        }
        memory.submit_summary(request.block, "summary").unwrap();
    }
}

#[test]
fn recall_retains_only_newest_bounded_matches() {
    let memory = OptMem::new(MemoryStorage::new());
    for i in 0..5000 {
        memory
            .note(day(), &format!("probe {i}: {}", "x".repeat(220)))
            .unwrap();
    }
    memory
        .configure(&[(ConfigKey::PartChars, Some(1000))])
        .unwrap();
    let hits = memory.recall("PROBE").unwrap();
    assert_eq!(hits.total_matches, 5000);
    assert!(hits.entries.len() < 5);
    assert_eq!(hits.entries.last().unwrap().id, 4999);
    assert!(
        hits.entries
            .iter()
            .map(|e| e.to_string().len() + 1)
            .sum::<usize>()
            <= 1000
    );
    assert_eq!(memory.recall("^#7 ").unwrap().entries[0].id, 7);
    assert_eq!(memory.recall("no such word").unwrap().total_matches, 0);
    assert!(memory.recall("[").is_err());
}

#[test]
fn in_memory_concurrent_writers_assign_unique_ids() {
    let memory = Arc::new(OptMem::new(MemoryStorage::new()));
    let handles: Vec<_> = (0..16)
        .map(|i| {
            let memory = memory.clone();
            std::thread::spawn(move || memory.note(day(), &format!("parallel {i}")).unwrap().id)
        })
        .collect();
    let mut ids: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    ids.sort_unstable();
    assert_eq!(ids, (0..16).collect::<Vec<_>>());
}

#[test]
fn config_changes_are_all_or_nothing_and_do_not_leak_between_stores() {
    let first = OptMem::new(MemoryStorage::new());
    let second = OptMem::new(MemoryStorage::new());
    assert!(
        first
            .configure(&[
                (ConfigKey::WakeLines, Some(10)),
                (ConfigKey::EntryChars, Some(999))
            ])
            .is_err()
    );
    assert_eq!(first.config().unwrap().wake_lines, 96);
    first
        .configure(&[(ConfigKey::WakeLines, Some(10))])
        .unwrap();
    assert_eq!(second.config().unwrap().wake_lines, 96);
}
