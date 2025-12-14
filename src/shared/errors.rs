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
