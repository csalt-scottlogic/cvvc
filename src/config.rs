use anyhow::{anyhow, Context};
use ini::{Ini, Properties};
use std::{
    env,
    ffi::OsStr,
    path::{Path, PathBuf},
    str::FromStr,
};

/// Global configuration
///
/// The global configuration includes anything not repository-specific.  It can be set at the system level, and also
/// includes configuration loaded at the user level.
///
/// Although configuration is primarily loaded from configuration files, some specific settings can be overridden by
/// environment variables.
pub struct GlobalConfig {
    system_config: Ini,
    user_config: Ini,
}

impl GlobalConfig {
    /// Create a [`GlobalConfig`] object by loading configuration from named files.
    ///
    /// Both files are optional.
    ///
    /// This function is infallible.  Any errors on loading the configuration are ignored.
    pub fn from_files<T: AsRef<Path>, U: AsRef<Path>>(
        system_path: Option<T>,
        user_path: Option<U>,
    ) -> Self {
        GlobalConfig {
            system_config: load_ini_safe(system_path),
            user_config: load_ini_safe(user_path),
        }
    }

    /// Create a [`GlobalConfig`] object by loading configuration from the default files.
    ///
    /// At present, this function doesn't load a system file, because CVVC doesn't use any
    /// system-level configuration.  It tries to find a user
    /// configuration file in the same places Git looks.
    ///
    /// This function is infallible.  Any errors on loading the configuration are ignored.
    pub fn from_default_files() -> Self {
        Self::from_files(Self::find_system_file(), Self::find_user_file())
    }

    /// Return all configured values of a named setting.
    ///
    /// This method looks up a setting by section and name, in both the system and user configurations.
    /// It returns all values found as a [`Vec<String>`], which may be empty.  In the result, if multiple
    /// values are returned, settings found in the system configuration (if any) precede settings found in
    /// the user configuration (if any).
    pub fn get_setting_all(&self, section: &str, key: &str) -> Vec<String> {
        let mut vals = get_setting_from_ini(&self.system_config, section, key);
        vals.append(&mut get_setting_from_ini(&self.user_config, section, key));
        vals
    }

    /// Return a single configured value of a named setting, if present.
    ///
    /// This method looks up a setting by section and name, in both the system and user configurations.
    /// If it finds any values, it returns the last value it finds.  If it does not find any values,
    /// it returns `None`.
    ///
    /// If a setting is present in both the system and user configurations, this method returns a value
    /// from the user configuration.
    pub fn get_setting(&self, section: &str, key: &str) -> Option<String> {
        let vals = self.get_setting_all(section, key);
        if !vals.is_empty() {
            Some(vals.last().unwrap().to_owned())
        } else {
            None
        }
    }

    /// Get the configured user name, if set.
    ///
    /// This method only returns the user name set in configuration files; it ignores environment variables.
    ///
    /// The user name setting should be the user's real name, such as "Caitlin Thomas".
    pub fn user_name(&self) -> Option<String> {
        self.get_setting("user", "name")
    }

    /// Get the configured user email, if set.
    ///
    /// This method only returns the user email address set in configuration files; it ignores environment variables.
    ///
    /// The user email setting should just be an email address, without any real name part, and not surrounded by
    /// angle brackets.
    pub fn user_email(&self) -> Option<String> {
        self.get_setting("user", "email")
    }

    /// Get the configured author name, if set.
    ///
    /// This method only returns the author name set in configuration files; it ignores environment variables.
    ///
    /// The author name setting should be the author's real name, such as "Caitlin Thomas".
    pub fn author_name(&self) -> Option<String> {
        self.get_setting("author", "name")
    }

    /// Get the configured author email address, if set.
    ///
    /// This method only returns the author email address set in configuration files; it ignores environment variables.
    ///
    /// The author email setting should just be an email address, without any real name part, and not surrounded by
    /// angle brackets.
    pub fn author_email(&self) -> Option<String> {
        self.get_setting("author", "email")
    }

    /// Get the configured committer name, if set.
    ///
    /// This method only returns the committer name set in configuration files; it ignores environment variables.
    ///
    /// The committer name setting should be the committer's real name, such as "Caitlin Thomas".
    pub fn committer_name(&self) -> Option<String> {
        self.get_setting("committer", "name")
    }

    /// Get the configured author email address, if set.
    ///
    /// This method only returns the author email address set in configuration files; it ignores environment variables.
    ///
    /// The author email setting should just be an email address, without any real name part, and not surrounded by
    /// angle brackets.
    pub fn committer_email(&self) -> Option<String> {
        self.get_setting("committer", "email")
    }

