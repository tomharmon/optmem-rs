//! Permanent, append-only agent memory with a rebuildable binary summary tree.
//!
//! ```
//! use chrono::NaiveDate;
//! use optmem::{OptMem, storage::MemoryStorage, Wake};
//! let memory = OptMem::new(MemoryStorage::new());
//! memory.note(NaiveDate::from_ymd_opt(2026, 10, 7).unwrap(), "Use Rust for the library")?;
//! assert!(matches!(memory.wake(1, None)?, Wake::Ready(_)));
//! # Ok::<(), optmem::Error>(())
//! ```

mod blocks;
mod error;
mod model;
pub mod settings;
pub mod storage;

use std::{collections::VecDeque, io::BufRead};

use chrono::{Datelike, NaiveDate};
use regex::RegexBuilder;
use storage::{Storage, StorageTransaction};

pub use blocks::cover;
pub use error::{Error, Result};
pub use model::*;

pub const RAW_MAX: u64 = 16;

/// The storage-independent engine. Dates are supplied by the caller; no clock or
/// filesystem is consulted by the engine. Summarization is supplied by the agent.
pub struct OptMem<S: Storage> {
    storage: S,
    config: Option<Config>,
}

impl<S: Storage> OptMem<S> {
    pub fn new(storage: S) -> Self {
        Self {
            storage,
            config: None,
        }
    }

