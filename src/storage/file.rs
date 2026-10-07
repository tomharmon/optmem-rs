use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;

use crate::{
    Block, Config, ConfigKey, ConfigOverrides, Error, Memory, NewMemory, Result, error::io,
    parse_date,
};

use super::{Storage, StorageTransaction, check_level};

pub const LOG_RECORD_BYTES: u64 = 320;
pub const TREE_RECORD_BYTES: u64 = 288;

/// Fixed-width, UTF-8 storage compatible with the original Python OptMem.
/// All transactions take the same OS advisory lock, including across processes.
#[derive(Clone, Debug)]
pub struct FileStorage {
    root: PathBuf,
}

impl FileStorage {
    /// Open an existing identity. Never creates the root directory.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        match fs::metadata(&root) {
            Ok(meta) if meta.is_dir() => (),
            Ok(_) => {
                return Err(io(
                    root,
                    std::io::Error::from(std::io::ErrorKind::NotADirectory),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::MissingStore(root));
            }
            Err(e) => return Err(io(root, e)),
        }
        fs::create_dir_all(root.join("TREE")).map_err(|e| io(root.join("TREE"), e))?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("LOG.txt"))
            .map_err(|e| io(root.join("LOG.txt"), e))?;
        Ok(Self { root })
    }

    /// Create an identity explicitly; existing entries, summaries and config are preserved.
    pub fn initialize(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref();
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root).map_err(|e| io(root, e))?;
        Self::open(root)
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

struct Transaction<'a> {
    root: &'a Path,
    lock: File,
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock);
    }
}

impl Storage for FileStorage {
    fn transaction(&self) -> Result<Box<dyn StorageTransaction + '_>> {
        let path = self.root.join(".lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| io(&path, e))?;
        FileExt::lock_exclusive(&lock).map_err(|e| io(&path, e))?;
        Ok(Box::new(Transaction {
            root: &self.root,
            lock,
        }))
    }
}

fn count(path: &Path, width: u64) -> Result<u64> {
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(meta.len() / width),
        Ok(_) => Err(io(
            path,
            std::io::Error::from(std::io::ErrorKind::InvalidData),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(io(path, e)),
    }
}

fn offset(index: u64, width: u64) -> Result<u64> {
    index
        .checked_mul(width)
        .ok_or_else(|| Error::Storage("record offset overflow".into()))
}

fn padded(text: &str, width: u64) -> Result<Vec<u8>> {
    if text.len() as u64 >= width {
        return Err(Error::Invalid(format!(
            "Too long: {} bytes. The record holds {}.",
            text.len(),
            width - 1
        )));
    }
    let mut buf = text.as_bytes().to_vec();
    buf.resize(width as usize - 1, b' ');
    buf.push(b'\n');
    Ok(buf)
}

fn append(path: &Path, width: u64, records: &[Vec<u8>]) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| io(path, e))?;
    let len = file.metadata().map_err(|e| io(path, e))?.len();
    // A torn trailing record was never acknowledged. Remove it before appending.
    file.set_len(len - len % width).map_err(|e| io(path, e))?;
    file.seek(SeekFrom::End(0)).map_err(|e| io(path, e))?;
    for record in records {
        file.write_all(record).map_err(|e| io(path, e))?;
    }
    file.sync_all().map_err(|e| io(path, e))?;
    sync_dir(path.parent().expect("store file has parent"))
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| io(path, e))?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

impl Transaction<'_> {
    fn log(&self) -> PathBuf {
        self.root.join("LOG.txt")
    }
    fn level(&self, size: u64) -> Result<PathBuf> {
        check_level(size)?;
        Ok(self.root.join("TREE").join(size.to_string()))
    }
}

