use glob::Pattern;
use std::{collections::HashMap, path::Path};

pub struct IgnorePattern {
    patterns: Vec<Pattern>,
    exclude: bool,
}

impl IgnorePattern {
    pub fn from_str(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() || text.starts_with("#") {
            None
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

    pub fn matches(&self, path: &str) -> Option<bool> {
        for p in &self.patterns {
            if p.matches(path) {
                return Some(self.exclude);
            }
        }
        None
    }

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

    fn patternify(text: &str, exclude: bool, relative: bool) -> Option<Self> {
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
        Some(IgnorePattern { patterns, exclude })
    }

    fn pattern_push_safe(patterns: &mut Vec<Pattern>, text: &str) {
        let pattern = Pattern::new(text);
        if let Ok(pattern) = pattern {
            patterns.push(pattern);
        }
    }
}

pub struct IgnoreInfo {
    absolute: Vec<IgnorePattern>,
    scoped: HashMap<String, Vec<IgnorePattern>>,
}

impl IgnoreInfo {
    pub fn new(absolute: Vec<IgnorePattern>, scoped: HashMap<String, Vec<IgnorePattern>>) -> Self {
        IgnoreInfo { absolute, scoped }
    }

    // true for ignore, false for include
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
        //println!("checking path {str_path} (parent path is {str_scope})");
        if self.scoped.contains_key(&str_scope) {
            //println!("Checking at this level");
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
