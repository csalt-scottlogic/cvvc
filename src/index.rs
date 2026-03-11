use chrono::{DateTime, Utc};
use std::{cmp::Ordering, fmt::Display, iter::repeat_n, path::Path};

use self::errors::{InvalidIndexEntryError, InvalidIndexError};
use crate::helpers::{
    self, datetime_to_bytes,
    fs::{index_path_file, index_path_parent, FileMetadata},
};

mod errors;

#[derive(Debug)]
pub enum IndexEntryType {
    File,
    Symlink,
    Gitlink,
}

impl IndexEntryType {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            8 => Some(IndexEntryType::File),
            10 => Some(IndexEntryType::Symlink),
            14 => Some(IndexEntryType::Gitlink),
            _ => None,
        }
    }

    pub fn to_byte(&self) -> u8 {
        match self {
            IndexEntryType::File => 8,
            IndexEntryType::Symlink => 10,
            IndexEntryType::Gitlink => 14,
        }
    }
}

impl Display for IndexEntryType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            IndexEntryType::File => "regular file",
            IndexEntryType::Symlink => "symbolic link",
            IndexEntryType::Gitlink => "git link",
        };
        write!(f, "{str}")
    }
}

#[derive(Debug)]
pub enum IndexEntryPermissions {
    Executable,
    NonExecutable,
    Link,
}

impl IndexEntryPermissions {
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0o644 => Some(IndexEntryPermissions::NonExecutable),
            0o755 => Some(IndexEntryPermissions::Executable),
            0 => Some(IndexEntryPermissions::Link),
            _ => None,
        }
    }

    pub fn to_u16(&self) -> u16 {
        match self {
            IndexEntryPermissions::Link => 0,
            IndexEntryPermissions::NonExecutable => 0o644,
            IndexEntryPermissions::Executable => 0o755,
        }
    }
}

impl Display for IndexEntryPermissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            IndexEntryPermissions::Executable => "0755",
            IndexEntryPermissions::NonExecutable => "0644",
            IndexEntryPermissions::Link => "0000",
        };
        write!(f, "{str}")
    }
}

pub struct IndexEntry {
    pub ctime: DateTime<Utc>,
    pub mtime: DateTime<Utc>,
    pub dev: u32,
    pub ino: u32,
    pub mode_type: IndexEntryType,
    pub mode_perms: IndexEntryPermissions,
    pub uid: u32,
    pub gid: u32,
    pub fsize: u32,
    pub flag_assume_valid: bool,
    pub flag_stage: u8,
    pub object_id: String,
    pub object_name: String,
}

impl IndexEntry {
    pub fn byte_length(&self) -> usize {
        // Round up to 8-byte boundary
        let blocks = (self.object_name.len() + 63) / 8 + 1;
        blocks * 8
    }

