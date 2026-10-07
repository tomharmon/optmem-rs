use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard},
};

use crate::{Block, Config, ConfigOverrides, Error, Memory, NewMemory, Result};

use super::{Storage, StorageTransaction, check_level};

/// A thread-safe backend with no filesystem access. Share with `Arc` for one identity.
#[derive(Default)]
pub struct MemoryStorage {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    entries: Vec<Memory>,
    summaries: BTreeMap<u64, Vec<String>>,
    config: ConfigOverrides,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

struct Transaction<'a> {
    state: MutexGuard<'a, State>,
}

impl Storage for MemoryStorage {
    fn transaction(&self) -> Result<Box<dyn StorageTransaction + '_>> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::Storage("in-memory lock poisoned".into()))?;
        Ok(Box::new(Transaction { state }))
    }
}

impl StorageTransaction for Transaction<'_> {
    fn entry_count(&mut self) -> Result<u64> {
        Ok(self.state.entries.len() as u64)
    }
    fn read_entries(&mut self, lo: u64, hi: u64) -> Result<Vec<Memory>> {
        if lo > hi || hi > self.state.entries.len() as u64 {
            return Err(Error::Invalid("Entry range is beyond the memory.".into()));
        }
        Ok(self.state.entries[lo as usize..hi as usize].to_vec())
    }
    fn append_entries(&mut self, entries: &[NewMemory]) -> Result<u64> {
        let base = self.state.entries.len() as u64;
        self.state
            .entries
            .extend(entries.iter().enumerate().map(|(i, e)| Memory {
                id: base + i as u64,
                date: e.date,
                text: e.text.clone(),
            }));
        Ok(base)
    }
    fn summary_count(&mut self, size: u64) -> Result<u64> {
        check_level(size)?;
        Ok(self
            .state
            .summaries
            .get(&size)
            .map_or(0, |v| v.len() as u64))
    }
    fn read_summary(&mut self, block: Block) -> Result<Option<String>> {
        block.require_summary()?;
        Ok(self
            .state
            .summaries
            .get(&block.size())
            .and_then(|v| v.get((block.lo() / block.size()) as usize))
            .cloned())
    }
    fn append_summary(&mut self, block: Block, text: &str) -> Result<()> {
        block.require_summary()?;
        let level = self.state.summaries.entry(block.size()).or_default();
        if level.len() as u64 != block.lo() / block.size() {
            return Err(Error::Storage(
                "summary append is not at the next slot".into(),
            ));
        }
        level.push(text.to_owned());
        Ok(())
    }
    fn truncate_summaries(&mut self, size: u64, keep: u64) -> Result<()> {
        check_level(size)?;
        if let Some(level) = self.state.summaries.get_mut(&size)
            && keep < level.len() as u64
        {
            level.truncate(keep as usize);
        }
        Ok(())
    }
    fn read_config(&mut self) -> Result<ConfigOverrides> {
        Ok(self.state.config.clone())
    }
    fn write_config(&mut self, overrides: &ConfigOverrides) -> Result<()> {
        Config::from_overrides(overrides)?;
        self.state.config = overrides.clone();
        Ok(())
    }
}
