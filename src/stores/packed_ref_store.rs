use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::anyhow;

use crate::stores::{BranchLocation, BranchSpec, RefSpec, RefStore, RefTarget, TargetedRef};

/// The git-compatible "packed refs" store.
///
/// This store's data is loaded using the [`PackedRefStore::new_from_file()`] function, and is
/// kept in memory after loading.
pub struct PackedRefStore {
    path: PathBuf,
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
        let contents = Self::parse_file(&file)?;
        Ok(Self {
            contents,
            path: file.as_ref().to_path_buf(),
        })
    }

    #[cfg(test)]
    fn new_from_map(map: HashMap<String, String>) -> Self {
        Self {
            contents: map,
            path: PathBuf::new(),
        }
    }

    fn parse_file<P: AsRef<Path>>(file: P) -> Result<HashMap<String, String>, anyhow::Error> {
        let file_contents = std::fs::read_to_string(file)?;
        Self::parse_file_contents(&file_contents)
    }

    fn parse_file_contents(file_contents: &str) -> Result<HashMap<String, String>, anyhow::Error> {
        let file_contents = file_contents.lines();
        let mut parsed_contents = HashMap::<String, String>::new();
        let mut counter = 0usize;
        for line in file_contents {
            counter += 1;
            let Some((rspec, target)) = Self::parse_file_line(line, counter)? else {
                continue;
            };
            parsed_contents.insert(rspec, target);
        }
        Ok(parsed_contents)
    }

    fn parse_file_line(
        line: &str,
        line_number: usize,
    ) -> Result<Option<(String, String)>, anyhow::Error> {
        let line = line.trim();
        if line.is_empty() || line.starts_with("#") {
            return Ok(None);
        }
        let Some(split_idx) = line.find(" ") else {
            return Err(anyhow!("line {line_number} does not contain space"));
        };
        let rspec = RefSpec::from_str(&line[(split_idx + 1)..])?;
        let target = line[..split_idx].to_string();
        Ok(Some((rspec.to_string(), target)))
    }

    fn specs(&self) -> impl Iterator<Item = RefSpec> + use<'_> {
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

    fn branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        Ok(self
            .specs()
            .filter_map(|r| match r {
                RefSpec::Branch(x) => Some(x),
                _ => None,
            })
            .collect::<Vec<BranchSpec>>())
    }

    fn local_branches(&self) -> Result<Vec<BranchSpec>, anyhow::Error> {
        Ok(self
            .branches()?
            .into_iter()
            .filter(|b| b.location == BranchLocation::Local)
            .collect())
    }

    fn tags(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        Ok(self
            .specs()
            .filter_map(|r| match r {
                RefSpec::Tag(_) => Some(r),
                _ => None,
            })
            .collect::<Vec<RefSpec>>())
    }

    fn all_refs(&self) -> Result<Vec<RefSpec>, anyhow::Error> {
        Ok(self.specs().collect::<Vec<RefSpec>>())
    }

    fn all_ref_targets(&self) -> Result<Vec<TargetedRef>, anyhow::Error> {
        Ok(self
            .contents
            .iter()
            .map(|x| TargetedRef {
                spec: RefSpec::from_str(x.0).unwrap(),
                target: RefTarget::from_str(x.1).unwrap(),
            })
            .collect())
    }

    fn resolve_target(&self, r: &RefSpec) -> Result<Option<RefTarget>, anyhow::Error> {
        let key = r.to_string();
        if !self.contents.contains_key(&key) {
            Ok(None)
        } else {
            Ok(Some(RefTarget::from_str(&self.contents[&key])?))
        }
    }

    fn search_remotes_for_branch(&self, name: &str) -> Result<Vec<BranchSpec>, anyhow::Error> {
        Ok(self
            .specs()
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

    fn create_update_ref(
        &self,
        _refspec: &RefSpec,
        _target: &RefTarget,
    ) -> Result<(), anyhow::Error> {
        Err(anyhow!("cannot update packed refs"))
    }

    fn delete_ref(&mut self, refspec: &RefSpec) -> Result<(), anyhow::Error> {
        let ref_name = refspec.to_string();
        if self.contents.contains_key(&ref_name) {
            self.contents.remove(&ref_name);
        }
        let file_contents = std::fs::read_to_string(&self.path)?;
        let file_contents = file_contents.lines();
        let tmp_name = self.path.with_added_extension(".tmp");
        {
            let mut output_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .truncate(true)
                .open(&tmp_name)?;
            for line in file_contents {
                if !line.ends_with(&ref_name) {
                    writeln!(output_file, "{line}")?;
                }
            }
        }
        std::fs::rename(&tmp_name, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, str::FromStr};

    use crate::stores::{RefSpec, RefStore};

    use super::PackedRefStore;

    #[test]
    fn parse_file_line_succeeds_for_empty_line() {
        let test_input = "";

        let test_output = PackedRefStore::parse_file_line(test_input, 0).unwrap();

        assert_eq!(None, test_output);
    }

    #[test]
    fn parse_file_line_succeeds_for_whitespace_line() {
        let test_input = "    \t ";

        let test_output = PackedRefStore::parse_file_line(test_input, 0).unwrap();

        assert_eq!(None, test_output);
    }

    #[test]
    fn parse_file_line_fails_for_line_without_space() {
        let test_input = "fcmsdalfihjnelrk";

        PackedRefStore::parse_file_line(test_input, 37).unwrap_err();
    }

    #[test]
    fn parse_file_line_succeeds_for_valid_line() {
        let test_input = "1111111111111111111111111111111111111111 refs/heads/branch";

        let test_result = PackedRefStore::parse_file_line(test_input, 73)
            .unwrap()
            .unwrap();

        assert_eq!("refs/heads/branch", test_result.0);
        assert_eq!("1111111111111111111111111111111111111111", test_result.1);
    }

    #[test]
    fn parse_file_line_fails_for_line_with_invalid_ref() {
        let test_input = "1111111111111111111111111111111111111111 not_a_branch";

        PackedRefStore::parse_file_line(test_input, 62).unwrap_err();
    }

    #[test]
    fn parse_file_contents_succeeds_for_valid_input_lines() {
        let test_input = "# packed-refs\n\
        \n\
        1111111111111111111111111111111111111111 refs/heads/branch\n\
        2222222222222222222222222222222222222222 refs/heads/branch-2\n\
        1234123412341234123412341234123412341234 refs/remotes/server/tracking";

        let test_output = PackedRefStore::parse_file_contents(test_input).unwrap();

        assert_eq!(3, test_output.len());
        assert_eq!(
            "1111111111111111111111111111111111111111",
            test_output["refs/heads/branch"]
        );
        assert_eq!(
            "2222222222222222222222222222222222222222",
            test_output["refs/heads/branch-2"]
        );
        assert_eq!(
            "1234123412341234123412341234123412341234",
            test_output["refs/remotes/server/tracking"]
        );
    }

    #[test]
    fn parse_file_contents_fails_if_any_input_line_is_invalid() {
        let test_input = "# packed-refs\n\
        \n\
        1111111111111111111111111111111111111111 refs/heads/branch\n\
        2222222222222222222222222222222222222222 refs_heads_branch_2\n\
        1234123412341234123412341234123412341234 refs/remotes/server/tracking";

        PackedRefStore::parse_file_contents(test_input).unwrap_err();
    }

    #[test]
    fn create_errors() {
        let mut test_data = HashMap::new();
        test_data.insert(
            "refs/heads/branch".to_string(),
            "1111111111111111111111111111111111111111".to_string(),
        );
        test_data.insert(
            "refs/heads/branch-2".to_string(),
            "2222222222222222222222222222222222222222".to_string(),
        );
        test_data.insert(
            "refs/remotes/server/tracking".to_string(),
            "1234123412341234123412341234123412341234".to_string(),
        );
        let test_object = PackedRefStore::new_from_map(test_data);

        test_object.create().unwrap_err();
    }

    #[test]
    fn is_existing_ref_succeeds_for_existing_ref() {
        let mut test_data = HashMap::new();
        test_data.insert(
            "refs/heads/branch".to_string(),
            "1111111111111111111111111111111111111111".to_string(),
        );
        test_data.insert(
            "refs/heads/branch-2".to_string(),
            "2222222222222222222222222222222222222222".to_string(),
        );
        test_data.insert(
            "refs/remotes/server/tracking".to_string(),
            "1234123412341234123412341234123412341234".to_string(),
        );
        let test_object = PackedRefStore::new_from_map(test_data);
        let test_param = RefSpec::from_str("refs/heads/branch").unwrap();

        let test_output = test_object.is_existing_ref(&test_param).unwrap();

        assert!(test_output);
    }

    #[test]
    fn is_existing_ref_succeeds_for_non_existing_ref() {
        let mut test_data = HashMap::new();
        test_data.insert(
            "refs/heads/branch".to_string(),
            "1111111111111111111111111111111111111111".to_string(),
        );
        test_data.insert(
            "refs/heads/branch-2".to_string(),
            "2222222222222222222222222222222222222222".to_string(),
        );
        test_data.insert(
            "refs/remotes/server/tracking".to_string(),
            "1234123412341234123412341234123412341234".to_string(),
        );
        let test_object = PackedRefStore::new_from_map(test_data);
        let test_param = RefSpec::from_str("refs/heads/not-a-branch").unwrap();

        let test_output = test_object.is_existing_ref(&test_param).unwrap();

        assert!(!test_output);
    }
}
