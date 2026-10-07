use std::path::PathBuf;

use crate::Block;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("No memory at {0}. Create it with init, or point MEMORY_DIR at an existing store.")]
    MissingStore(PathBuf),
    #[error("{0}")]
    Invalid(String),
    #[error("The summary of #{block} is {reason}. Recover with: forget {block}")]
    CorruptSummary { block: Block, reason: String },
    #[error("Memory #{id} is corrupt: {reason}")]
    CorruptLog { id: u64, reason: String },
    #[error("Wrong block: {requested}. Blocks are built in order; the next is {next}. Run nap.")]
    WrongBlock { requested: Block, next: Block },
    #[error("T={snapshot}, but the log holds {count} memories. Run wake.")]
    FutureSnapshot { snapshot: u64, count: u64 },
    #[error("WAKE_LINES={budget} cannot cover this snapshot: at least {minimum} lines are needed.")]
    InsufficientBudget { budget: usize, minimum: usize },
    #[error("Storage error: {0}")]
    Storage(String),
}

pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
