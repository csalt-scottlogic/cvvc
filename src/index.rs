use chrono::{DateTime, Utc};
use std::{cmp::Ordering, fmt::Display, iter::repeat_n, path::Path};

use self::errors::{
    InvalidIndexEntryError, InvalidIndexEntryKind, InvalidIndexError, InvalidIndexKind,
};
use crate::helpers::{
    self, datetime_to_bytes,
    fs::{index_path_file, index_path_parent, FileMetadata},
};

/// Index parse errors
pub mod errors;

/// The file type of an index entry.
#[derive(Debug)]
pub enum IndexEntryType {
    /// Regular file
    File,

    /// Symbolic link
    Symlink,

    /// Git submodule
    Gitlink,
}

impl IndexEntryType {
    /// Parse an [`IndexEntryType`] from a byte
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            8 => Some(IndexEntryType::File),
            10 => Some(IndexEntryType::Symlink),
            14 => Some(IndexEntryType::Gitlink),
            _ => None,
        }
    }

    /// Convert an [`IndexEntryType`] value to a byte
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

/// The permissions of an index entry
#[derive(Debug)]
pub enum IndexEntryPermissions {
    Executable,
    NonExecutable,
    Link,
}

impl IndexEntryPermissions {
    /// Parse an [`IndexEntryPermissions`] value from a [`u16`].  
    /// If the value is not zero, octal 644 or octal 755, return `None`
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            0o644 => Some(IndexEntryPermissions::NonExecutable),
            0o755 => Some(IndexEntryPermissions::Executable),
            0 => Some(IndexEntryPermissions::Link),
            _ => None,
        }
    }

    /// Convert an [`IndexEntryPermissions`] value to a [`u16`].
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

/// The contents of a single index entry.
///
/// On Windows, the `dev`, `ino`, `uid` and `gid` fields are not populated, and the `ctime` field has a
/// different meaning to other platforms.  On other operating systems, these fields also may not be
/// populated, depending on the type of filesystem the repository is stored on.  This is unlikely to cause
/// users issues as indexes should not be shared across repositories even if they are clones of each other.
#[derive(Debug)]
pub struct IndexEntry {
    /// On Windows, the file creation time.  On other operating systems, the file metadata change time, if supported.
    pub ctime: DateTime<Utc>,

    /// File modification time
    pub mtime: DateTime<Utc>,

    /// Device number, if supported
    pub dev: u32,

    /// File inode number, if supported
    pub ino: u32,

    /// File type
    pub mode_type: IndexEntryType,

    /// File permissions, largely limited to the executable bit.
    pub mode_perms: IndexEntryPermissions,

    /// User ID of file owner, if supported
    pub uid: u32,

    /// Group ID of file's owning group, if supported
    pub gid: u32,

    /// File size, or [`u32::MAX`] if the file size is larger than that value
    pub fsize: u32,

    /// Within git, this flag is used to indicate that the file should not be checked for changes.
    /// CVVC does not at present support this functionality and sets this flag to `false` for all new
    /// index entries.
    pub flag_assume_valid: bool,

    /// Within git, this flag is used to disambiguate unmerged items during a merge, where the same
    /// path can potentially point to different objects.  At present it is not used within CVVC
    pub flag_stage: u8,

    /// Object ID
    pub object_id: String,

    /// Object path, relative to the worktree, using the ASCII `/` character (charpoint 47) as the path separator
    pub object_name: String,
}

impl IndexEntry {
    /// The length of this [`IndexEntry`] when serialised, including trailing padding (if needed) which rounds the size up
    /// to a multiple of 8 bytes.
    pub fn byte_length(&self) -> usize {
        let raw_length = self.object_name.len() + 63;
        // Round up to 8-byte boundary
        let blocks = if raw_length % 8 != 0 {
            (self.object_name.len() + 63) / 8 + 1
        } else {
            raw_length / 8
        };
        blocks * 8
    }