    /// Get the author name and email address, if set.
    ///
    /// This method returns a value of the form "Real Name <email.address@example.com>".
    ///
    /// If the environment variable `GIT_AUTHOR_NAME` is set, it uses that for the real name.  If it is not set,
    /// it uses the first item it finds from the following list:
    /// - the author name setting in configuration files
    /// - the user name setting in configuration files
    /// - the system username
    ///
    /// If none of the above are set, it sets the real name to "(unknown)".
    ///
    /// If the environment variable `GIT_AUTHOR_EMAIL` is set, it uses that for the email address.  If it is not set,
    /// it uses the first item it finds from the following list:
    /// - the author email setting in configuration files
    /// - the user email setting in configuration files
    /// - the system username and system hostname, in the form `<user@host>`
    ///
    /// If none of the above are found, it will use `<unknown@unknown>` for the email address.  If the system hostname
    /// but not the system username can be determined, or vice versa, it may use `<unknown@host>` or `<user@unknown>`.
    pub fn author(&self) -> String {
        let author_name = get_setting_from_env("GIT_AUTHOR_NAME")
            .or_else(|| self.author_name())
            .or_else(|| self.user_name())
            .or_else(|| whoami::username().ok())
            .unwrap_or_else(|| "(unknown)".to_string());
        let author_email = get_setting_from_env("GIT_AUTHOR_EMAIL")
            .or_else(|| self.author_email())
            .or_else(|| self.user_email())
            .or_else(|| {
                let sys_username = whoami::username().unwrap_or_else(|_| "unknown".to_string());
                let sys_hostname = whoami::hostname().unwrap_or_else(|_| "unknown".to_string());
                Some(format!("{sys_username}@{sys_hostname}"))
            })
            .unwrap();
        format!("{author_name} <{author_email}>")
    }

    /// Get the committer name and email address, if set.
    ///
    /// This method returns a value of the form "Real Name <email.address@example.com>".
    ///
    /// If the environment variable `GIT_COMMITTER_NAME` is set, it uses that for the real name.  If it is not set,
    /// it uses the first item it finds from the following list:
    /// - the committer name setting in configuration files
    /// - the user name setting in configuration files
    /// - the system username
    ///
    /// If none of the above are set, it sets the real name to "(unknown)".
    ///
    /// If the environment variable `GIT_COMMITTER_EMAIL` is set, it uses that for the email address.  If it is not set,
    /// it uses the first item it finds from the following list:
    /// - the committer email setting in configuration files
    /// - the user email setting in configuration files
    /// - the system username and system hostname, in the form `<user@host>`
    ///
    /// If none of the above are found, it will use `<unknown@unknown>` for the email address.  If the system hostname
    /// but not the system username can be determined, or vice versa, it may use `<unknown@host>` or `<user@unknown>`.
    pub fn committer(&self) -> String {
        let committer_name = get_setting_from_env("GIT_COMMITTER_NAME")
            .or_else(|| self.committer_name())
            .or_else(|| self.user_name())
            .or_else(|| whoami::username().ok())
            .unwrap_or_else(|| "<unknown>".to_string());
        let committer_email = get_setting_from_env("GIT_COMMITTER_EMAIL")
            .or_else(|| self.committer_email())
            .or_else(|| self.user_email())
            .or_else(|| {
                let sys_username = whoami::username().unwrap_or_else(|_| "unknown".to_string());
                let sys_hostname = whoami::hostname().unwrap_or_else(|_| "unknown".to_string());
                Some(format!("{sys_username}@{sys_hostname}"))
            })
            .unwrap();
        format!("{committer_name} <{committer_email}>")
    }

    /// Get the name of the default branch for a new repository.
    ///
    /// This method returns the value of the init.defaultbranch setting if set, and the string "main" if it is not.
    pub fn default_branch_name(&self) -> String {
        self.get_setting("init", "defaultbranch")
            .unwrap_or_else(|| String::from("main"))
    }

    /// Try to find the likely path of the user configuration file.
    ///
    /// If the environment variable `XDG_CONFIG_HOME` is set, this function returns the first of
    /// `$XDG_CONFIG_HOME/.gitconfig` or `$XDG_CONFIG_HOME/git/config` that exists.
    /// If that environment variable is not set, it looks for those files in the user's home directory.
    ///
    /// This function does not guarantee that either file exists.
    pub fn find_user_file() -> Option<PathBuf> {
        let home_dir = env::var("XDG_CONFIG_HOME")
            .ok()
            .and_then(|d| PathBuf::from_str(&d).ok())
            .or_else(env::home_dir);
        let home_dir = home_dir.as_ref()?;
        Self::find_user_file_in_dir(home_dir)
    }

    fn find_user_file_in_dir<P: AsRef<Path>>(dir: P) -> Option<PathBuf> {
        let gitconfig = dir.as_ref().join(".gitconfig");
        if gitconfig.exists() {
            return Some(gitconfig);
        }
        let gitconfig = dir.as_ref().join(".config").join("git").join("config");
        if gitconfig.exists() {
            Some(gitconfig)
        } else {
            None
        }
    }

