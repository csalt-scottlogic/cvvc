//! This module implements a repository's "loose object" store.

use anyhow::{anyhow, Context};
use flate2::{bufread::ZlibEncoder, read::ZlibDecoder, Compression};
use std::{
    fs,
    io::{BufReader, Cursor, Read},
    path::{Path, PathBuf},
};

use crate::{
    objects::{RawObject, RawObjectData},
    repo::is_partial_object_id,
    stores::ObjectStore,
};

/// The "loose object store" of a Git or CVVC repository.
///
/// This stores objects as individual files, each with a header describing the object's type and size.
pub struct LooseObjectStore {
    base_path: PathBuf,
}

impl LooseObjectStore {
    /// Create a new loose object store.
    ///
    /// The path to the loose object store should by convention be `.git/objects`, but this is not enforced.  The path
    /// does not have to exist, but if it does exist, it must be a directory.  If the path does not exist, the
    /// [`LooseObjectStore::create`] method can be used to create it.
    ///
    /// This function returns an error if the path exists but is not a directory.
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
    /// Create a loose object store.
    ///
    /// This method checks that the path to the store exists in the filesystem, and creates it if it does not.
    ///
    /// This method returns an error if the path does not exist and could not be created.
    fn create(&self) -> Result<(), anyhow::Error> {
        if !self.base_path.exists() {
            fs::create_dir_all(&self.base_path).context("Failed to create loose objects dir")
        } else {
            Ok(())
        }
    }

    /// Confirm that this object store is writeable.
    ///
    /// At present this method always returns `true`, as there is no way in CVVC to make a repository read-only.
    fn _is_writeable(&self) -> bool {
        true
    }

    /// Search the object store for objects whose IDs begin with a given prefix.
    ///
    /// This method searches the loose object store for objects whose IDs begin with the given partial ID.
    /// On success, it returns a [`Vec<String>`] containing the full IDs of all matching objects, which can
    /// be empty if none were found.
    ///
    /// The method will return an error if the parameter is not a valid partial or full object ID.
    ///
    /// The method will return an error if it encounters any filesystem errors.
    fn search_objects(&self, partial_object_id: &str) -> Result<Vec<String>, anyhow::Error> {
        if !is_partial_object_id(partial_object_id) {
            return Err(anyhow!("parameter is not a valid partial object ID"));
        }
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

    /// Confirm whether or not the given object exists in the loose object store.
    ///
    /// On success, this method returns whether or not an object with the given ID *appears* to be
    /// present in the object store.  It does not confirm whether or not the object is readable.
    ///
    /// This method returns an error if the given ID is not a valid partial object ID, or if an error occurs
    /// on reading the filesystem.
    ///
    /// If the parameter is a valid *partial* object ID, rather than a complete object ID, this method will
    /// normally return `Ok(None)`.
    fn has_object(&self, object_id: &str) -> Result<bool, anyhow::Error> {
        if !is_partial_object_id(object_id) {
            Err(anyhow!("object ID is not valid"))
        } else {
            let object_file = self.object_file(object_id);
            Ok(object_file.try_exists()? && object_file.is_file())
        }
    }

    /// Read a [`RawObject`] from the loose object store.
    ///
    /// This method reads a [`RawObject`] from the loose object store, if it exists there.  If the given ID is a legal
    /// object ID but does not refer to an extent object in the loose object store, the method returns `Ok(None)`.
    ///
    /// An error is returned if:
    /// - the object ID is not a legal object ID
    /// - the object's file cannot be read, or any other kind of filesystem error occurs
    /// - the object's file cannot be decompressed
    /// - the decompressed object data cannot be minimally parsed.
    ///
    /// "Minimal" parsing means that the decompressed data cannot be parsed sufficiently to create a [`RawObject`]
    /// instance.  This essentially requires that the decompressed data must begin with the expected header describing
    /// the object's type and size, and that the length of the remaining data matches the length given in the header.
    fn read_raw_object_data(
        &self,
        object_id: &str,
    ) -> Result<Option<RawObjectData>, anyhow::Error> {
        if !is_partial_object_id(object_id) {
            return Err(anyhow!("object ID is not valid"));
        }
        let path = self.object_file(object_id);
        if !path.is_file() {
            return Ok(None);
        }
        let file = fs::File::open(path)?;
        let mut decompressor = ZlibDecoder::new(file);
        let mut data: Vec<u8> = vec![];
        decompressor.read_to_end(&mut data)?;
        Ok(Some(RawObjectData::from_data_with_header(&data)?))
    }

    /// Write a [`RawObject`] to the loose object store.
    ///
    /// This method writes the content of a [`RawObject`] to the loose object store, if it does not already
    /// exist, and returns the object's ID.
    ///
    /// If an object with the same ID already exists in the loose object store, this method does not overwrite
    /// it, assuming that collisions are rare enough that we can assume the files have the same content.
    ///
    /// This method returns an error if it encounters errors on writing to the filesystem.
    fn write_raw_object(&self, obj: &RawObject) -> Result<String, anyhow::Error> {
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