    /// Use caller-supplied settings without reading or writing backend config.
    /// Applications can load these from a user config; library callers need no files.
    pub fn with_config(storage: S, config: Config) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            storage,
            config: Some(config),
        })
    }

    fn transaction_config(&self, tx: &mut dyn StorageTransaction) -> Result<Config> {
        match &self.config {
            Some(config) => Ok(config.clone()),
            None => Config::from_overrides(&tx.read_config()?),
        }
    }
    pub fn storage(&self) -> &S {
        &self.storage
    }
    pub fn into_storage(self) -> S {
        self.storage
    }

    pub fn len(&self) -> Result<u64> {
        self.storage.transaction()?.entry_count()
    }
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    pub fn config(&self) -> Result<Config> {
        self.transaction_config(&mut *self.storage.transaction()?)
    }

    /// Backend-local overrides, excluding caller-supplied settings from with_config.
    pub fn config_overrides(&self) -> Result<ConfigOverrides> {
        self.storage.transaction()?.read_config()
    }

    /// Apply all changes together. `None` restores a key's default.
    pub fn configure(&self, changes: &[(ConfigKey, Option<usize>)]) -> Result<ConfigOverrides> {
        if self.config.is_some() {
            return Err(Error::Invalid(
                "Settings are supplied by the caller; update its config and reopen the engine."
                    .into(),
            ));
        }
        let mut tx = self.storage.transaction()?;
        let mut overrides = tx.read_config()?;
        for &(key, value) in changes {
            if let Some(value) = value {
                key.validate(value)?;
                overrides.insert(key, value);
            } else {
                overrides.remove(&key);
            }
        }
        tx.write_config(&overrides)?;
        Ok(overrides)
    }

    pub fn note(&self, date: NaiveDate, text: &str) -> Result<Memory> {
        validate_date(date)?;
        let mut tx = self.storage.transaction()?;
        let config = self.transaction_config(&mut *tx)?;
        let text = check_text(text, config.entry_chars)?;
        let id = tx.append_entries(&[NewMemory {
            date,
            text: text.clone(),
        }])?;
        Ok(Memory { id, date, text })
    }

    /// Import `YYYY-MM-DD <text>` lines. Validate the entire input before writing;
    /// dates must be nondecreasing, including against the current last entry.
    pub fn import<R: BufRead>(&self, source: R) -> Result<ImportResult> {
        let mut tx = self.storage.transaction()?;
        let config = self.transaction_config(&mut *tx)?;
        let count = tx.entry_count()?;
        let mut last = if count == 0 {
            None
        } else {
            Some(tx.read_entries(count - 1, count)?[0].date)
        };
        let mut entries = Vec::new();
        for (i, line) in source.lines().enumerate() {
            let line = line.map_err(|e| {
                Error::Invalid(format!("line {}: import must be UTF-8 text: {e}", i + 1))
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let problem = |e: Error| Error::Invalid(format!("line {}: {e}", i + 1));
            let (date, text) = line
                .split_once(' ')
                .ok_or_else(|| problem(Error::Invalid("expected 'YYYY-MM-DD <text>'.".into())))?;
            let date = parse_date(date).map_err(problem)?;
            if let Some(previous) = last
                && date < previous
            {
                return Err(problem(Error::Invalid(format!(
                    "date {date} precedes the previous memory ({previous})."
                ))));
            }
            entries.push(NewMemory {
                date,
                text: check_text(text, config.entry_chars).map_err(problem)?,
            });
            last = Some(date);
        }
        if entries.is_empty() {
            return Err(Error::Invalid("Import has no memories.".into()));
        }
        let first_id = tx.append_entries(&entries)?;
        let count = entries.len() as u64;
        let pending = pending_count(&mut *tx, first_id + count)?;
        Ok(ImportResult {
            first_id,
            count,
            pending,
        })
    }

    pub fn pending_count(&self, snapshot: Option<u64>) -> Result<u64> {
        let mut tx = self.storage.transaction()?;
        let count = snapshot_count(&mut *tx, snapshot)?;
        pending_count(&mut *tx, count)
    }

    pub fn next_compression(&self, snapshot: Option<u64>) -> Result<Option<CompressionRequest>> {
        let mut tx = self.storage.transaction()?;
        let config = self.transaction_config(&mut *tx)?;
        let count = snapshot_count(&mut *tx, snapshot)?;
        next_request(&mut *tx, count, &config)
    }

    /// A block may only be submitted at the next pending position. Resubmitting
    /// an already settled block is harmless and never overwrites its summary.
    pub fn submit_summary(&self, block: Block, text: &str) -> Result<SummaryStatus> {
        block.require_summary()?;
        let mut tx = self.storage.transaction()?;
        let config = self.transaction_config(&mut *tx)?;
        let count = tx.entry_count()?;
        let Some(next) = next_pending(&mut *tx, count)? else {
            return Ok(SummaryStatus::NothingPending);
        };
        if block != next {
            if tx.read_summary(block)?.is_some() {
                return Ok(SummaryStatus::AlreadySettled);
            }
            return Err(Error::WrongBlock {
                requested: block,
                next,
            });
        }
        let text = check_text(text, config.entry_chars)?;
        // Verify the source halves before accepting a large summary, even if a
        // caller submits without first asking for its compression request.
        if block.size() > RAW_MAX {
            compression_sources(&mut *tx, block)?;
        }
        tx.append_summary(block, &text)?;
        Ok(SummaryStatus::Saved)
    }

    /// Drop this level's suffix and all ancestor suffixes. The raw log is untouched.
    /// Ancestors are invalidated first so a failed truncate cannot leave a stale parent.
    pub fn forget(&self, block: Block) -> Result<Vec<Block>> {
        block.require_summary()?;
        let mut tx = self.storage.transaction()?;
        self.transaction_config(&mut *tx)?;
        let count = tx.entry_count()?;
        let mut levels = Vec::new();
        let mut gone = Vec::new();
        let mut size = block.size();
        while size <= count {
            let keep = block.lo() / size;
            let have = tx.summary_count(size)?;
            if have > keep {
                for i in keep..have {
                    gone.push(Block::new(i * size, (i + 1) * size)?);
                }
                levels.push((size, keep));
            }
            let Some(next) = size.checked_mul(2) else {
                break;
            };
            size = next;
        }
        if gone.is_empty() {
            return Err(Error::Invalid(format!("No summary at {block}.")));
        }
        for (size, keep) in levels.into_iter().rev() {
            tx.truncate_summaries(size, keep)?;
        }
        Ok(gone)
    }

    pub fn zoom(&self, block: Block) -> Result<Vec<MemoryView>> {
        let halves = block.halves()?;
        let mut tx = self.storage.transaction()?;
        self.transaction_config(&mut *tx)?;
        let count = tx.entry_count()?;
        if block.lo() >= count {
            return Err(Error::Invalid(format!(
                "#{block} is beyond the memory: it holds {count} memories. Run wake."
            )));
        }
        halves
            .into_iter()
            .filter(|b| b.lo() < count)
            .map(|b| view(&mut *tx, b))
            .collect()
    }

    /// Render a bounded page as of a fixed entry count. Pass the returned snapshot
    /// when reading subsequent pages, so notes arriving between pages cannot shift it.
    pub fn wake(&self, part: usize, snapshot: Option<u64>) -> Result<Wake> {
        let mut tx = self.storage.transaction()?;
        let config = self.transaction_config(&mut *tx)?;
        let count = snapshot_count(&mut *tx, snapshot)?;
        let mut entries = Vec::new();
        for block in cover(count, config.wake_lines)? {
            let entry = view(&mut *tx, block)?;
            if matches!(&entry, MemoryView::Summary { text: None, .. }) {
                let request = next_request(&mut *tx, count, &config)?.ok_or_else(|| {
                    Error::CorruptSummary {
                        block,
                        reason: "missing despite a complete summary level".into(),
                    }
                })?;
                return Ok(Wake::NeedsCompression {
                    missing: block,
                    request,
                });
            }
            entries.push(entry);
        }
        let pages = paginate(entries, &config);
        if part == 0 || part > pages.len() {
            return Err(Error::Invalid(format!(
                "No part {part}: the memory has {} parts. Run wake.",
                pages.len()
            )));
        }
        let next_compression = if part == pages.len() {
            next_request(&mut *tx, count, &config)?
        } else {
            None
        };
        Ok(Wake::Ready(WakePage {
            snapshot: count,
            part,
            total_parts: pages.len(),
            entries: pages.into_iter().nth(part - 1).expect("validated page"),
            next_compression,
        }))
    }

    /// Case-insensitive Rust regex search over full raw lines, including IDs and dates.
    /// Scan in bounded chunks and retain only the newest matches fitting PART_CHARS.
    pub fn recall(&self, pattern: &str) -> Result<Recall> {
        let regex = RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|e| Error::Invalid(format!("bad regex: {e}")))?;
        let mut tx = self.storage.transaction()?;
        let config = self.transaction_config(&mut *tx)?;
        let count = tx.entry_count()?;
        let mut matches = VecDeque::new();
        let mut total_matches = 0;
        let mut bytes = 0;
        let mut lo = 0;
        while lo < count {
            let hi = lo.saturating_add(4096).min(count);
            for entry in tx.read_entries(lo, hi)? {
                let line = entry.to_string();
                if regex.is_match(&line) {
                    total_matches += 1;
                    let size = line.len() + 1;
                    matches.push_back((entry, size));
                    bytes += size;
                    while bytes > config.part_chars {
                        bytes -= matches
                            .pop_front()
                            .expect("over-budget queue is nonempty")
                            .1;
                    }
                }
            }
            lo = hi;
        }
        Ok(Recall {
            entries: matches.into_iter().map(|(e, _)| e).collect(),
            total_matches,
        })
    }
}