    pub fn from_bytes(data: &[u8]) -> Result<IndexEntry, InvalidIndexEntryError> {
        // Shortest possible index entry length, for a single-character filename.
        if data.len() < 64 {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::TooShort,
            });
        }
        let ctime_s = helpers::u32_from_be_bytes_unchecked(data, 0);
        let ctime_ns = helpers::u32_from_be_bytes_unchecked(data, 4);
        let ctime = DateTime::<Utc>::from_timestamp(ctime_s.into(), ctime_ns);
        let Some(ctime) = ctime else {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnparseableTimestamp(ctime_s, ctime_ns),
            });
        };
        let mtime_s = helpers::u32_from_be_bytes_unchecked(data, 8);
        let mtime_ns = helpers::u32_from_be_bytes_unchecked(data, 12);
        let mtime = DateTime::<Utc>::from_timestamp(mtime_s.into(), mtime_ns);
        let Some(mtime) = mtime else {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnparseableTimestamp(mtime_s, mtime_ns),
            });
        };
        let dev = helpers::u32_from_be_bytes_unchecked(data, 16);
        let ino = helpers::u32_from_be_bytes_unchecked(data, 20);
        let mode = helpers::u16_from_be_bytes_unchecked(data, 26);
        let mode_type_val = mode >> 12;
        let mode_type = IndexEntryType::from_byte(mode_type_val as u8);
        let Some(mode_type) = mode_type else {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnexpectedMode(mode_type_val),
            });
        };
        let mode_perms = IndexEntryPermissions::from_u16(mode & 0x1FF);
        let Some(mode_perms) = mode_perms else {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::UnexpectedPermissions(mode & 0x1FF),
            });
        };
        let uid = helpers::u32_from_be_bytes_unchecked(data, 28);
        let gid = helpers::u32_from_be_bytes_unchecked(data, 32);
        let fsize = helpers::u32_from_be_bytes_unchecked(data, 36);
        let object_id = hex::encode(&data[40..60]);
        let flags = helpers::u16_from_be_bytes_unchecked(data, 60);
        let assume_valid = flags & 0x8000 != 0;
        let stage = u8::try_from((flags >> 12) & 3).unwrap();
        let name_len: usize = (flags & 0xFFF).into();
        if data.len() < name_len + 63 {
            return Err(InvalidIndexEntryError {
                error_kind: errors::InvalidIndexEntryKind::TooShort,
            });
        }
        let name = if name_len < 0xFFF {
            if data[name_len + 62] != 0 {
                return Err(InvalidIndexEntryError {
                    error_kind: errors::InvalidIndexEntryKind::NameNotNullTerminated,
                });
            }
            String::from_utf8_lossy(&data[62..(name_len + 62)])
        } else {
            let real_name_len = data[62..].iter().position(|x| *x == 0);
            let Some(real_name_len) = real_name_len else {
                return Err(InvalidIndexEntryError {
                    error_kind: errors::InvalidIndexEntryKind::NameNotNullTerminated,
                });
            };
            String::from_utf8_lossy(&data[62..(real_name_len + 62)])
        };
        Ok(IndexEntry {
            ctime,
            mtime,
            dev,
            ino,
            mode_type,
            mode_perms,
            uid,
            gid,
            fsize,
            flag_assume_valid: assume_valid,
            flag_stage: stage,
            object_id,
            object_name: name.to_string(),
        })
    }

    pub fn from_file(
        path: &Path,
        object_id: String,
        index_ready_name: String,
    ) -> Result<Self, anyhow::Error> {
        let metadata = FileMetadata::from_path(path)?;
        Ok(IndexEntry {
            ctime: metadata.ctime,
            mtime: metadata.mtime,
            dev: metadata.dev,
            ino: metadata.ino,
            mode_type: metadata.mode_type,
            mode_perms: metadata.mode_perms,
            uid: metadata.uid,
            gid: metadata.gid,
            fsize: metadata.fsize,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id,
            object_name: index_ready_name,
        })
    }

    pub fn serialise(&self, buf: &mut Vec<u8>) {
        buf.extend(datetime_to_bytes(&self.ctime));
        buf.extend(datetime_to_bytes(&self.mtime));
        buf.extend(self.dev.to_be_bytes());
        buf.extend(self.ino.to_be_bytes());
        let mode =
            u32::from((u16::from(self.mode_type.to_byte()) << 12) | self.mode_perms.to_u16());
        buf.extend(mode.to_be_bytes());
        buf.extend(self.uid.to_be_bytes());
        buf.extend(self.gid.to_be_bytes());
        buf.extend(self.fsize.to_be_bytes());
        let obj_id = hex::decode(&self.object_id);
        if let Ok(obj_id) = obj_id {
            buf.extend(obj_id.iter());
        } else {
            buf.extend(repeat_n(0_u8, 20));
        }
        let mut flags: u16 = if self.flag_assume_valid { 0x8000 } else { 0 };
        flags |= (self.flag_stage as u16) << 12;
        let capped_len: u16 = if self.object_name.len() > 0xfff {
            0xfff
        } else {
            self.object_name.len() as u16
        };
        flags |= capped_len;
        buf.extend(flags.to_be_bytes());
        buf.extend(self.object_name.bytes());
        buf.push(0);
        // The formula for computing the entry length only works on v2 indexes (that pesky hardcoded 63 I just perpetrated)
        buf.extend(repeat_n(0, 8 - ((self.object_name.len() + 63) % 8)));
    }

    pub fn object_directory_name(&self) -> &str {
        index_path_parent(&self.object_name)
    }

    pub fn object_file_name(&self) -> &str {
        index_path_file(&self.object_name)
    }

    pub fn mode(&self) -> u32 {
        let mt = match self.mode_type {
            IndexEntryType::File => 0o100000,
            IndexEntryType::Symlink => 0o120000,
            IndexEntryType::Gitlink => 0o160000,
        };
        let mp = match self.mode_perms {
            IndexEntryPermissions::NonExecutable => 0o644,
            IndexEntryPermissions::Executable => 0o755,
            IndexEntryPermissions::Link => 0,
        };
        mt | mp
    }
}

