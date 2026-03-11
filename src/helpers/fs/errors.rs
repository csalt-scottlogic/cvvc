use std::{error::Error, fmt::Display};

#[derive(Debug)]
pub enum PathErrorKind {
    InvalidPath,
    PathOutsideRepo,
}

#[derive(Debug)]
pub struct PathError {
    pub path: String,
    kind: PathErrorKind,
}

impl PathError {
    pub fn new<T: ToString>(path: T, kind: PathErrorKind) -> Self {
        PathError {
            path: path.to_string(),
            kind,
        }
    }
}

impl Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            PathErrorKind::InvalidPath => write!(f, "invalid path '{}'", self.path),
            PathErrorKind::PathOutsideRepo => write!(f, "path '{}' is outside the repo", self.path),
        }
    }
}

impl Error for PathError {}
