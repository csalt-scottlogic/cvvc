//! This module contains filesystem-related helper functions.
//!
//! This module (and CVVC in general) uses [`std::ffi::OsStr::to_string_lossy`] to convert filesystem
//! paths to [String]s for handling inside CVVC.  Because of this, you are likely to have issues using
//! CVVC-based apps on filesystems that are not Unicode-compatible.

use anyhow::{anyhow, Context};
use std::{
    collections::VecDeque,
    fs::{create_dir_all, Metadata, OpenOptions, ReadDir},
    io::{self, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};

use crate::index::{IndexEntryPermissions, IndexEntryType};

/// Filesystem-specific error structs.
pub mod errors;

/// Take an OS-specific path and convert it into the Git index format
///
/// Within Git and CVVC repositories, paths are generally stored in the strict Unix format with components
/// separated by the ASCII '/' character (charpoint 47), and without any leading or trailing separator.  This function
/// converts a [Path] to this format.
///
/// This function is also used when converting branch names to filesystem paths.
pub fn path_translate(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<String>>()
        .join("/")
}

/// Take a Git index path and convert it into an OS-specific path.
///
/// Within Git and CVVC repositories, paths are generally stored in the strict Unix format with components
/// separated by the ASCII '/' character (charpoint 47), and without any leading or trailing separator.
/// This function splits a string on the '/' character and creates an owned [`PathBuf`] with each element of the
/// split string being a component of the path.
///
/// In general, paths returned by this function will be relative; the caller will have to append them to the
/// worktree path to get a usable absolute path.
pub fn path_translate_rev(path: &str) -> PathBuf {
    PathBuf::from_iter(path.split("/"))
}

/// Returns a repository-format path without its final component, if there is one.
///
/// Within Git and CVVC repositories, paths are generally stored in the strict Unix format with components
/// separated by the ASCII '/' character (charpoint 47), and without any leading or trailing separator.  This
/// function is the equivalent of [`Path::parent`], in that it returns its input up to but not including the final
/// '/' character.
///
/// If the input consists of a single component, and therefore contains no '/' characters, this
/// function returns an empty string slice.
pub fn index_path_parent(path: &str) -> &str {
    if !path.contains('/') {
        ""
    } else {
        let end = path.rfind('/').unwrap();
        &path[..end]
    }
}

/// Returns the final component of a repository-format path, if there is one.
///
/// Within Git and CVVC repositories, paths are generally stored in the strict Unix format with components
/// separated by the ASCII '/' character (charpoint 47), and without any leading or trailing separator.  This
/// function is the equivalent of [`Path::file_name`], in that it returns its input after and not including the
/// final '/' character.
///
/// If the input consists of a single component, and therefore contains no '/' characters, this function
/// returns the entire input.
pub fn index_path_file(path: &str) -> &str {
    if !path.contains('/') {
        path
    } else {
        let end = path.rfind('/').unwrap() + 1;
        &path[end..]
    }
}

/// Opens a file and writes a single line of text to it, replacing any previous content.
///
/// This function opens a file at the given path in create, write and truncate mode; if the file
/// does not exist it will be created, and if it does exist then any prior content will be replaced.  It then
/// writes a string to the file, followed by a terminating newline.  Although the function name implies
/// that the string will consist of a single line of text, this is not enforced; the content is not truncated
/// and a multi-line string will be written in full.
///
/// This function does not check if the parent directory of the path exists.  If it does not exist, the function
/// will return an error.
///
/// This function will also return an error if any of the underlying filesystem functions returns an error for
/// any other reason; for example permissions errors or hardware errors.
pub fn write_single_line<T: AsRef<Path>>(path: T, content: &str) -> Result<(), anyhow::Error> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    writeln!(file, "{content}")?;
    file.flush()?;
    Ok(())
}

/// Creates any missing directories in a path.
///
/// This function wraps [`create_dir_all`] to create all of the components of a path as directories, if they
/// do not already exist.  If the path does already exist, it checks that the final component is a
/// directory and returns an error if it is not.
///
/// Like the underlying function, this function is not atomic.  Because of this, if it returns an error,
/// it is not guaranteed that the filesystem will not have been modified.  Any components which were
/// successfully created will remain in the filesystem.
///
/// This function can return an error for any of the reasons that [`std::fs::create_dir`] can return an error.
pub fn check_and_create_dir<P: AsRef<Path>>(path: P) -> Result<PathBuf, anyhow::Error> {
    let path_reffed = path.as_ref();
    if path_reffed.exists() {
        if path_reffed.is_dir() {
            Ok(path_reffed.to_path_buf())
        } else {
            Err(anyhow!("Path exists but is not a directory"))
        }
    } else {
        create_dir_all(&path).context("Could not create all components of directory path")?;
        Ok(path_reffed.to_path_buf())
    }
}

