use std::{
    collections::VecDeque,
    fs::ReadDir,
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, TimeZone};

pub fn u32_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u32 {
    u32::from_be_bytes(data[start_idx..(start_idx + 4)].try_into().unwrap())
}

pub fn u16_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u16 {
    u16::from_be_bytes(data[start_idx..(start_idx + 2)].try_into().unwrap())
}

pub fn datetime_to_bytes<Z>(dt: &DateTime<Z>) -> impl Iterator<Item = u8>
where
    Z: TimeZone,
{
    (dt.timestamp() as u32)
        .to_be_bytes()
        .iter()
        .map(|b| *b)
        .chain(dt.timestamp_subsec_nanos().to_be_bytes().iter().map(|b| *b))
        .collect::<Vec<u8>>()
        .into_iter()
}

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
