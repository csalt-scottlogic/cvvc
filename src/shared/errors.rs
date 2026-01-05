use std::{
    error::Error,
    fmt::{self, Display, Formatter},
};

#[derive(Debug)]
pub struct FindObjectError {
    candidates: Option<Vec<String>>,
}

impl FindObjectError {
    pub fn none() -> FindObjectError {
        FindObjectError { candidates: None }
    }

    pub fn some(candidates: &[String]) -> FindObjectError {
        FindObjectError {
            candidates: Some(
                candidates
                    .iter()
                    .map(|x| x.to_string())
                    .collect::<Vec<String>>(),
            ),
        }
    }
}

impl Display for FindObjectError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self.candidates {
            Some(_) => write!(f, "multiple objects found"),
            None => write!(f, "no objects found"),
        }
    }
}

impl Error for FindObjectError {}

#[derive(Debug)]
pub struct InvalidObjectError {}

impl Display for InvalidObjectError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "invalid object")
    }
}

impl Error for InvalidObjectError {}

#[derive(Debug)]
pub enum InvalidIndexEntryKind {
    TooShort,
    UnexpectedMode(u16),
    UnexpectedPermissions(u16),
    NameNotNullTerminated,
    UnparseableTimestamp(u32, u32),
}

#[derive(Debug)]
pub struct InvalidIndexEntryError {
    pub error_kind: InvalidIndexEntryKind
}

impl Display for InvalidIndexEntryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let msg = match self.error_kind {
            InvalidIndexEntryKind::TooShort => String::from("not enough data"),
            InvalidIndexEntryKind::UnexpectedMode(m) => format!("unexpected mode {m:#04b}"),
            InvalidIndexEntryKind::UnexpectedPermissions(p) => format!("unexpected permissions {p:#04o}"),
            InvalidIndexEntryKind::NameNotNullTerminated => String::from("name not null-terminated"),
            InvalidIndexEntryKind::UnparseableTimestamp(s, ns) => format!("unparseable timestamp {s}.{ns}"),
        };
        write!(f, "invalid index entry: {msg}")
    }
}

impl Error for InvalidIndexEntryError {}

#[derive(Debug)]
pub enum InvalidIndexKind {
    TooShort,
    MissingMagic,
    UnsupportedVersion(u32),
    InvalidEntry(InvalidIndexEntryError)
}

#[derive(Debug)]
pub struct InvalidIndexError {
    pub error_kind: InvalidIndexKind
}

impl Display for InvalidIndexError {
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