impl StorageTransaction for Transaction<'_> {
    fn entry_count(&mut self) -> Result<u64> {
        count(&self.log(), LOG_RECORD_BYTES)
    }

    fn read_entries(&mut self, lo: u64, hi: u64) -> Result<Vec<Memory>> {
        if lo > hi || hi > self.entry_count()? {
            return Err(Error::Invalid("Entry range is beyond the memory.".into()));
        }
        let path = self.log();
        let mut file = File::open(&path).map_err(|e| io(&path, e))?;
        file.seek(SeekFrom::Start(offset(lo, LOG_RECORD_BYTES)?))
            .map_err(|e| io(&path, e))?;
        let mut out = Vec::new();
        let mut record = [0; LOG_RECORD_BYTES as usize];
        for id in lo..hi {
            file.read_exact(&mut record).map_err(|e| io(&path, e))?;
            let bad = |reason: String| Error::CorruptLog { id, reason };
            let line = std::str::from_utf8(&record)
                .map_err(|e| bad(e.to_string()))?
                .trim_end();
            let mut fields = line.splitn(3, ' ');
            if fields.next() != Some(format!("#{id}").as_str()) {
                return Err(bad("record ID does not match its position".into()));
            }
            let date = parse_date(fields.next().unwrap_or("")).map_err(|e| bad(e.to_string()))?;
            let text = fields
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| bad("missing text".into()))?;
            out.push(Memory {
                id,
                date,
                text: text.into(),
            });
        }
        Ok(out)
    }

    fn append_entries(&mut self, entries: &[NewMemory]) -> Result<u64> {
        let base = self.entry_count()?;
        let records = entries
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let id = base
                    .checked_add(i as u64)
                    .ok_or_else(|| Error::Storage("memory ID overflow".into()))?;
                padded(&format!("#{id} {} {}", e.date, e.text), LOG_RECORD_BYTES)
            })
            .collect::<Result<Vec<_>>>()?;
        append(&self.log(), LOG_RECORD_BYTES, &records)?;
        Ok(base)
    }

    fn summary_count(&mut self, size: u64) -> Result<u64> {
        count(&self.level(size)?, TREE_RECORD_BYTES)
    }

    fn read_summary(&mut self, block: Block) -> Result<Option<String>> {
        let path = self.level(block.size())?;
        if block.lo() / block.size() >= count(&path, TREE_RECORD_BYTES)? {
            return Ok(None);
        }
        let mut file = File::open(&path).map_err(|e| io(&path, e))?;
        file.seek(SeekFrom::Start(offset(
            block.lo() / block.size(),
            TREE_RECORD_BYTES,
        )?))
        .map_err(|e| io(&path, e))?;
        let mut record = [0; TREE_RECORD_BYTES as usize];
        file.read_exact(&mut record).map_err(|e| io(&path, e))?;
        let text = std::str::from_utf8(&record)
            .map_err(|_| Error::CorruptSummary {
                block,
                reason: "not valid UTF-8".into(),
            })?
            .trim_end();
        if text.is_empty() {
            return Err(Error::CorruptSummary {
                block,
                reason: "blank".into(),
            });
        }
        Ok(Some(text.to_owned()))
    }

    fn append_summary(&mut self, block: Block, text: &str) -> Result<()> {
        let path = self.level(block.size())?;
        if count(&path, TREE_RECORD_BYTES)? != block.lo() / block.size() {
            return Err(Error::Storage(
                "summary append is not at the next slot".into(),
            ));
        }
        append(
            &path,
            TREE_RECORD_BYTES,
            &[padded(text, TREE_RECORD_BYTES)?],
        )
    }

    fn truncate_summaries(&mut self, size: u64, keep: u64) -> Result<()> {
        let path = self.level(size)?;
        let have = count(&path, TREE_RECORD_BYTES)?;
        if have > keep {
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .map_err(|e| io(&path, e))?;
            file.set_len(offset(keep, TREE_RECORD_BYTES)?)
                .map_err(|e| io(&path, e))?;
            file.sync_all().map_err(|e| io(&path, e))?;
        }
        Ok(())
    }

    fn read_config(&mut self) -> Result<ConfigOverrides> {
        let path = self.root.join("config");
        let source = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ConfigOverrides::new()),
            Err(e) => return Err(io(&path, e)),
        };
        let mut overrides = ConfigOverrides::new();
        for (index, line) in source.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let where_ =
                |e: Error| Error::Invalid(format!("{} line {}: {e}", path.display(), index + 1));
            let key: ConfigKey = key.trim().parse().map_err(where_)?;
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(where_(Error::Invalid(format!(
                    "{key} must be a positive whole number, not '{value}'."
                ))));
            }
            let value = value
                .parse::<usize>()
                .map_err(|_| where_(Error::Invalid(format!("{key} is too large."))))?;
            key.validate(value).map_err(where_)?;
            overrides.insert(key, value);
        }
        Ok(overrides)
    }

    fn write_config(&mut self, overrides: &ConfigOverrides) -> Result<()> {
        Config::from_overrides(overrides)?;
        let mut out = String::from(
            "# OptMem sizes. A commented line follows the tool's default.\n# Edit with memo config NAME=VALUE.\n\n",
        );
        for key in ConfigKey::ALL {
            let prefix = if overrides.contains_key(&key) {
                ""
            } else {
                "# "
            };
            let value = overrides.get(&key).copied().unwrap_or(key.default_value());
            out.push_str(&format!(
                "{prefix}{key} = {value} # {}\n",
                key.description()
            ));
        }
        let tmp = self.root.join(".config.tmp");
        let path = self.root.join("config");
        let mut file = File::create(&tmp).map_err(|e| io(&tmp, e))?;
        file.write_all(out.as_bytes()).map_err(|e| io(&tmp, e))?;
        file.sync_all().map_err(|e| io(&tmp, e))?;
        drop(file);
        fs::rename(&tmp, &path).map_err(|e| io(&path, e))?;
        sync_dir(self.root)
    }
}
