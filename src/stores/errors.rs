use std::{error::Error, fmt::Display};

#[derive(Debug)]
pub struct InvalidRefNameError {
    name: String,
}

impl InvalidRefNameError {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

impl Display for InvalidRefNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid ref name '{}'", self.name)
    }
}

impl Error for InvalidRefNameError {}
