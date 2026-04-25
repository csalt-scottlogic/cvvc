use anyhow::Context;
use std::{
    fmt::Display,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    str::FromStr,
};

use crate::{helpers, repo::is_partial_object_id};

/// An entry in a ref log.
#[derive(Debug)]
pub struct RefLogEntry {
    /// The previous commit ID.
    ///
    /// This can be `None` for the first entry in a specific reflog,
    /// for example on repository clone or on branch creation.
    pub old_object_id: Option<String>,

    /// The new commit ID.
    pub new_object_id: String,

    /// The name and email of the committer, and the timestamp of the event.
    ///
    /// This is stored in the same format as the `committer` and `author` fields
    /// of a commit: "real name <email@example.com> nnnn +xx" where `nnnn` is the
    /// timestamp in seconds-since-datum format, and `+xx` is the timezone offset
    /// from UTC.
    pub committer: String,

    /// The ref log message.  By convention this indicates the action and a message
    /// such as "commit: [first line of message]" or "checkout: switched from branch-a to
    /// branch-b", but the message can potentially be an arbitrary string if a ref log entry
    /// was inserted via Git plumbing commands.
    pub message: String,
}

impl RefLogEntry {
    /// Create a new ref log entry.
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

    /// Convert a string to a [`RefLogEntry`].
    ///
    /// The string should consist of two potentially valid object IDs, each followed by a space;
    /// then a user name, email and timestamp, followed optionally by a tab and an arbitrary message.
    ///
    /// This function will return an error if this does not apply.  It does not verify the format of the
    /// user name, email and timestamp.  For the initial "old ID" field, a string of 40 zeroes is accepted
    /// if the ref log entry has no old ID value.
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
    /// Format a [`RefLogEntry`] value as text.
    ///
    /// This function is the inverse of [`RefLogEntry::from_str`], and converts a [`RefLogEntry`] object to its
    /// on-disk format.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let old_object_id = match &self.old_object_id {
            Some(id) => id.as_str(),
            None => "0000000000000000000000000000000000000000",
        };
        write!(
            f,
            "{} {} {}\t{}",
            old_object_id, self.new_object_id, self.committer, self.message
        )
    }
}

/// A structure used to access a set of ref logs.
pub struct RefLog {
    base_path: PathBuf,
}

impl RefLog {
    /// Create a new in-memory [`RefLog`] object representing ref logs stored under the given path.
    ///
    /// The path does not have to exist.  If it does not, [`RefLog::create`] will try to create it.
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            base_path: path.as_ref().to_path_buf(),
        }
    }

    /// Create the directory structure for storing a set of ref logs, if it does not exist.
    ///
    /// # Errors
    ///
    /// This method returns an error if it encounters any error writing to the filesystem.
    pub fn create(&self) -> Result<(), anyhow::Error> {
        if !self.base_path.exists() {
            fs::create_dir_all(&self.base_path).context("Failed to create ref log directory")
        } else {
            Ok(())
        }
    }

    /// Write a new [`RefLogEntry`] to the appropriate ref log.
    ///
    /// This method writes to the ref log for `HEAD`, and if the `branch_name` parameter is not
    /// `None`, also writes the same entry to the ref log for the given branch.  If a ref log
    /// for that branch does not exist, it is created.
    ///
    /// # Errors
    ///
    /// This method returns an error if it encounters any error writing to the filesystem.
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

    /// Copy the content of a ref log file to `stdout`.
    ///
    /// This method will copy the ref log file for `branch_name`, or the ref log for
    /// `HEAD` if the `branch_name` parameter is `None`.
    ///
    /// The branch given does not need to exist, as long as its ref log file exists.
    ///
    /// # Errors
    ///
    /// This method will return an error if it encounters any errors reading from
    /// the filesystem, or if the branch given does not have a ref log file.
    pub fn dump(&self, branch_name: Option<&str>) -> Result<(), anyhow::Error> {
        let file_path = self.ref_log_file_path(branch_name);
        let mut file = OpenOptions::new()
            .read(true)
            .open(file_path)
            .context("Failed to open ref-log file")?;
        io::copy(&mut file, &mut io::stdout())?;
        Ok(())
    }

    /// Return `true` if a ref log file exists on disk for the given branch
    /// (or for "`HEAD`"), and `false` if not.
    ///
    /// This method is infallible, and returns `false` if it encounters any filesystem errors.
    pub fn check_exists(&self, branch_name: &str) -> bool {
        let file_path = if branch_name == "HEAD" {
            self.ref_log_file_path(None)
        } else {
            self.ref_log_file_path(Some(branch_name))
        };
        file_path.exists()
    }

    /// Return a list of extant ref logs on disk.
    ///
    /// This method returns an error if it encounters any errors reading from the filesystem.
    pub fn list_ref_logs(&self) -> Result<Vec<String>, anyhow::Error> {
        let mut output = Vec::<String>::new();
        for ref_log_entry in helpers::fs::walk_fs(&self.base_path)? {
            let ref_log_entry = ref_log_entry?;
            if ref_log_entry.is_file() {
                output.push(helpers::fs::path_translate(ref_log_entry.strip_prefix(&self.base_path)?))
            }
        }
        output.sort();
        Ok(output)
    }

    fn ref_log_file_path(&self, branch_name: Option<&str>) -> PathBuf {
        match branch_name {
            None => self.base_path.join("HEAD"),
            Some(n) => self.base_path.join("refs").join("heads").join(n),
        }
    }
}
