use anyhow::{anyhow, Context};
use flate2::{bufread::ZlibEncoder, read::ZlibDecoder, Compression};
use std::{
    fs,
    io::{BufReader, Cursor, Read},
    path::{Path, PathBuf},
};

use crate::{objects::RawObject, repo::is_partial_object_id, stores::ObjectStore};

pub struct LooseObjectStore {
    base_path: PathBuf,
}

impl LooseObjectStore {
    pub fn new<T: AsRef<Path>>(path: &T) -> Result<Self, anyhow::Error> {
        let path = path.as_ref();
        if path.exists() && !path.is_dir() {
            Err(anyhow!("Path exists but is not a directory"))
        } else {
            Ok(LooseObjectStore {
                base_path: path.to_path_buf(),
            })
        }
    }

    // Object file names have had the first two characters removed.
    // Because of that, they look like valid object IDs that are 38 chars long,
    // even though they're not, on their own, valid object IDs
    fn is_object_file_name(name: &str) -> bool {
        is_partial_object_id(name) && name.len() == 38
    }

    fn object_file(&self, object_id: &str) -> PathBuf {
        self.base_path.join(&object_id[..2]).join(&object_id[2..])
    }
}

impl ObjectStore for LooseObjectStore {
    fn create(&self) -> Result<(), anyhow::Error> {
        if !self.base_path.exists() {
            fs::create_dir_all(&self.base_path).context("Failed to create loose objects dir")
        } else {
            Ok(())
        }
    }

    fn _is_writeable(&self) -> bool {
        true
    }

    fn search_objects(&self, partial_object_id: &str) -> Result<Vec<String>, anyhow::Error> {
        let search_dir = self.base_path.join(&partial_object_id[..2]);
        let mut collected = Vec::<String>::new();
        if !search_dir.exists() {
            return Ok(collected);
        }
        let dir_entries = fs::read_dir(&search_dir)
            .context(format!(
                "Trying to read path {}",
                &search_dir.to_string_lossy()
            ))?
            .collect::<Result<Vec<_>, std::io::Error>>()?;
        for mut f in dir_entries
            .iter()
            .map(|e| e.file_name().into_string().unwrap_or("".to_owned()))
            .filter(|f| f.starts_with(&partial_object_id[2..]) && Self::is_object_file_name(f))
        {
            f.insert_str(0, &partial_object_id[..2]);
            collected.push(f);
        }
        Ok(collected)
    }

    fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error> {
        Ok(self.object_file(object_id).is_file())
    }

    fn read_object(
        &self,
        object_id: &str,
    ) -> Result<Option<RawObject>, anyhow::Error> {
        let path = self.object_file(object_id);
        if !path.is_file() {
            return Ok(None);
        }
        let file = fs::File::open(path)?;
        let mut decompressor = ZlibDecoder::new(file);
        let mut data: Vec<u8> = vec![];
        decompressor.read_to_end(&mut data)?;
        Ok(Some(RawObject::from_data_with_header(&data, object_id)?))
    }

    fn write_raw_object(
        &self,
        obj: &RawObject,
    ) -> Result<String, anyhow::Error> {
        let path = self.object_file(obj.object_id());

        if !path.exists() {
            let obj_parent_dir = path.parent();
            if let Some(obj_parent_dir) = obj_parent_dir {
                if !obj_parent_dir.exists() {
                    fs::create_dir_all(obj_parent_dir)?;
                }
            }
            let mut file = fs::File::create(path)?;
            let mut compressor = ZlibEncoder::new(
                BufReader::new(Cursor::new(obj.content_with_header())),
                Compression::best(),
            );
            std::io::copy(&mut compressor, &mut file)?;
        }
        Ok(obj.object_id().to_string())
    }
}