fn check_text(text: &str, max_bytes: usize) -> Result<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(Error::Invalid(
            "Empty. A memory is one line of text.".into(),
        ));
    }
    if text.contains(['\n', '\r']) {
        return Err(Error::Invalid(
            "A memory is one line: merge multiple lines, or note them separately.".into(),
        ));
    }
    if text.len() > max_bytes {
        return Err(Error::Invalid(format!(
            "Too long: {} bytes, limit {max_bytes}. Compress it further.",
            text.len()
        )));
    }
    Ok(text.into())
}

fn validate_date(date: NaiveDate) -> Result<()> {
    if !(1..=9999).contains(&date.year()) {
        return Err(Error::Invalid(
            "Date must have a year from 0001 to 9999.".into(),
        ));
    }
    Ok(())
}

pub(crate) fn parse_date(s: &str) -> Result<NaiveDate> {
    if s.len() != 10
        || !s.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return Err(Error::Invalid(format!(
            "expected 'YYYY-MM-DD <text>', got: {s}"
        )));
    }
    let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| Error::Invalid(format!("{s} is not a real date.")))?;
    validate_date(date)?;
    Ok(date)
}

fn snapshot_count(tx: &mut dyn StorageTransaction, snapshot: Option<u64>) -> Result<u64> {
    let count = tx.entry_count()?;
    let snapshot = snapshot.unwrap_or(count);
    if snapshot > count {
        return Err(Error::FutureSnapshot { snapshot, count });
    }
    Ok(snapshot)
}

