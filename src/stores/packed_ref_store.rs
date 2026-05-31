use std::{collections::HashMap, path::Path, str::FromStr};

use anyhow::anyhow;

use crate::stores::{BranchLocation, BranchSpec, RefSpec, RefStore};

/// The git-compatible "packed refs" store.
///
/// This store's data is loaded using the [`PackedRefStore::new_from_file()`] function, and is
/// kept in memory after loading.
pub struct PackedRefStore {
    contents: HashMap<String, String>,
}

impl PackedRefStore {
    /// Load a packed-ref file.
    ///
    /// The file is expected to be a text file with lines consisting of an object ID
    /// and a ref, separated by a space.  Lines starting with a `#` character, or solely
    /// containing whitespace, are ignored.
    ///
    /// # Errors
    ///
    /// This function returns an error if it encounters any filesystem errors, if the file contains a
    /// non-comment line that does not contain a space character, or if the file contains
    /// an invalid ref name.
    pub fn new_from_file<P: AsRef<Path>>(file: P) -> Result<Self, anyhow::Error> {
        let contents = Self::parse_file(file)?;
        Ok(Self { contents })
    }

    fn parse_file<P: AsRef<Path>>(file: P) -> Result<HashMap<String, String>, anyhow::Error> {
        let file_contents = std::fs::read_to_string(file)?;
        let file_contents = file_contents.lines();
        let mut parsed_contents = HashMap::<String, String>::new();
        let mut counter = 0usize;
        for line in file_contents {
            counter += 1;
            let line = line.trim();
            if line.is_empty() || line.starts_with("#") {
                continue;
            }
            let split_idx = line.find(" ");
            let Some(split_idx) = split_idx else {
                return Err(anyhow!("line {} does not contain space", counter));
            };
            let target = line[..split_idx].to_string();
            let rspec = RefSpec::from_str(&line[(split_idx + 1)..])?;
            parsed_contents.insert(rspec.to_string(), target);
        }
        Ok(parsed_contents)
    }

    fn get_specs(&self) -> impl Iterator<Item = RefSpec> + use<'_> {
        self.contents.keys().map(|s| RefSpec::from_str(s).unwrap())
    }
}

impl RefStore for PackedRefStore {
    fn create(&self) -> Result<(), anyhow::Error> {
        Err(anyhow!("cannot create new packed ref store"))
    }

    fn is_existing_ref(&self, r: &RefSpec) -> Result<bool, anyhow::Error> {
        Ok(self.contents.contains_key(&r.to_string()))
    }

    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        Ok(self
            .get_specs()
            .filter_map(|r| match r {
                RefSpec::Branch(x) => Some(x),
                _ => None,
            })
            .collect::<Vec<BranchSpec>>())
    }

    fn tags(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        Ok(self
            .get_specs()
            .filter_map(|r| match r {
                RefSpec::Tag(_) => Some(r),
                _ => None,
            })
            .collect::<Vec<RefSpec>>())
    }

    fn all_refs(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        Ok(self.get_specs().collect::<Vec<RefSpec>>())
    }

    fn all_ref_targets(&self) -> Result<Vec<(RefSpec, String)>, anyhow::Error> {
        Ok(self
            .contents
            .iter()
            .map(|x| (RefSpec::from_str(x.0).unwrap(), x.1.to_string()))
            .collect::<Vec<(RefSpec, String)>>())
    }

    fn resolve_target(&self, r: &RefSpec) -> Result<Option<String>, anyhow::Error> {
        let key = r.to_string();
        if !self.contents.contains_key(&key) {
            Ok(None)
        } else {
            Ok(Some(self.contents[&key].to_string()))
        }
    }

    fn search_remotes_for_branch(&self, name: &str) -> Result<Vec<BranchSpec>, anyhow::Error> {
        Ok(self
            .get_specs()
            .filter_map(|r| match r {
                RefSpec::Branch(x) => Some(x),
                _ => None,
            })
            .filter_map(|b| match b.location {
                BranchLocation::Remote(_) => Some(b),
                _ => None,
            })
            .filter(|b| b.name == name)
            .collect::<Vec<BranchSpec>>())
    }

    fn create_ref(&self, _r: &RefSpec, _object_id: &str) -> Result<(), anyhow::Error> {
        Err(anyhow!("cannot create new packed refs"))
    }

    fn update_branch(&self, _branch: &BranchSpec, _commit_id: &str) -> Result<(), anyhow::Error> {
        Err(anyhow!("cannot update packed refs"))
    }
}
