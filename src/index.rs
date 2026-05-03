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
#[derive(Debug, PartialEq)]
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
#[derive(Debug, PartialEq)]
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
#[derive(Debug, PartialEq)]
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
    /// do, then the serialised data may not be readable on other Git implementations.  
    /// The public API of this type, at the time of writing, sorts all data on insertion.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_entry_type_from_byte_file() {
        let test_input: u8 = 8;

        let result = IndexEntryType::from_byte(test_input);

        assert_eq!(Some(IndexEntryType::File), result);
    }

    #[test]
    fn index_entry_type_from_byte_symlink() {
        let test_input: u8 = 10;

        let result = IndexEntryType::from_byte(test_input);

        assert_eq!(Some(IndexEntryType::Symlink), result);
    }

    #[test]
    fn index_entry_type_from_byte_gitlink() {
        let test_input: u8 = 14;

        let result = IndexEntryType::from_byte(test_input);

        assert_eq!(Some(IndexEntryType::Gitlink), result);
    }

    #[test]
    fn index_entry_type_from_byte_invalid() {
        let test_input: u8 = 242;

        let result = IndexEntryType::from_byte(test_input);

        assert_eq!(None, result);
    }

    #[test]
    fn index_entry_type_to_byte_file() {
        let test_input = IndexEntryType::File;

        let result = test_input.to_byte();

        assert_eq!(8, result);
    }

    #[test]
    fn index_entry_type_to_byte_symlink() {
        let test_input = IndexEntryType::Symlink;

        let result = test_input.to_byte();

        assert_eq!(10, result);
    }

    #[test]
    fn index_entry_type_to_byte_gitlink() {
        let test_input = IndexEntryType::Gitlink;

        let result = test_input.to_byte();

        assert_eq!(14, result);
    }

    #[test]
    fn index_entry_type_fmt_file() {
        let test_input = IndexEntryType::File;

        let result = test_input.to_string();

        assert_eq!("regular file", result);
    }

    #[test]
    fn index_entry_type_fmt_symlink() {
        let test_input = IndexEntryType::Symlink;

        let result = test_input.to_string();

        assert_eq!("symbolic link", result);
    }

    #[test]
    fn index_entry_type_fmt_gitlink() {
        let test_input = IndexEntryType::Gitlink;

        let result = test_input.to_string();

        assert_eq!("git link", result);
    }

    #[test]
    fn index_entry_permissions_from_u16_non_executable() {
        let test_input: u16 = 0o644;

        let result = IndexEntryPermissions::from_u16(test_input);

        assert_eq!(Some(IndexEntryPermissions::NonExecutable), result);
    }

    #[test]
    fn index_entry_permissions_from_u16_executable() {
        let test_input: u16 = 0o755;

        let result = IndexEntryPermissions::from_u16(test_input);

        assert_eq!(Some(IndexEntryPermissions::Executable), result);
    }

    #[test]
    fn index_entry_permissions_from_u16_link() {
        let test_input: u16 = 0;

        let result = IndexEntryPermissions::from_u16(test_input);

        assert_eq!(Some(IndexEntryPermissions::Link), result);
    }

    #[test]
    fn index_entry_permissions_from_u16_invalid_value() {
        let test_input: u16 = 0o237;

        let result = IndexEntryPermissions::from_u16(test_input);

        assert_eq!(None, result);
    }

    #[test]
    fn index_entry_permissions_to_u16_non_executable() {
        let test_input = IndexEntryPermissions::NonExecutable;

        let result = test_input.to_u16();

        assert_eq!(0o644, result);
    }

    #[test]
    fn index_entry_permissions_to_u16_executable() {
        let test_input = IndexEntryPermissions::Executable;

        let result = test_input.to_u16();

        assert_eq!(0o755, result);
    }

    #[test]
    fn index_entry_permissions_to_u16_link() {
        let test_input = IndexEntryPermissions::Link;

        let result = test_input.to_u16();

        assert_eq!(0, result);
    }

    #[test]
    fn index_entry_permissions_fmt_non_executable() {
        let test_input = IndexEntryPermissions::NonExecutable;

        let result = test_input.to_string();

        assert_eq!("0644", result);
    }

    #[test]
    fn index_entry_permissions_fmt_executable() {
        let test_input = IndexEntryPermissions::Executable;

        let result = test_input.to_string();

        assert_eq!("0755", result);
    }

    #[test]
    fn index_entry_permissions_fmt_link() {
        let test_input = IndexEntryPermissions::Link;

        let result = test_input.to_string();

        assert_eq!("0000", result);
    }

    #[test]
    fn index_entry_byte_length_rounds_up() {
        let test_input = IndexEntry {
            ctime: Utc::now(),
            mtime: Utc::now(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 71000,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "00000000000000000000".to_string(),
            object_name: "an_ordinary_file_name".to_string(),
        };
        let expected_result = 88usize;

        let result = test_input.byte_length();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_byte_length_does_not_round_up_on_block_boundary() {
        let test_input = IndexEntry {
            ctime: Utc::now(),
            mtime: Utc::now(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 71000,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "00000000000000000000".to_string(),
            object_name: "a__file__with_name_ending_on_the__block__boundary".to_string(),
        };
        let expected_result = 112usize;

        let result = test_input.byte_length();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_from_bytes_returns_ok() {
        // This is a genuine index entry from this project's own index, tweaked so that the fields
        // that are zero on Windows are populated with the same values as the byte_length() tests above

        // This input is used for all the from_bytes() tests expected to return Ok(entry), and is tweaked
        // for tests expected to return Error(...)
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);

        assert!(result.is_ok());
    }

    #[test]
    fn index_entry_from_bytes_sets_ctime() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_ctime = DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
            .unwrap()
            .to_utc();

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_ctime, result.ctime);
    }

    #[test]
    fn index_entry_from_bytes_sets_mtime() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_mtime = DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
            .unwrap()
            .to_utc();

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_mtime, result.mtime);
    }

    #[test]
    fn index_entry_from_bytes_sets_dev() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_dev = 4472;

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_dev, result.dev);
    }

    #[test]
    fn index_entry_from_bytes_sets_ino() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_ino = 4468;

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_ino, result.ino);
    }

    #[test]
    fn index_entry_from_bytes_sets_mode_type() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(IndexEntryType::File, result.mode_type);
    }

    #[test]
    fn index_entry_from_bytes_sets_mode_perms() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(IndexEntryType::File, result.mode_type);
    }

    #[test]
    fn index_entry_from_bytes_sets_uid() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_uid = 80105;

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_uid, result.uid);
    }

    #[test]
    fn index_entry_from_bytes_sets_gid() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_gid = 2857;

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_gid, result.gid);
    }

    #[test]
    fn index_entry_from_bytes_sets_fsize() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_fsize = 3372;

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_fsize, result.fsize);
    }

    #[test]
    fn index_entry_from_bytes_sets_object_id() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_id = "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4";

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_id, result.object_id);
    }

    #[test]
    fn index_entry_from_bytes_sets_flag_assume_valid_false() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert!(!result.flag_assume_valid);
    }

    #[test]
    fn index_entry_from_bytes_sets_flag_assume_valid_true() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x80, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert!(result.flag_assume_valid);
    }

    #[test]
    fn index_entry_from_bytes_sets_flag_stage_0() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(0, result.flag_stage);
    }

    #[test]
    fn index_entry_from_bytes_sets_flag_stage_3() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(3, result.flag_stage);
    }

    #[test]
    fn index_entry_from_bytes_sets_name() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_name = "src/index/errors.rs";

        let result = IndexEntry::from_bytes(&test_input);

        let Ok(result) = result else {
            panic!();
        };
        assert_eq!(expected_name, result.object_name);
    }

    #[test]
    fn index_entry_from_bytes_error_if_insufficient_data_to_hold_all_fields() {
        let test_input = [0x69u8, 0xae, 0xe0, 0x4];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();

        assert_eq!(InvalidIndexEntryKind::TooShort, result.error_kind);
    }

    #[test]
    fn index_entry_from_bytes_error_if_invalid_ctime() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x3, 0x68, 0x9a, 0xca, 0xff, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();
        let InvalidIndexEntryKind::UnparseableTimestamp(_x, _y) = result.error_kind else {
            panic!();
        };
    }

    #[test]
    fn index_entry_from_bytes_error_if_invalid_mtime() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x64, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();
        let InvalidIndexEntryKind::UnparseableTimestamp(_x, _y) = result.error_kind else {
            panic!();
        };
    }

    #[test]
    fn index_entry_from_bytes_error_if_invalid_mode_type() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x11, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();
        let InvalidIndexEntryKind::UnexpectedMode(x) = result.error_kind else {
            panic!();
        };
        assert_eq!(1, x);
    }

    #[test]
    fn index_entry_from_bytes_error_if_invalid_mode_permissions() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xaf, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();
        let InvalidIndexEntryKind::UnexpectedPermissions(x) = result.error_kind else {
            panic!();
        };
        assert_eq!(0o657, x);
    }

    #[test]
    fn index_entry_from_bytes_error_if_data_is_too_short_for_declared_name_length() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73,
        ];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();
        let InvalidIndexEntryKind::TooShort = result.error_kind else {
            panic!();
        };
    }

    #[test]
    fn index_entry_from_bytes_error_if_name_is_too_short() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0, 0x69, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4,
            0x68,
        ];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();
        let InvalidIndexEntryKind::NameNotNullTerminated = result.error_kind else {
            panic!();
        };
    }

    #[test]
    fn index_entry_from_bytes_error_if_name_is_too_long() {
        let test_input = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0x30, 0x13, 0x73,
            0x72, 0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72,
            0x73, 0x2e, 0x72, 0x73, 0x73, 0, 0, 0, 0, 0, 0,
        ];

        let result = IndexEntry::from_bytes(&test_input);
        let result = result.unwrap_err();
        let InvalidIndexEntryKind::NameNotNullTerminated = result.error_kind else {
            panic!();
        };
    }

    #[test]
    fn index_entry_serialise() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let expected_result = [
            0x69u8, 0xae, 0xe0, 0x4, 0xc, 0xc8, 0x34, 0x60, 0x69, 0xb8, 0x42, 0x9e, 0x4, 0x68,
            0x2a, 0xf0, 0, 0, 0x11, 0x78, 0, 0, 0x11, 0x74, 0, 0, 0x81, 0xa4, 0, 0x1, 0x38, 0xe9,
            0, 0, 0x0b, 0x29, 0, 0, 0xd, 0x2c, 0xf6, 0xd9, 0xd2, 0x6f, 0x9d, 0x58, 0xb5, 0x8c, 0xd,
            0x7b, 0x1c, 0x69, 0xf6, 0xb2, 0x46, 0xcf, 0xca, 0x46, 0x40, 0xc4, 0, 0x13, 0x73, 0x72,
            0x63, 0x2f, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2f, 0x65, 0x72, 0x72, 0x6f, 0x72, 0x73,
            0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut results = Vec::<u8>::new();

        test_input.serialise(&mut results);

        assert_eq!(expected_result, *results);
    }

    #[test]
    fn index_entry_object_directory_name_if_non_root() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let expected_result = "src/index";

        let result = test_input.object_directory_name();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_object_directory_name_if_root() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "errors.rs".to_string(),
        };
        let expected_result = "";

        let result = test_input.object_directory_name();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_object_file_name_if_non_root() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let expected_result = "errors.rs";

        let result = test_input.object_file_name();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_object_file_name_if_root() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "errors.rs".to_string(),
        };
        let expected_result = "errors.rs";

        let result = test_input.object_file_name();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_mode_file_non_executable() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "errors.rs".to_string(),
        };
        let expected_result = 0o100644;

        let result = test_input.mode();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_mode_file_executable() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::Executable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "errors.rs".to_string(),
        };
        let expected_result = 0o100755;

        let result = test_input.mode();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_mode_file_symlink() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::Symlink,
            mode_perms: IndexEntryPermissions::Link,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "errors.rs".to_string(),
        };
        let expected_result = 0o120000;

        let result = test_input.mode();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_mode_file_gitlink() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::Gitlink,
            mode_perms: IndexEntryPermissions::Link,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "errors.rs".to_string(),
        };
        let expected_result = 0o160000;

        let result = test_input.mode();

        assert_eq!(expected_result, result);
    }

    #[test]
    fn index_entry_cmp_names_differ_at_root() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let other = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "vrc/index/errors.rs".to_string(),
        };

        let result = test_input.cmp(&other);

        assert_eq!(Ordering::Less, result);
    }

    #[test]
    fn index_entry_cmp_names_differ_at_file() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let other = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/arrows.rs".to_string(),
        };

        let result = test_input.cmp(&other);

        assert_eq!(Ordering::Greater, result);
    }

    #[test]
    fn index_entry_cmp_same_name_different_stage() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let other = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 2,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };

        let result = test_input.cmp(&other);

        assert_eq!(Ordering::Less, result);
    }

    #[test]
    fn index_entry_cmp_same_name_same_stage_other_fields_differ() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-10T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-10T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 60105,
            ino: 60013,
            mode_type: IndexEntryType::Symlink,
            mode_perms: IndexEntryPermissions::Link,
            uid: 92220,
            gid: 1450,
            fsize: 819,
            flag_assume_valid: true,
            flag_stage: 0,
            object_id: "f6d9d26f9c58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let other = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };

        let result = test_input.cmp(&other);

        assert_eq!(Ordering::Equal, result);
    }

    #[test]
    fn index_entry_eq_same_name_same_stage_other_fields_differ() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-10T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-10T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 60105,
            ino: 60013,
            mode_type: IndexEntryType::Symlink,
            mode_perms: IndexEntryPermissions::Link,
            uid: 92220,
            gid: 1450,
            fsize: 819,
            flag_assume_valid: true,
            flag_stage: 0,
            object_id: "f6d9d26f9c58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let other = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };

        let result = test_input == other;

        assert!(result);
    }

    #[test]
    fn index_entry_eq_names_differ() {
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/errors.rs".to_string(),
        };
        let other = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:18.073935600Z")
                .unwrap()
                .to_utc(),
            dev: 4472,
            ino: 4468,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 80105,
            gid: 2857,
            fsize: 3372,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "f6d9d26f9d58b58c0d7b1c69f6b246cfca4640c4".to_string(),
            object_name: "src/index/arrows.rs".to_string(),
        };

        let result = test_input == other;

        assert!(!result);
    }

    #[test]
    fn index_new_version() {
        let expected_result = 2;

        let test_result = Index::new();

        assert_eq!(expected_result, test_result.version);
    }

    #[test]
    fn index_new_entries_empty() {
        let expected_result = 0;

        let test_result = Index::new().entries.len();

        assert_eq!(expected_result, test_result);
    }

    #[test]
    fn index_from_bytes_loads_empty_index() {
        let test_input = [0x44u8, 0x49, 0x52, 0x43, 0, 0, 0, 2, 0, 0, 0, 0];

        let test_result = Index::from_bytes(&test_input).unwrap();

        assert_eq!(2, test_result.version);
        assert_eq!(0, test_result.entries.len());
    }

    #[test]
    fn index_from_bytes_short_data() {
        let test_input = [0x44u8, 0x49, 0x52, 0x43, 0, 0, 0, 2, 0, 0, 0];

        let test_result = Index::from_bytes(&test_input).unwrap_err();

        assert_eq!(InvalidIndexKind::TooShort, test_result.error_kind);
    }

    #[test]
    fn index_from_bytes_wrong_magic() {
        let test_input = [0x44u8, 0x49, 0x43, 0x52, 0, 0, 0, 2, 0, 0, 0, 0];

        let test_result = Index::from_bytes(&test_input).unwrap_err();

        assert_eq!(InvalidIndexKind::MissingMagic, test_result.error_kind);
    }

    #[test]
    fn index_from_bytes_wrong_version() {
        let test_input = [0x44u8, 0x49, 0x52, 0x43, 0, 0, 0, 5, 0, 0, 0, 0];

        let test_result = Index::from_bytes(&test_input).unwrap_err();

        assert_eq!(
            InvalidIndexKind::UnsupportedVersion(5),
            test_result.error_kind
        );
    }

    #[test]
    fn index_from_bytes_success() {
        let test_input = [
            0x44u8, 0x49, 0x52, 0x43, 0, 0, 0, 2, 0, 0, 0, 2, 0x69, 0xae, 0xe0, 4, 0xc, 0xc8, 0x34,
            0x60, 0x69, 0xb8, 0x42, 0x9d, 0x32, 0xfa, 0x47, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xd, 0xb3, 0x27, 0x32, 0x6e, 0xa7, 0x73, 0,
            0xf6, 0xcf, 0xd5, 0xa2, 0xf3, 0x87, 0x49, 0xff, 0x41, 0x65, 0xf1, 0xac, 0x65, 0xc6, 0,
            0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x6f, 0x62, 0x6a, 0x65, 0x63,
            0x74, 0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0, 0x69, 0xae, 0xe0, 4, 0xc, 0xc8,
            0x34, 0x60, 0x69, 0xb8, 0x41, 0xaa, 0x2c, 0x94, 0xa0, 0xd0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0x9d, 0xbe, 0x65, 0xc5, 0x99, 0xde,
            0x1f, 0xda, 0xbc, 0x46, 0x92, 0x2b, 0xa, 0x86, 0x9a, 0x25, 0x5c, 0xa8, 0x2f, 0xdd,
            0x80, 0, 0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x72, 0x65, 0x66, 0x5f,
            0x6c, 0x6f, 0x67, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let expected_entries = vec![
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 3507,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                object_name: "src/cli/objects.rs".to_string(),
            },
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/cli/ref_log.rs".to_string(),
            },
        ];

        let test_output = Index::from_bytes(&test_input).unwrap();

        assert_eq!(2, test_output.version);
        assert_eq!(2, test_output.entries.len());
        assert_eq!(expected_entries, test_output.entries);
    }

    #[test]
    fn index_from_bytes_invalid_entry() {
        let test_input = [
            0x44u8, 0x49, 0x52, 0x43, 0, 0, 0, 2, 0, 0, 0, 2, 0x69, 0xae, 0xe0, 4, 0xaf, 0xc8,
            0x34, 0x60, 0x69, 0xb8, 0x8f, 0x9d, 0x32, 0xfa, 0x47, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xd, 0xb3, 0x27, 0x32, 0x6e, 0xa7, 0x73,
            0, 0xf6, 0xcf, 0xd5, 0xa2, 0xf3, 0x87, 0x49, 0xff, 0x41, 0x65, 0xf1, 0xac, 0x65, 0xc6,
            0, 0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x6f, 0x62, 0x6a, 0x65, 0x63,
            0x74, 0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0, 0x69, 0xae, 0xe0, 4, 0xc, 0xc8,
            0x34, 0x60, 0x69, 0xb8, 0x41, 0xaa, 0x2c, 0x94, 0xa0, 0xd0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0x9d, 0xbe, 0x65, 0xc5, 0x99, 0xde,
            0x1f, 0xda, 0xbc, 0x46, 0x92, 0x2b, 0xa, 0x86, 0x9a, 0x25, 0x5c, 0xa8, 0x2f, 0xdd,
            0x80, 0, 0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x72, 0x65, 0x66, 0x5f,
            0x6c, 0x6f, 0x67, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];

        let test_result = Index::from_bytes(&test_input).unwrap_err();

        assert!(matches!(
            test_result.error_kind,
            InvalidIndexKind::InvalidEntry(_)
        ));
    }

    #[test]
    fn index_from_bytes_valid_but_truncated() {
        let test_input = [
            0x44u8, 0x49, 0x52, 0x43, 0, 0, 0, 2, 0, 0, 0, 2, 0x69, 0xae, 0xe0, 4, 0xc, 0xc8, 0x34,
            0x60, 0x69, 0xb8, 0x42, 0x9d, 0x32, 0xfa, 0x47, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xd, 0xb3, 0x27, 0x32, 0x6e, 0xa7, 0x73, 0,
            0xf6, 0xcf, 0xd5, 0xa2, 0xf3, 0x87, 0x49, 0xff, 0x41, 0x65, 0xf1, 0xac, 0x65, 0xc6, 0,
            0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x6f, 0x62, 0x6a, 0x65, 0x63,
            0x74, 0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0, 0x69, 0xae, 0xe0, 4, 0xc, 0xc8,
            0x34, 0x60, 0x69, 0xb8, 0x41, 0xaa, 0x2c, 0x94, 0xa0, 0xd0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0x9d, 0xbe, 0x65, 0xc5, 0x99, 0xde,
            0x1f, 0xda, 0xbc, 0x46, 0x92, 0x2b, 0xa, 0x86, 0x9a, 0x25, 0x5c, 0xa8, 0x2f, 0xdd,
            0x80, 0, 0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x72, 0x65, 0x66, 0x5f,
            0x6c, 0x6f, 0x67, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0,
        ];

        let test_result = Index::from_bytes(&test_input).unwrap_err();

        assert!(matches!(test_result.error_kind, InvalidIndexKind::TooShort));
    }

    #[test]
    fn index_serialise() {
        let test_input = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let expected_result = vec![
            0x44u8, 0x49, 0x52, 0x43, 0, 0, 0, 2, 0, 0, 0, 2, 0x69, 0xae, 0xe0, 4, 0xc, 0xc8, 0x34,
            0x60, 0x69, 0xb8, 0x42, 0x9d, 0x32, 0xfa, 0x47, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xd, 0xb3, 0x27, 0x32, 0x6e, 0xa7, 0x73, 0,
            0xf6, 0xcf, 0xd5, 0xa2, 0xf3, 0x87, 0x49, 0xff, 0x41, 0x65, 0xf1, 0xac, 0x65, 0xc6, 0,
            0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x6f, 0x62, 0x6a, 0x65, 0x63,
            0x74, 0x73, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0, 0x69, 0xae, 0xe0, 4, 0xc, 0xc8,
            0x34, 0x60, 0x69, 0xb8, 0x41, 0xaa, 0x2c, 0x94, 0xa0, 0xd0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0x81, 0xa4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0x9d, 0xbe, 0x65, 0xc5, 0x99, 0xde,
            0x1f, 0xda, 0xbc, 0x46, 0x92, 0x2b, 0xa, 0x86, 0x9a, 0x25, 0x5c, 0xa8, 0x2f, 0xdd,
            0x80, 0, 0x12, 0x73, 0x72, 0x63, 0x2f, 0x63, 0x6c, 0x69, 0x2f, 0x72, 0x65, 0x66, 0x5f,
            0x6c, 0x6f, 0x67, 0x2e, 0x72, 0x73, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let mut test_output: Vec<u8> = vec![];

        test_input.serialise(&mut test_output);

        assert_eq!(expected_result, test_output);
    }

    #[test]
    fn index_contains_path_true() {
        let test_object = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let test_input = "src/cli/ref_log.rs";

        let result = test_object.contains_path(test_input);

        assert!(result);
    }

    #[test]
    fn index_contains_path_false() {
        let test_object = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let test_input = "src/cli/init.rs";

        let result = test_object.contains_path(test_input);

        assert!(!result);
    }

    #[test]
    fn index_contains_path_prefix_false() {
        let test_object = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let test_input = "src/cli/ob";

        let result = test_object.contains_path(test_input);

        assert!(!result);
    }

    #[test]
    fn index_remove_entry_present() {
        let mut test_object = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let test_input = "src/cli/objects.rs";
        let expected_outcome = Index {
            version: 2,
            entries: vec![IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/cli/ref_log.rs".to_string(),
            }],
        };

        let result = test_object.remove(test_input);

        assert!(result);
        assert_eq!(expected_outcome, test_object);
    }

    #[test]
    fn index_remove_entry_not_present() {
        let mut test_object = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let test_input = "src/not_an/entry.html";
        let expected_outcome = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };

        let test_result = test_object.remove(test_input);

        assert!(!test_result);
        assert_eq!(expected_outcome, test_object);
    }

    #[test]
    fn index_add() {
        let mut test_object = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let test_input = IndexEntry {
            ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                .unwrap()
                .to_utc(),
            mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                .unwrap()
                .to_utc(),
            dev: 0,
            ino: 0,
            mode_type: IndexEntryType::File,
            mode_perms: IndexEntryPermissions::NonExecutable,
            uid: 0,
            gid: 0,
            fsize: 669,
            flag_assume_valid: false,
            flag_stage: 0,
            object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
            object_name: "src/cli/private.rs".to_string(),
        };
        let expected_outcome = vec![
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 3507,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                object_name: "src/cli/objects.rs".to_string(),
            },
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/cli/private.rs".to_string(),
            },
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/cli/ref_log.rs".to_string(),
            },
        ];

        test_object.add(test_input);

        assert_eq!(expected_outcome, test_object.entries);
    }

    #[test]
    fn index_add_range() {
        let mut test_object = Index {
            version: 2,
            entries: vec![
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 3507,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                    object_name: "src/cli/objects.rs".to_string(),
                },
                IndexEntry {
                    ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                        .unwrap()
                        .to_utc(),
                    mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                        .unwrap()
                        .to_utc(),
                    dev: 0,
                    ino: 0,
                    mode_type: IndexEntryType::File,
                    mode_perms: IndexEntryPermissions::NonExecutable,
                    uid: 0,
                    gid: 0,
                    fsize: 669,
                    flag_assume_valid: false,
                    flag_stage: 0,
                    object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                    object_name: "src/cli/ref_log.rs".to_string(),
                },
            ],
        };
        let mut test_input = vec![
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/cli/private.rs".to_string(),
            },
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/annex.rs".to_string(),
            },
        ];
        let expected_outcome = vec![
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/annex.rs".to_string(),
            },
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:49:17.855263200Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 3507,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "27326ea77300f6cfd5a2f38749ff4165f1ac65c6".to_string(),
                object_name: "src/cli/objects.rs".to_string(),
            },
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/cli/private.rs".to_string(),
            },
            IndexEntry {
                ctime: DateTime::parse_from_rfc3339("2026-03-09T14:58:12.214447200Z")
                    .unwrap()
                    .to_utc(),
                mtime: DateTime::parse_from_rfc3339("2026-03-16T17:45:14.747938Z")
                    .unwrap()
                    .to_utc(),
                dev: 0,
                ino: 0,
                mode_type: IndexEntryType::File,
                mode_perms: IndexEntryPermissions::NonExecutable,
                uid: 0,
                gid: 0,
                fsize: 669,
                flag_assume_valid: false,
                flag_stage: 0,
                object_id: "be65c599de1fdabc46922b0a869a255ca82fdd80".to_string(),
                object_name: "src/cli/ref_log.rs".to_string(),
            },
        ];

        test_object.add_range(&mut test_input);

        assert_eq!(expected_outcome, test_object.entries);
    }
}
