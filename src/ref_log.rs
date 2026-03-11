use anyhow::Context;
use std::{
    fmt::Display,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::repo::is_partial_object_id;

#[derive(Debug)]
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
            let committer = match msg_sep {
                None => &value[42..],
                Some(i) => &value[42..i],
            }
            .to_string();
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
        let file_path = self.ref_log_file_path(branch_name);
        self.write_to_file(entry, &file_path)?;
        if branch_name.is_some() {
            self.write_to_file(entry, self.ref_log_file_path(None))?;
        }
        Ok(())
    }

    fn write_to_file<P: AsRef<Path>>(
        &self,
        entry: &RefLogEntry,
        path: P,
    ) -> Result<(), anyhow::Error> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .context("failed to open reflog file")?;
        writeln!(file, "{}", entry).context("failed to write to reflog file")?;
        Ok(())
    }

    pub fn dump(&self, branch_name: Option<&str>) -> Result<(), anyhow::Error> {
        let file_path = self.ref_log_file_path(branch_name);
        let mut file = OpenOptions::new()
            .read(true)
            .open(file_path)
            .context("Failed to open ref-log file")?;
        io::copy(&mut file, &mut io::stdout())?;
        Ok(())
    }

    pub fn check_exists(&self, branch_name: &str) -> bool {
        let file_path = if branch_name == "HEAD" {
            self.ref_log_file_path(None)
        } else {
            self.ref_log_file_path(Some(branch_name))
        };
        file_path.exists()
    }

    pub fn list_ref_logs(&self) -> Result<Vec<String>, anyhow::Error> {
        let mut output = Vec::<String>::new();
        if self.base_path.join("HEAD").exists() {
            output.push("HEAD".to_string());
        }
        let branch_ref_log_dir = self.base_path.join("refs").join("heads");
        for ref_log_entry in fs::read_dir(branch_ref_log_dir)? {
            let ref_log_entry = ref_log_entry?;
            let file_type = ref_log_entry.file_type()?;
            if file_type.is_file() {
                output.push(format!(
                    "refs/heads/{}",
                    ref_log_entry.file_name().to_string_lossy()
                ));
            }
        }
        Ok(output)
    }

    fn ref_log_file_path(&self, branch_name: Option<&str>) -> PathBuf {
        match branch_name {
            None => self.base_path.join("HEAD"),
            Some(n) => self.base_path.join("refs").join("heads").join(n),
        }
    }
}
