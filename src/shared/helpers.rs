use std::{
    collections::VecDeque,
    fs::ReadDir,
    io,
    path::{Path, PathBuf},
};

pub fn u32_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u32 {
    u32::from_be_bytes(data[start_idx..(start_idx + 4)].try_into().unwrap())
}

pub fn u16_from_be_bytes_unchecked(data: &[u8], start_idx: usize) -> u16 {
    u16::from_be_bytes(data[start_idx..(start_idx + 2)].try_into().unwrap())
}

pub fn path_translate(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<String>>()
        .join("/")
}

pub fn path_translate_rev(path: &str) -> PathBuf {
    PathBuf::from_iter(path.split("/"))
}

pub fn walk_fs(path: &Path) -> io::Result<FsWalker> {
    let dirs = VecDeque::<PathBuf>::new();
    let dir_reader = path.read_dir()?;

    Ok(FsWalker { dirs, dir_reader })
}

pub struct FsWalker {
    dirs: VecDeque<PathBuf>,
    dir_reader: ReadDir,
}

impl Iterator for FsWalker {
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
                    let next_dir = self.dirs.pop_back()?;
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