    /// Parse a byte array as an [`IndexEntry`].
    ///
    /// # Errors
    ///
    /// If this function fails, it returns an [`InvalidIndexEntryError`].  Its [`InvalidIndexEntryError::error_kind`] field gives the underlying cause of the error, as follows:
    ///
    /// - if the array is too short to contain an entry with a name at least one byte in length, or if it is too short to contain a name of the length
    /// specified in the name length field, it returns [`InvalidIndexEntryKind::TooShort`]
    /// - if the `ctime` or `mtime` fields are not valid timestamps, it returns [`InvalidIndexEntryKind::UnparseableTimestamp`]
    /// - if the `mode_type` field is not one of the permitted values expressed by [`IndexEntryType`], it returns [`InvalidIndexEntryKind::UnexpectedMode`]
    /// - if the `mode_permissions` field is not zero, octal 644 or octal 755, it returns [`InvalidIndexEntryKind::UnexpectedPermissions`]
    /// - if the name string is not properly terminated, or the name length field is shorter than the actual name length, it returns [`InvalidIndexEntryKind::NameNotNullTerminated`]
    pub fn from_bytes(data: &[u8]) -> Result<IndexEntry, InvalidIndexEntryError> {
        // Shortest possible index entry length, for a single-character filename.
        if data.len() < 64 {
            return Err(InvalidIndexEntryError {
                error_kind: InvalidIndexEntryKind::TooShort,
            });
        }
        let ctime_s = helpers::u32_from_be_bytes_unchecked(data, 0);
        let ctime_ns = helpers::u32_from_be_bytes_unchecked(data, 4);
        let ctime = DateTime::<Utc>::from_timestamp(ctime_s.into(), ctime_ns);
        let Some(ctime) = ctime else {
            return Err(InvalidIndexEntryError {
                error_kind: InvalidIndexEntryKind::UnparseableTimestamp(ctime_s, ctime_ns),
            });
        };
        let mtime_s = helpers::u32_from_be_bytes_unchecked(data, 8);
        let mtime_ns = helpers::u32_from_be_bytes_unchecked(data, 12);
        let mtime = DateTime::<Utc>::from_timestamp(mtime_s.into(), mtime_ns);
        let Some(mtime) = mtime else {
            return Err(InvalidIndexEntryError {
                error_kind: InvalidIndexEntryKind::UnparseableTimestamp(mtime_s, mtime_ns),
            });
        };
        let dev = helpers::u32_from_be_bytes_unchecked(data, 16);
        let ino = helpers::u32_from_be_bytes_unchecked(data, 20);
        let mode = helpers::u16_from_be_bytes_unchecked(data, 26);
        let mode_type_val = mode >> 12;
        let mode_type = IndexEntryType::from_byte(mode_type_val as u8);
        let Some(mode_type) = mode_type else {
            return Err(InvalidIndexEntryError {
                error_kind: InvalidIndexEntryKind::UnexpectedMode(mode_type_val),
            });
        };
        let mode_perms = IndexEntryPermissions::from_u16(mode & 0x1FF);
        let Some(mode_perms) = mode_perms else {
            return Err(InvalidIndexEntryError {
                error_kind: InvalidIndexEntryKind::UnexpectedPermissions(mode & 0x1FF),
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
                error_kind: InvalidIndexEntryKind::TooShort,
            });
        }
        let name = if name_len < 0xFFF {
            if data[name_len + 62] != 0 {
                return Err(InvalidIndexEntryError {
                    error_kind: InvalidIndexEntryKind::NameNotNullTerminated,
                });
            }
            String::from_utf8_lossy(&data[62..(name_len + 62)])
        } else {
            let real_name_len = data[62..].iter().position(|x| *x == 0);
            let Some(real_name_len) = real_name_len else {
                return Err(InvalidIndexEntryError {
                    error_kind: InvalidIndexEntryKind::NameNotNullTerminated,
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

    /// Create a new index entry for the file at a given path.
    ///
    /// Loads the file's current metadata from the filesystem  and turns it into an index entry.
    ///
    /// # Errors
    ///
    /// This function returns an error if the file does not exist, or any other filesystem error occurs
    /// when loading the file's metadata.
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

    /// Write the contents of an index entry to an extensible byte sequence.
    ///
    /// The index entry is written in the Git on-disk index format.
    pub fn serialise<T: Extend<u8>>(&self, buf: &mut T) {
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
            buf.extend(obj_id.iter().copied());
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
        buf.extend([0]);
        // The formula for computing the entry length only works on v2 indexes (that pesky hardcoded 63 I just perpetrated)
        buf.extend(repeat_n(0, 8 - ((self.object_name.len() + 63) % 8)));
    }

    /// Get the parent directory of this entry.
    ///
    /// Returns an empty string if the entry is in the worktree root.
    pub fn object_directory_name(&self) -> &str {
        index_path_parent(&self.object_name)
    }

    /// Get the filename of this entry
    pub fn object_file_name(&self) -> &str {
        index_path_file(&self.object_name)
    }

    /// Get the combined [`IndexEntryType`] and [`IndexEntryPermissions`] fields of this entry, as a [`u32`] value as stored on disk.
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
    /// Order two index entries by name and then by merge stage
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

/// The in-memory representation of an entire index.
pub struct Index {
    /// The index version number.  At present, CVVC only supports v2 indexes.
    pub version: u32,

    entries: Vec<IndexEntry>,
}

impl Index {
    /// Create an empty index
    pub fn new() -> Self {
        Index {
            version: 2,
            entries: Vec::<IndexEntry>::new(),
        }
    }

    /// Get a reference to the index's entries.
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// Load an index from a sequence of bytes.
    ///
    /// # Errors
    ///
    /// If an error occurs on load, this function returns an [`InvalidIndexError`] with the
    /// [`InvalidIndexError::error_kind`] field indicating the reason for the error as follows:
    ///
    /// - if the data is too short to contain a valid header, it returns [`InvalidIndexKind::TooShort`]
    /// - if the data does not start with the correct identification byte sequence, it returns [`InvalidIndexKind::MissingMagic`]
    /// - if the index header does not indicate index version 2, it returns [`InvalidIndexKind::UnsupportedVersion`]
    /// - if any individual index entry cannot be parsed, it returns [`InvalidIndexKind::InvalidEntry`], which contains the
    /// underlying [`InvalidIndexEntryError`]
    ///
    /// If the data is too short to contain the number of entries specified in the header, the error kind may
    /// be either [`InvalidIndexKind::TooShort`] or [`InvalidIndexKind::InvalidEntry`] depending on whether or not the
    /// end of the data occurs at the end of a valid entry.
    pub fn from_bytes(data: &[u8]) -> Result<Index, InvalidIndexError> {
        if data.len() < 12 {
            return Err(InvalidIndexError {
                error_kind: InvalidIndexKind::TooShort,
            });
        }
        if data[..4] != *b"DIRC" {
            return Err(InvalidIndexError {
                error_kind: InvalidIndexKind::MissingMagic,
            });
        }
        let version = helpers::u32_from_be_bytes_unchecked(data, 4);
        if version != 2 {
            return Err(InvalidIndexError {
                error_kind: InvalidIndexKind::UnsupportedVersion(version),
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
                        error_kind: InvalidIndexKind::InvalidEntry(e),
                    })
                }
            };
            idx += entry.byte_length();
            if idx >= data.len() {
                return Err(InvalidIndexError {
                    error_kind: errors::InvalidIndexKind::TooShort,
                });
            }
            entries.push(entry);
        }
        Ok(Index { version, entries })
    }

    /// Write an [`Index`] to an extensible byte sequence.
    ///
    /// The index entry is written in the Git on-disk index format, version 2.
    ///
    /// You must not call this method if the index contents are not sorted.  If you
    /// do, then the serialised data may not be readable.
    pub fn serialise<T: Extend<u8>>(&self, buf: &mut T) {
        buf.extend([68, 73, 82, 67]); // equivalent of b"DIRC"
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

    /// Determine if this index contains an entry with the given path.
    ///
    /// The path must match an index entry's path exactly.
    pub fn contains_path(&self, path: &str) -> bool {
        self.entries.iter().any(|e| e.object_name == path)
    }

    /// Remove a path from this index.
    ///
    /// Returns true if an entry with this path was present in the index, false if the index was unchanged.
    pub fn remove(&mut self, path: &str) -> bool {
        let start_len = self.entries.len();
        self.entries.retain(|e| e.object_name != path);
        start_len > self.entries.len()
    }

    /// Retain all of the index entries whose object IDs are in a given list, and remove all of the
    /// entries whose IDs are not on that list.
    pub fn remove_not_present(&mut self, object_ids: &[String]) {
        self.entries.retain(|e| object_ids.contains(&e.object_id));
    }

    /// Add an entry to the index, consuming it.
    pub fn add(&mut self, entry: IndexEntry) {
        self.entries.push(entry);
        self.entries.sort();
    }

    /// Add a sequence of entries to the index, consuming them.
    pub fn add_range(&mut self, entries: &mut Vec<IndexEntry>) {
        self.entries.append(entries);
        self.entries.sort();
    }
}