/// Iterate over a directory tree, using a prune function to determine whether or not to descend into subdirectories.
///
/// The [`FsWalker`] iterator returned by this function iterates over a directory tree, and  calls the `pruner` function
/// on every subdirectory path it encounters, including nested subdirectories.  If the pruner returns `true`, the
/// iterator does not descend into that subdirectory.
///
/// The iterator will read subdirectories lazily as required, rather than traversing the entire tree immediately.
/// Because of this, filesystem errors may surface when iterating over the result of this function, especially
/// if the filesystem has been modified during the lifetime of the iterator.  If a file or directory is created
/// after this function is called but before the iterator is fully consumed, then whether that file or directory
/// will be included in the iterator's output is not defined.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use cvvc::helpers::fs::walk_fs_pruned;
/// 
/// // Print all files and directories, excluding any temporary directories.
/// let paths = walk_fs_pruned(&std::env::current_dir().unwrap(), &|p: &Path| p.file_name().is_some_and(|x| x != "tmp")).unwrap();
/// for path in paths {
///     println!("{}", path.unwrap().display());
/// }
/// ```
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

/// Iterate over a directory tree, descending into all subdirectories.
///
/// This function returns an [`FsWalker`] iterator like [`walk_fs_pruned`], but returns every file
/// and subdirectory in the directory tree, without pruning.  It is equivalent to calling [`walk_fs_pruned`]
/// with a pruner function that returns `false` on any input.
///
/// The iterator will read subdirectories lazily as required, rather than traversing the entire tree immediately.
/// Because of this, filesystem errors may surface when iterating over the result of this function, especially
/// if the filesystem has been modified during the lifetime of the iterator.  If a file or directory is created
/// after this function is called but before the iterator is fully consumed, then whether that file or directory
/// will be included in the iterator's output is not defined.
pub fn walk_fs<'a, P: AsRef<Path>>(path: P) -> io::Result<FsWalker<'a>> {
    walk_fs_pruned(path.as_ref(), &|_| false)
}

/// An iterator which iterates over a directory tree.
///
/// This is the iterator returned by [`walk_fs`] and [`walk_fs_pruned`].  See their documentation for further
/// information.
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

/// A structure representing platform-independent file metadata.
///
/// The metadata specified here is that used in index entries.  The values of the `mode_type` and
/// `mode_perms` fields are limited to those supported by Git and CVVC; the `mode_perms` field
/// distinguishes between executable and non-executable files rather than indicating permissions information.  
/// For files larger than 2.1Gb in size, the `fsize` field is set to [`u32::MAX`].
///
/// On Windows, the `dev`, `ino`, `uid` and `gid` fields are not populated, and the
/// `ctime` field has a different meaning to other platforms.  This is unlikely to cause users issues
/// unless they use the same repository from both Windows and Unix systems, for example on a network
/// fileshare.
pub struct FileMetadata {
    /// On Windows, the file creation time.  On other systems, the file change time, if supported.
    pub ctime: DateTime<Utc>,

    /// File modification time.
    pub mtime: DateTime<Utc>,

    /// Device number (zero on systems that do not support it)
    pub dev: u32,

    /// File inode number (zero on systems that do not support it)
    pub ino: u32,

    /// File type
    pub mode_type: IndexEntryType,

    /// File "permissions"
    pub mode_perms: IndexEntryPermissions,

    /// User ID (zero on systems that do not support it)
    pub uid: u32,

    /// Group ID (zero on systems that do not support it)
    pub gid: u32,

    /// File size.  This is equal to [`u32::MAX`] if the file's true size is larger.
    pub fsize: u32,
}

impl FileMetadata {
    /// Loads file metadata.
    ///
    /// This function loads the metadata for a path, in a platform-dependent way, determined at compile time.
    /// For details of the metadata returned, see the [`FileMetadata`] documentation.
    ///
    /// This function will return an error if the path does not point to a valid, accessible file on the
    /// filesystem; and no doubt for various platform-dependent reasons.
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
