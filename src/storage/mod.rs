//! Backend contract. Algorithms depend only on these logical records, never on files.
//!
//! A transaction is an exclusive, consistent view across entries, summaries and config.
//! It lasts until its guard is dropped. This is a locking scope, not a rollback journal:
//! writes must be durable when they return; errors may leave earlier completed writes.
//! Implementations must serialize transactions across all handles to the same store.
//! Raw IDs are contiguous and immutable; each summary level is a dense prefix in block
//! order. Missing summaries return `None`; corrupt records and I/O failures are errors.
//! The engine validates all input before calling write methods. Direct callers of
//! these low-level methods are responsible for the same date, text and range checks.

mod file;
mod memory;

use crate::{Block, ConfigOverrides, Memory, NewMemory, Result};

pub use file::{FileStorage, LOG_RECORD_BYTES, TREE_RECORD_BYTES};
pub use memory::MemoryStorage;

/// Object-safe storage interface, usable as `Box<dyn Storage>` or `Arc<dyn Storage>`.
pub trait Storage: Send + Sync {
    fn transaction(&self) -> Result<Box<dyn StorageTransaction + '_>>;
}

pub trait StorageTransaction {
    fn entry_count(&mut self) -> Result<u64>;
    /// Return exactly `[lo,hi)`, in ID order; out-of-bounds ranges are errors.
    fn read_entries(&mut self, lo: u64, hi: u64) -> Result<Vec<Memory>>;
    /// Append an already validated batch and assign IDs inside this transaction.
    fn append_entries(&mut self, entries: &[NewMemory]) -> Result<u64>;
    fn summary_count(&mut self, size: u64) -> Result<u64>;
    fn read_summary(&mut self, block: Block) -> Result<Option<String>>;
    /// Append only at the level's next slot; reject attempts to overwrite or skip.
    fn append_summary(&mut self, block: Block, text: &str) -> Result<()>;
    /// Remove a suffix of one level, leaving at most `keep` records.
    fn truncate_summaries(&mut self, size: u64, keep: u64) -> Result<()>;
    fn read_config(&mut self) -> Result<ConfigOverrides>;
    fn write_config(&mut self, overrides: &ConfigOverrides) -> Result<()>;
}

impl<T: Storage + ?Sized> Storage for Box<T> {
    fn transaction(&self) -> Result<Box<dyn StorageTransaction + '_>> {
        (**self).transaction()
    }
}

impl<T: Storage + ?Sized> Storage for std::sync::Arc<T> {
    fn transaction(&self) -> Result<Box<dyn StorageTransaction + '_>> {
        (**self).transaction()
    }
}

pub(crate) fn check_level(size: u64) -> Result<()> {
    if size < 2 || !size.is_power_of_two() {
        return Err(crate::Error::Invalid(format!(
            "{size} is not a summary level."
        )));
    }
    Ok(())
}
