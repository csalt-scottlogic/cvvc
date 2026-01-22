use std::{
    collections::VecDeque,
    fs::{Metadata, ReadDir},
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};

use crate::shared::{IndexEntryPermissions, IndexEntryType};

pub mod errors;

/// Take an OS-specific path and convert it into the Git index format (path separator is ASCII '/', no leading or trailing separator)
pub fn path_translate(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<String>>()
        .join("/")
}

/// Take a Git index path and convert it into an OS-specific path.
pub fn path_translate_rev(path: &str) -> PathBuf {
    PathBuf::from_iter(path.split("/"))
}

pub fn walk_fs_pruned<'a>(
    path: &Path,
    pruner: &'a dyn Fn(&Path) -> bool,
) -> io::Result<FsWalker<'a>> {
    let dirs = VecDeque::<PathBuf>::new();
    let dir_reader = path.read_dir()?;

    Ok(FsWalker {
        dirs,
        dir_reader,
        pruner,
    })
}

pub struct FsWalker<'a> {
    dirs: VecDeque<PathBuf>,
    dir_reader: ReadDir,
    pruner: &'a dyn Fn(&Path) -> bool,
}

impl Iterator for FsWalker<'_> {
    type Item = io::Result<PathBuf>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.dir_reader.next().transpose().unwrap() {
                Some(entry) => {
                    let typ = entry.file_type();
                    let Ok(typ) = typ else {
                        return Some(Err(typ.unwrap_err()));
                    };
                    if typ.is_dir() {
                        self.dirs.push_front(entry.path());
                    }
                    if typ.is_file() {
                        return Some(Ok(entry.path()));
                    }
                }
                None => {
                    let mut next_dir: PathBuf;
                    loop {
                        next_dir = self.dirs.pop_back()?;
                        if !(self.pruner)(&next_dir) {
                            break;
                        }
                    }
                    let next_reader = next_dir.read_dir();
                    if let Err(next_reader) = next_reader {
                        return Some(Err(next_reader));
                    }
                    self.dir_reader = next_reader.unwrap();
                }
            }
        }
    }
}

pub struct FileMetadata {
    pub ctime: DateTime<Utc>,
    pub mtime: DateTime<Utc>,
    pub dev: u32,
    pub ino: u32,
    pub mode_type: IndexEntryType,
    pub mode_perms: IndexEntryPermissions,
    pub uid: u32,
    pub gid: u32,
    pub fsize: u32,
}

impl FileMetadata {
    pub fn from_path(path: &Path) -> Result<Self, anyhow::Error> {
        let metadata = path.metadata()?;
        get_platform_metadata(metadata, path)
    }
}

#[cfg(windows)]
fn get_platform_metadata(metadata: Metadata, path: &Path) -> Result<FileMetadata, anyhow::Error> {
    use is_executable::IsExecutable;
    use std::os::windows::fs::MetadataExt;

    let ctime = time_convert(metadata.creation_time());
    let mtime = time_convert(metadata.last_write_time());
    let mode_type = if metadata.is_symlink() {
        IndexEntryType::Symlink
    } else {
        IndexEntryType::File
    };
    let mode_perms = if metadata.is_symlink() {
        IndexEntryPermissions::Link
    } else if path.is_executable() {
        IndexEntryPermissions::Executable
    } else {
        IndexEntryPermissions::NonExecutable
    };
    let fsize: u32 = metadata.file_size().try_into().unwrap_or(u32::MAX);
    Ok(FileMetadata {
        ctime,
        mtime,
        dev: 0,
        ino: 0,
        mode_type,
        mode_perms,
        uid: 0,
        gid: 0,
        fsize,
    })
}

#[cfg(windows)]
fn time_convert(ft: u64) -> DateTime<Utc> {
    match ft {
        0 => DateTime::<Utc>::UNIX_EPOCH,
        _ => DateTime::<Utc>::from(nt_time::FileTime::new(ft)),
    }
}

#[cfg(unix)]
fn get_platform_metadata(metadata: Metadata, _path: &Path) -> Result<FileMetadata, anyhow::Error> {
    use std::os::unix::fs::MetadataExt;

    let ctime = time_convert(metadata.ctime(), metadata.ctime_nsec());
    let mtime = time_convert(metadata.mtime(), metadata.mtime_nsec());
    let mode_type = if metadata.is_symlink() {
        IndexEntryType::Symlink
    } else {
        IndexEntryType::File
    };
    let dev: u32 = metadata.dev().try_into().unwrap_or(u32::MAX);
    let ino: u32 = metadata.ino().try_into().unwrap_or(u32::MAX);
    let mode_perms = if metadata.is_symlink() {
        IndexEntryPermissions::Link
    } else if metadata.mode() & 0o100 != 0 {
        IndexEntryPermissions::Executable
    } else {
        IndexEntryPermissions::NonExecutable
    };
    let fsize: u32 = metadata.size().try_into().unwrap_or(u32::MAX);
    Ok(FileMetadata {
        ctime,
        mtime,
        dev,
        ino,
        mode_type,
        mode_perms,
        uid: metadata.uid(),
        gid: metadata.gid(),
        fsize,
    })
}

#[cfg(unix)]
fn time_convert(s: i64, ns: i64) -> DateTime<Utc> {
    let ns: u32 = ns.try_into().unwrap_or(0);
    match DateTime::<Utc>::from_timestamp(s, ns) {
        Some(t) => t,
        None => DateTime::<Utc>::UNIX_EPOCH,
    }
}