    /// Try to find the likely path of the system configuration file.
    ///
    /// At present, this function always returns `None`.
    pub fn find_system_file() -> Option<PathBuf> {
        None
    }
}

/// Repository-specific configuration.
pub struct RepoConfig {
    path: PathBuf,
    cf: Ini,
}

impl RepoConfig {
    /// Create a new [`RepoConfig`] object.
    ///
    /// If the path does not exist, a basic default config
    /// will be created in memory, but not saved.  The path's validity
    /// is not checked, so if the path is invalid, this will only be
    /// discovered when calls to [`Self::save()`] fail.
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        let pb = path.as_ref().to_path_buf();
        let cf = if pb.exists() {
            load_ini_safe(Some(&pb))
        } else {
            Self::default_config()
        };
        Self { path: pb, cf }
    }

    /// Save the config.
    ///
    /// # Errors
    ///
    /// This method errors if the config's path (passed in to the [`Self::new()`]) method) is invalid,
    /// or if other errors occur when writing to the filesystem.
    pub fn save(&self) -> Result<(), anyhow::Error> {
        self.cf
            .write_to_file(&self.path)
            .with_context(|| "failed to write config")
    }

    /// Get the `core.repositoryformatversion` setting.
    ///
    /// # Errors
    ///
    /// This method errors if the key `core.repositoryformatversion` is not present, or if it is not set to a valid `u32` value.
    pub fn version(&self) -> Result<u32, anyhow::Error> {
        let unparsed_version = get_setting_from_ini(&self.cf, "core", "repositoryformatversion");
        if unparsed_version.is_empty() {
            return Err(anyhow!("repository version not set"));
        }
        u32::from_str(&unparsed_version[0]).with_context(|| "version is not a number")
    }

    /// List the names of remotes
    ///
    /// This method iterates though all of the config sections named something
    /// like `[remote "<name>"] and returns the `<name>` part of each.
    pub fn remote_names(&self) -> Vec<&str> {
        self.cf
            .sections()
            .filter_map(|x| {
                x.map(|y| {
                    if y.starts_with("remote") {
                        Some(&y[8..(y.len() - 1)])
                    } else {
                        None
                    }
                })
            })
            .flatten()
            .collect::<Vec<&str>>()
    }

    /// Get the details of a named remote, if it exists in the config.
    ///
    /// Returns `None` if the remote `name` does not exist.
    ///
    /// If the remote is configured with fetch URLs but no push URLs, the `push_urls`
    /// property will be a clone of the contents of the `fetch_urls` property.
    pub fn remote_info<'a>(&'a self, name: &'a str) -> Option<RemoteInfo<'a>> {
        let section = self.cf.section(Some(format!("remote \"{name}\"")))?;
        let fetch_urls = get_str_setting_from_ini_section(section, "url");
        let push_urls = get_str_setting_from_ini_section(section, "pushurl");
        let push_urls = if push_urls.is_empty() {
            fetch_urls.clone()
        } else {
            push_urls
        };
        Some(RemoteInfo {
            name: name,
            fetch_urls,
            push_urls,
        })
    }

    fn default_config() -> Ini {
        let mut conf = Ini::new();
        conf.with_section(Some("core"))
            .set("repositoryformatversion", "0")
            .set("filemode", "false")
            .set("bare", "false");
        conf
    }
}

/// The details of a remote repository.
pub struct RemoteInfo<'a> {
    /// The name by which the remote is referred to on the command line or in ref paths.
    pub name: &'a str,

    /// The list of URLs that can be fetched from.
    pub fetch_urls: Vec<&'a str>,

    /// The list of URLs that can be pushed to.
    pub push_urls: Vec<&'a str>,
}

fn load_ini_safe<T: AsRef<Path>>(path: Option<T>) -> Ini {
    path.and_then(|p| Ini::load_from_file(p).ok())
        .unwrap_or_default()
}

fn get_setting_from_ini(ini: &Ini, section: &str, key: &str) -> Vec<String> {
    if let Some(sec) = ini.section(Some(section)) {
        get_setting_from_ini_section(sec, key)
    } else {
        Vec::<String>::new()
    }
}

fn get_setting_from_ini_section(section: &Properties, key: &str) -> Vec<String> {
    section
        .get_all(key)
        .map(|v| v.trim().to_string())
        .collect::<Vec<String>>()
}

fn get_str_setting_from_ini_section<'a>(section: &'a Properties, key: &str) -> Vec<&'a str> {
    section
        .get_all(key)
        .map(|v| v.trim())
        .collect::<Vec<&str>>()
}

fn get_setting_from_env<T: AsRef<OsStr>>(key: T) -> Option<String> {
    env::var(key).ok()
}
