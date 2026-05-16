use anyhow::Context;
use glob::Pattern;
use std::{collections::HashMap, error::Error, fmt::Display, path::Path, str::FromStr};

use crate::objects::Blob;

/// An [`Error`] struct that indicates a zero-content line in an ignore file.
///
/// This is not an error as such, because it could be an empty line or a comment line,
/// but it indicates the line can't be parsed as an ignore pattern.
#[derive(Debug)]
pub struct EmptyIgnorePattern {}

impl Display for EmptyIgnorePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "empty ignore pattern")
    }
}

impl Error for EmptyIgnorePattern {}

/// A pattern representing a single line from a Git ignore file.
///
/// The pattern can indicate a pattern that should be ignored, or a pattern that should *not* be ignored.
pub struct IgnorePattern {
    patterns: Vec<Pattern>,
    exclude: bool,
}

impl FromStr for IgnorePattern {
    type Err = EmptyIgnorePattern;

    /// Parse a line from a Git ignore file and turn it into an [`IgnorePattern`] object.
    ///
    /// This function returns `Err` if the line is empty or is a comment.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let text = s.trim();
        if text.is_empty() || text.starts_with("#") {
            Err(EmptyIgnorePattern {})
        } else if let Some(t) = text.strip_prefix("!/") {
            Self::patternify(t, false, true)
        } else if let Some(t) = text.strip_prefix("!") {
            Self::patternify(t, false, false)
        } else if let Some(t) = text.strip_prefix("/") {
            Self::patternify(t, true, true)
        } else if let Some(t) = text.strip_prefix("\\") {
            Self::patternify(t, true, false)
        } else {
            Self::patternify(text, true, false)
        }
    }
}

impl IgnorePattern {
    /// Check a path against a pattern.
    ///
    /// This method returns `Some(true)` if the pattern matches and the path **should** be excluded,
    /// `Some(false)` if the pattern matches and the path **should not** be excluded, or `None` if the pattern
    /// does not match.
    pub fn matches(&self, path: &str) -> Option<bool> {
        for p in &self.patterns {
            if p.matches(path) {
                return Some(self.exclude);
            }
        }
        None
    }

    /// Check a path against a set of patterns.  The order of patterns in the set is significant.
    ///
    /// This method returns `Some(true)` if one or more patterns matches and the final matching pattern is exclusionary.
    /// It returns `Some(false)` if one or more patterns matches and the final matching pattern is inclusive.  It returns
    /// `None` if no patterns in the set match
    pub fn matches_set(rules: &[IgnorePattern], text: &str) -> Option<bool> {
        let mut result: Option<bool> = None;
        for pattern in rules {
            let match_result = pattern.matches(text);
            if match_result.is_some() {
                result = match_result;
            }
        }
        result
    }

    fn patternify(text: &str, exclude: bool, relative: bool) -> Result<Self, EmptyIgnorePattern> {
        let relative = match relative {
            true => true,
            false => {
                if let Some(t) = text.strip_suffix("/") {
                    t.contains("/")
                } else {
                    text.contains("/")
                }
            }
        };
        let mut patterns = Vec::<Pattern>::with_capacity(1);
        Self::pattern_push_safe(&mut patterns, text);
        Self::pattern_push_safe(&mut patterns, &format!("{text}/**"));
        if !relative {
            Self::pattern_push_safe(&mut patterns, &format!("**/{text}"));
            Self::pattern_push_safe(&mut patterns, &format!("**/{text}/**"));
        }
        Ok(IgnorePattern { patterns, exclude })
    }

    fn pattern_push_safe(patterns: &mut Vec<Pattern>, text: &str) {
        let pattern = Pattern::new(text);
        if let Ok(pattern) = pattern {
            patterns.push(pattern);
        }
    }
}

/// The set of all ignore rules for a repository.
///
/// This can consist of repository- and system-wide rules (also known as "absolute rules"), and rules which
/// only apply to a specific directory tree ("scoped rules").  The scoped rules must come from files which
/// have already been added to the repository.
pub struct IgnoreInfo {
    absolute: Vec<IgnorePattern>,
    scoped: HashMap<String, Vec<IgnorePattern>>,
}

impl IgnoreInfo {
    #[cfg(test)]
    /// Create a new ignore pattern.
    ///
    /// This function is only used in unit testing; in the live code, the only route to creating
    /// an [`IgnoreInfo`] object is by parsing files.
    pub fn new(absolute: Vec<IgnorePattern>, scoped: HashMap<String, Vec<IgnorePattern>>) -> Self {
        IgnoreInfo { absolute, scoped }
    }

    /// Load a set of ignore info from files and blobs.
    ///
    /// This function takes a vector of paths to load and parse as absolute rules, and a map of blobs to parse as
    /// scoped rules.  The keys to this map are Git-format paths, and the values are sets of rules which apply to
    /// the directory tree under that path.
    ///
    /// This function will return an error if there are any filesystem errors reading any of the absolute rule files.
    pub fn from_files<P: AsRef<Path>>(
        absolute_rule_files: Vec<P>,
        scoped_files: HashMap<String, Blob>,
    ) -> Result<Self, anyhow::Error> {
        let mut absolute_ignores = Vec::<IgnorePattern>::new();
        for f in absolute_rule_files {
            absolute_ignores.append(&mut read_ignore_file(f)?);
        }
        let mut dir_ignores = HashMap::<String, Vec<IgnorePattern>>::new();
        for entry in scoped_files.into_iter() {
            dir_ignores.insert(
                entry.0,
                String::from_utf8_lossy(entry.1.data())
                    .lines()
                    .filter_map(|x| IgnorePattern::from_str(x).ok())
                    .collect(),
            );
        }
        Ok(Self {
            absolute: absolute_ignores,
            scoped: dir_ignores,
        })
    }