fn next_pending(tx: &mut dyn StorageTransaction, count: u64) -> Result<Option<Block>> {
    let mut size = 2;
    while size <= count {
        let have = tx.summary_count(size)?;
        if have < count / size {
            return Ok(Some(Block::new(have * size, (have + 1) * size)?));
        }
        let Some(next) = size.checked_mul(2) else {
            break;
        };
        size = next;
    }
    Ok(None)
}

fn pending_count(tx: &mut dyn StorageTransaction, count: u64) -> Result<u64> {
    let mut total = 0;
    let mut size = 2;
    while size <= count {
        total += (count / size).saturating_sub(tx.summary_count(size)?);
        let Some(next) = size.checked_mul(2) else {
            break;
        };
        size = next;
    }
    Ok(total)
}

fn view(tx: &mut dyn StorageTransaction, block: Block) -> Result<MemoryView> {
    if block.is_raw() {
        Ok(MemoryView::Raw(
            tx.read_entries(block.lo(), block.hi())?.remove(0),
        ))
    } else {
        Ok(MemoryView::Summary {
            block,
            text: tx.read_summary(block)?,
        })
    }
}

fn compression_sources(tx: &mut dyn StorageTransaction, block: Block) -> Result<Vec<MemoryView>> {
    if block.size() <= RAW_MAX {
        return Ok(tx
            .read_entries(block.lo(), block.hi())?
            .into_iter()
            .map(MemoryView::Raw)
            .collect());
    }
    block
        .halves()?
        .into_iter()
        .map(|half| {
            let text = tx
                .read_summary(half)?
                .ok_or_else(|| Error::CorruptSummary {
                    block: half,
                    reason: "blank or missing".into(),
                })?;
            Ok(MemoryView::Summary {
                block: half,
                text: Some(text),
            })
        })
        .collect()
}

fn next_request(
    tx: &mut dyn StorageTransaction,
    count: u64,
    config: &Config,
) -> Result<Option<CompressionRequest>> {
    let Some(block) = next_pending(tx, count)? else {
        return Ok(None);
    };
    Ok(Some(CompressionRequest {
        block,
        sources: compression_sources(tx, block)?,
        max_bytes: config.entry_chars,
        remaining: pending_count(tx, count)? - 1,
    }))
}

fn paginate(entries: Vec<MemoryView>, config: &Config) -> Vec<Vec<MemoryView>> {
    let mut pages = Vec::new();
    let mut page = Vec::new();
    let mut bytes = 0;
    for entry in entries {
        let size = entry.to_string().len() + 1;
        if !page.is_empty()
            && (page.len() >= config.part_lines || size > config.part_chars.saturating_sub(bytes))
        {
            pages.push(std::mem::take(&mut page));
            bytes = 0;
        }
        page.push(entry);
        bytes += size;
    }
    if !page.is_empty() || pages.is_empty() {
        pages.push(page);
    }
    pages
}