impl Ord for IndexEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.object_name.bytes().cmp(other.object_name.bytes()) {
            Ordering::Equal => self.flag_stage.cmp(&other.flag_stage),
            Ordering::Less => Ordering::Less,
            Ordering::Greater => Ordering::Greater,
        }
    }
}

impl PartialOrd for IndexEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for IndexEntry {
    fn eq(&self, rhs: &Self) -> bool {
        matches!(self.cmp(rhs), Ordering::Equal)
    }
}

impl Eq for IndexEntry {}

pub struct Index {
    pub version: u32,
    entries: Vec<IndexEntry>,
}

impl Index {
    pub fn new() -> Self {
        Index {
            version: 2,
            entries: Vec::<IndexEntry>::new(),
        }
    }

    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    pub fn from_bytes(data: &[u8]) -> Result<Index, InvalidIndexError> {
        if data.len() < 12 {
            return Err(InvalidIndexError {
                error_kind: errors::InvalidIndexKind::TooShort,
            });
        }
        if data[..4] != *b"DIRC" {
            return Err(InvalidIndexError {
                error_kind: errors::InvalidIndexKind::MissingMagic,
            });
        }
        let version = helpers::u32_from_be_bytes_unchecked(data, 4);
        if version != 2 {
            return Err(InvalidIndexError {
                error_kind: errors::InvalidIndexKind::UnsupportedVersion(version),
            });
        }
        let count = usize::try_from(helpers::u32_from_be_bytes_unchecked(data, 8)).unwrap();
        let mut entries = Vec::<IndexEntry>::with_capacity(count);
        let mut idx = 12;
        for _ in 0..count {
            let entry = IndexEntry::from_bytes(&data[idx..]);
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    return Err(InvalidIndexError {
                        error_kind: errors::InvalidIndexKind::InvalidEntry(e),
                    })
                }
            };
            idx += entry.byte_length();
            entries.push(entry);
        }
        Ok(Index { version, entries })
    }

    pub fn serialise(&self, buf: &mut Vec<u8>) {
        buf.extend(b"DIRC");
        buf.extend(self.version.to_be_bytes());
        let truncated_count = if self.entries.len() > (u32::MAX as usize) {
            u32::MAX
        } else {
            self.entries.len() as u32
        };
        buf.extend(truncated_count.to_be_bytes());
        for entry in &self.entries {
            entry.serialise(buf);
        }
    }

    pub fn contains_path(&self, path: &str) -> bool {
        self.entries.iter().any(|e| e.object_name == path)
    }

    pub fn remove(&mut self, path: &str) -> bool {
        let start_len = self.entries.len();
        self.entries.retain(|e| e.object_name != path);
        start_len > self.entries.len()
    }

    pub fn remove_not_present(&mut self, object_ids: &[String]) {
        self.entries.retain(|e| object_ids.contains(&e.object_id));
    }

    pub fn add_unsorted(&mut self, entry: IndexEntry) {
        self.entries.push(entry);
    }

    pub fn sort(&mut self) {
        self.entries.sort();
    }
}
