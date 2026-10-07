use std::{collections::BTreeMap, fmt, str::FromStr};

use chrono::NaiveDate;

use crate::{Error, Result};

/// An aligned, power-of-two half-open range. Singletons represent raw memories.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Block {
    lo: u64,
    hi: u64,
}

impl Block {
    pub fn new(lo: u64, hi: u64) -> Result<Self> {
        let size = hi.saturating_sub(lo);
        if !size.is_power_of_two() || !lo.is_multiple_of(size) {
            return Err(Error::Invalid(format!(
                "[{lo},{hi}) is not an aligned power-of-two block."
            )));
        }
        Ok(Self { lo, hi })
    }

    pub fn lo(self) -> u64 {
        self.lo
    }
    pub fn hi(self) -> u64 {
        self.hi
    }
    pub fn size(self) -> u64 {
        self.hi - self.lo
    }
    pub fn is_raw(self) -> bool {
        self.size() == 1
    }

    pub fn halves(self) -> Result<[Self; 2]> {
        self.require_summary()?;
        let mid = self.lo + self.size() / 2;
        Ok([
            Self {
                lo: self.lo,
                hi: mid,
            },
            Self {
                lo: mid,
                hi: self.hi,
            },
        ])
    }

    pub(crate) fn require_summary(self) -> Result<()> {
        if self.is_raw() {
            Err(Error::Invalid(
                "A summary block must contain at least two memories.".into(),
            ))
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_raw() {
            write!(f, "{}", self.lo)
        } else {
            write!(f, "{}-{}", self.lo, self.hi - 1)
        }
    }
}

/// CLI block IDs are inclusive at both ends, e.g. `16-31`.
impl FromStr for Block {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        let bad = || {
            Error::Invalid(format!(
                "'{value}' is not a block id. Copy one printed by wake, like 16-31."
            ))
        };
        let (lo, hi) = value.split_once('-').ok_or_else(bad)?;
        if lo.is_empty()
            || hi.is_empty()
            || !lo.bytes().chain(hi.bytes()).all(|c| c.is_ascii_digit())
        {
            return Err(bad());
        }
        let lo = lo.parse::<u64>().map_err(|_| bad())?;
        let hi = hi
            .parse::<u64>()
            .map_err(|_| bad())?
            .checked_add(1)
            .ok_or_else(bad)?;
        let block = Self::new(lo, hi).map_err(|_| bad())?;
        block.require_summary().map_err(|_| bad())?;
        Ok(block)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Memory {
    pub id: u64,
    pub date: NaiveDate,
    pub text: String,
}

impl fmt::Display for Memory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{} {} {}", self.id, self.date, self.text)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewMemory {
    pub date: NaiveDate,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MemoryView {
    Raw(Memory),
    Summary { block: Block, text: Option<String> },
}

impl MemoryView {
    pub fn block(&self) -> Block {
        match self {
            Self::Raw(memory) => Block {
                lo: memory.id,
                hi: memory.id + 1,
            },
            Self::Summary { block, .. } => *block,
        }
    }
}

impl fmt::Display for MemoryView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(memory) => memory.fmt(f),
            Self::Summary { block, text } => write!(
                f,
                "#{block} {}",
                text.as_deref().unwrap_or("not compressed yet")
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ConfigKey {
    WakeLines,
    EntryChars,
    PartChars,
    PartLines,
}

impl ConfigKey {
    pub const ALL: [Self; 4] = [
        Self::WakeLines,
        Self::EntryChars,
        Self::PartChars,
        Self::PartLines,
    ];
    pub fn default_value(self) -> usize {
        match self {
            Self::WakeLines => 96,
            Self::EntryChars => 280,
            Self::PartChars => 20_000,
            Self::PartLines => 500,
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::WakeLines => "the memory context: how many lines wake prints",
            Self::EntryChars => "the longest one memory may be, in bytes",
            Self::PartChars => "output paging: largest part, in bytes",
            Self::PartLines => "output paging: largest part, in lines",
        }
    }
    pub fn validate(self, value: usize) -> Result<()> {
        if value == 0 {
            return Err(Error::Invalid(format!(
                "{self} must be a positive whole number."
            )));
        }
        if self == Self::EntryChars && value > 280 {
            return Err(Error::Invalid(
                "ENTRY_CHARS is at most 280: a memory has to fit the fixed-width records.".into(),
            ));
        }
        Ok(())
    }
}

impl fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::WakeLines => "WAKE_LINES",
            Self::EntryChars => "ENTRY_CHARS",
            Self::PartChars => "PART_CHARS",
            Self::PartLines => "PART_LINES",
        })
    }
}

impl FromStr for ConfigKey {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "WAKE_LINES" => Ok(Self::WakeLines),
            "ENTRY_CHARS" => Ok(Self::EntryChars),
            "PART_CHARS" => Ok(Self::PartChars),
            "PART_LINES" => Ok(Self::PartLines),
            _ => Err(Error::Invalid(format!(
                "{s} is not a size. Name one of WAKE_LINES, ENTRY_CHARS, PART_CHARS, PART_LINES."
            ))),
        }
    }
}

pub type ConfigOverrides = BTreeMap<ConfigKey, usize>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Config {
    pub wake_lines: usize,
    pub entry_chars: usize,
    pub part_chars: usize,
    pub part_lines: usize,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        for (key, value) in [
            (ConfigKey::WakeLines, self.wake_lines),
            (ConfigKey::EntryChars, self.entry_chars),
            (ConfigKey::PartChars, self.part_chars),
            (ConfigKey::PartLines, self.part_lines),
        ] {
            key.validate(value)?;
        }
        Ok(())
    }

    pub fn from_overrides(overrides: &ConfigOverrides) -> Result<Self> {
        for (&key, &value) in overrides {
            key.validate(value)?;
        }
        let get = |k: ConfigKey| overrides.get(&k).copied().unwrap_or(k.default_value());
        Ok(Self {
            wake_lines: get(ConfigKey::WakeLines),
            entry_chars: get(ConfigKey::EntryChars),
            part_chars: get(ConfigKey::PartChars),
            part_lines: get(ConfigKey::PartLines),
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_overrides(&BTreeMap::new()).expect("valid defaults")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompressionRequest {
    pub block: Block,
    pub sources: Vec<MemoryView>,
    pub max_bytes: usize,
    pub remaining: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WakePage {
    pub snapshot: u64,
    pub part: usize,
    pub total_parts: usize,
    pub entries: Vec<MemoryView>,
    /// Offered only on the last page; it may be work this cover does not need.
    pub next_compression: Option<CompressionRequest>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Wake {
    Ready(WakePage),
    NeedsCompression {
        missing: Block,
        request: CompressionRequest,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SummaryStatus {
    Saved,
    AlreadySettled,
    NothingPending,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Recall {
    pub entries: Vec<Memory>,
    pub total_matches: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportResult {
    pub first_id: u64,
    pub count: u64,
    pub pending: u64,
}
