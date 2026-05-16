//! Error structs for errors specific to index parsing.

use std::{
    error::Error,
    fmt::{self, Display, Formatter},
};

/// An index entry type flag had an invalid value.  The valid values are 0x08, 0x0a and 0x0e.
#[derive(Debug, PartialEq)]
pub struct InvalidIndexEntryType {
    /// The invalid flag value.
    value: u8,
}

impl InvalidIndexEntryType {
    /// Create a new [`InvalidIndexEntryType`] object.
    pub fn new(value: u8) -> Self {
        Self { value }
    }
}

impl Display for InvalidIndexEntryType {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "invalid index entry type {:x}", self.value)
    }
}

impl Error for InvalidIndexEntryType {}

/// An index entry permissions field has an invalid value.  The valid values are 0o644, 0o755, and 0o000.
#[derive(Debug, PartialEq)]
pub struct InvalidIndexEntryPermissions {
    /// The actual value of the field.
    value: u16,
}

impl InvalidIndexEntryPermissions {
    /// Create a new [`InvalidIndexEntryPermissions`] object.
    pub fn new(value: u16) -> Self {
        Self { value }
    }
}

impl Display for InvalidIndexEntryPermissions {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "invalid index entry permissions {:o}", self.value)
    }
}

impl Error for InvalidIndexEntryPermissions {}

/// The reasons that an index entry may be invalid.
#[derive(Debug, PartialEq)]
pub enum InvalidIndexEntryKind {
    /// The entry was too short to be properly parsed.
    TooShort,

    /// The file mode was not one of the valid file values for an index entry.
    UnexpectedMode(u16),

    /// The file permissions field was not one of the valid permissions value for an index entry.
    UnexpectedPermissions(u16),

    /// The parser ran out of data before finding a NUL value to terminate the entry name.
    NameNotNullTerminated,

    /// The index timestamp value could not be parsed to a valid timestamp.
    UnparseableTimestamp(u32, u32),
}

/// An error which occurs when the index parser cannot parse an individual index entry.
#[derive(Debug, PartialEq)]
pub struct InvalidIndexEntryError {
    /// The reason the index entry could not be parsed.
    pub error_kind: InvalidIndexEntryKind,
}

impl Display for InvalidIndexEntryError {
    /// Converts an [`InvalidIndexEntryError`] to a human-readable string.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let msg = match self.error_kind {
            InvalidIndexEntryKind::TooShort => String::from("not enough data"),
            InvalidIndexEntryKind::UnexpectedMode(m) => format!("unexpected mode {m:#04b}"),
            InvalidIndexEntryKind::UnexpectedPermissions(p) => {
                format!("unexpected permissions {p:#04o}")
            }
            InvalidIndexEntryKind::NameNotNullTerminated => {
                String::from("name not null-terminated")
            }
            InvalidIndexEntryKind::UnparseableTimestamp(s, ns) => {
                format!("unparseable timestamp {s}.{ns}")
            }
        };
        write!(f, "invalid index entry: {msg}")
    }
}

impl Error for InvalidIndexEntryError {}

/// The reasons that an entire index may be invalid or unparseable.
#[derive(Debug, PartialEq)]
pub enum InvalidIndexKind {
    /// The index was too short to be properly parsed.  This implies the parser ran out of data before reaching the
    /// first index entry.  If the parser has beg
    TooShort,

    /// The header indicating that this file is an index was missing.
    MissingMagic,

    /// The index's version number is not supported by CVVC.
    UnsupportedVersion(u32),

    /// One or more of the entries in the index could not be parsed.
    InvalidEntry(InvalidIndexEntryError),
}

/// An error occurred when parsing an index.  This may have occurred due to an issue with an
/// individual entry, or with the index as a whole.
#[derive(Debug)]
pub struct InvalidIndexError {
    /// The first condition detected that makes the index content unparseable.
    pub error_kind: InvalidIndexKind,
}

impl Display for InvalidIndexError {
    /// Converts an [`InvalidIndexError`] to a human-readable string.
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let msg = match &self.error_kind {
            InvalidIndexKind::TooShort => String::from("not enough data"),
            InvalidIndexKind::MissingMagic => String::from("missing magic number"),
            InvalidIndexKind::UnsupportedVersion(v) => format!("unsupported index version {v}"),
            InvalidIndexKind::InvalidEntry(e) => format!("{e}"),
        };
        write!(f, "invalid index: {msg}")
    }
}

impl Error for InvalidIndexError {}