    /// Check if a path should be ignored.
    ///
    /// Returns `true` to ignore a file or directory, false to include it.  The path should be relative
    /// to the root of the repository worktree.
    pub fn check(&self, path: &Path) -> bool {
        let scoped_result = self.check_scoped(path);
        if let Some(scoped_result) = scoped_result {
            return scoped_result;
        };
        self.check_absolute(path)
    }

    fn check_scoped(&self, path: &Path) -> Option<bool> {
        self.check_scoped_recursive(path, path.parent())
    }

    fn check_scoped_recursive(&self, path: &Path, scope: Option<&Path>) -> Option<bool> {
        let str_path = path.to_string_lossy().to_string();
        let str_scope = match scope {
            None => String::new(),
            Some(scp) => {
                let str_scp = scp.to_string_lossy();
                if str_scp.is_empty() {
                    String::new()
                } else {
                    str_scp.to_string()
                }
            }
        };
        if self.scoped.contains_key(&str_scope) {
            let result = IgnorePattern::matches_set(&self.scoped[&str_scope], &str_path);
            if result.is_some() {
                return result;
            }
        }
        if !str_scope.is_empty() {
            self.check_scoped_recursive(path, scope.unwrap().parent())
        } else {
            None
        }
    }

    // Because this is called after check_scoped(), it always returns a final definite "exclude (true) or include-by-default (false)"
    fn check_absolute(&self, path: &Path) -> bool {
        for ip in &self.absolute {
            let result = ip.matches(&path.to_string_lossy());
            if let Some(result) = result {
                return result;
            }
        }
        false
    }
}

fn read_ignore_file<P: AsRef<Path>>(path: P) -> Result<Vec<IgnorePattern>, anyhow::Error> {
    let file_contents = std::fs::read_to_string(path).context("error reading ignore file")?;
    Ok(file_contents
        .lines()
        .filter_map(|x| IgnorePattern::from_str(x).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_file_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_included_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_in_first_subdir_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("a/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_in_first_subdir_included_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("a/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_in_deeper_subdir_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("a/b/c/d/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_in_deeper_subdir_included_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("a/b/c/d/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_with_parent_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test/a");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_with_parent_included_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test/a");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_with_deep_parent_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("a/test/a");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_excluded_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_included_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_in_first_subdir_excluded_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/a/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_in_first_subdir_included_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/a/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_in_deeper_subdir_excluded_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/a/b/c/d/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_in_deeper_subdir_included_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/a/b/c/d/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_with_parent_excluded_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/test/a");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_with_parent_included_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/test/a");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_with_deep_parent_excluded_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/a/test/a");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn does_not_match_file_in_root_dir_excluded_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_none());
    }

    #[test]
    fn does_not_match_file_in_root_dir_included_literally_in_sub_dir() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert("sub".to_string(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_none());
    }

    #[test]
    fn matches_file_excluded_in_root_dir_by_relative_path() {
        let test_pattern = IgnorePattern::from_str("te/st").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("te/st");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_included_in_root_dir_by_relative_path() {
        let test_pattern = IgnorePattern::from_str("!te/st").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("te/st");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_parent_excluded_in_root_dir_by_relative_path() {
        let test_pattern = IgnorePattern::from_str("te/st").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("te/st/file");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_parent_included_in_root_dir_by_relative_path() {
        let test_pattern = IgnorePattern::from_str("!te/st").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("te/st/file");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn does_not_match_file_in_subdir_with_relative_path() {
        let test_pattern = IgnorePattern::from_str("te/st").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/te/st");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_none());
    }

    #[test]
    fn matches_file_excluded_in_root_dir_by_relative_path_starting_with_slash() {
        let test_pattern = IgnorePattern::from_str("/test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_included_in_root_dir_by_relative_path_starting_with_slash() {
        let test_pattern = IgnorePattern::from_str("!/test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn matches_file_parent_excluded_in_root_dir_by_relative_path_starting_with_slash() {
        let test_pattern = IgnorePattern::from_str("/test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test/file");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_file_parent_included_in_root_dir_by_relative_path_starting_with_slash() {
        let test_pattern = IgnorePattern::from_str("!test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test/file");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn does_not_match_file_in_subdir_with_relative_path_starting_with_slash() {
        let test_pattern = IgnorePattern::from_str("/test").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("sub/test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_none());
    }

    #[test]
    fn matches_dir_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test/").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test/");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_dir_included_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("!test/").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test/");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }

    #[test]
    fn does_not_match_file_when_dir_with_same_name_is_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test/").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_none());
    }

    #[test]
    fn does_not_match_file_when_dir_with_same_name_is_included_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("!test/").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("test");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_none());
    }

    #[test]
    fn matches_dir_in_sub_dir_excluded_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("test/").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("a/b/test/");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(result.unwrap());
    }

    #[test]
    fn matches_dir_in_sub_dir_included_literally_in_root_dir() {
        let test_pattern = IgnorePattern::from_str("!test/").unwrap();
        let mut scoped_map = HashMap::<String, Vec<IgnorePattern>>::new();
        scoped_map.insert(String::new(), vec![test_pattern]);
        let test_info = IgnoreInfo::new(Vec::<IgnorePattern>::new(), scoped_map);
        let test_path = Path::new("a/b/test/");

        let result = test_info.check_scoped(test_path);

        assert!(result.is_some());
        assert!(!result.unwrap());
    }
}
