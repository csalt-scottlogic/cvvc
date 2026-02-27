use anyhow::Context;
use std::{
    fmt::Display,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::shared::repo::is_partial_object_id;

pub struct RefLogEntry {
    pub old_object_id: Option<String>,
    pub new_object_id: String,
    pub committer: String,
    pub message: String,
}

impl RefLogEntry {
    pub fn new(
        old_object_id: Option<&str>,
        new_object_id: &str,
        committer: &str,
        message: &str,
    ) -> Self {
        Self {
            old_object_id: old_object_id.map(str::to_string),
            new_object_id: new_object_id.to_string(),
            committer: committer.to_string(),
            message: message.to_string(),
        }
    }
}

impl FromStr for RefLogEntry {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() < 43
            || !is_partial_object_id(&value[..20])
            || !is_partial_object_id(&value[21..41])
            || value.chars().nth(20) != Some(' ')
            || value.chars().nth(41) != Some(' ')
        {
            Err("mangled ref log entry")
        } else {
            let old_object_id = if value[..20].chars().all(|b| b == '0') {
                None
            } else {
                Some(value[..20].to_string())
            };
            let msg_sep = value.find("\t");
            let committer = (match msg_sep {
                None => &value[42..],
                Some(i) => &value[42..i],
            }
            .to_string());
            let message = match msg_sep {
                None => String::new(),
                Some(i) => value[(i + 1)..].to_string(),
            };
            Ok(Self {
                old_object_id,
                new_object_id: value[21..41].to_string(),
                committer,
                message,
            })
        }
    }
}

impl Display for RefLogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let old_object_id = match &self.old_object_id {
            Some(id) => id.as_str(),
            None => "00000000000000000000",
        };
        write!(
            f,
            "{} {} {}\t{}",
            old_object_id, self.new_object_id, self.committer, self.message
        )
    }
}

pub struct RefLog {
    base_path: PathBuf,
}

impl RefLog {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            base_path: path.as_ref().to_path_buf(),
        }
    }

    pub fn create(&self) -> Result<(), anyhow::Error> {
        if !self.base_path.exists() {
            fs::create_dir_all(&self.base_path).context("Failed to create ref log directory")
        } else {
            Ok(())
        }
    }

    pub fn write(
        &self,
        entry: &RefLogEntry,
        branch_name: Option<&str>,
    ) -> Result<(), anyhow::Error> {
        let file_path = match branch_name {
            None => self.base_path.join("HEAD"),
            Some(n) => self.base_path.join("refs").join("heads").join(n),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(file_path)
            .context("failed to open reflog file")?;
        writeln!(file, "{}", entry).context("failed to write to reflog file")?;
        Ok(())
    }
}
