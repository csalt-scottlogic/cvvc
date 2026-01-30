use ini::Ini;
use std::{
    env,
    ffi::OsStr,
    fmt::Debug,
    path::{Path, PathBuf},
    str::FromStr,
};

pub struct GlobalConfig {
    system_config: Ini,
    user_config: Ini,
}

impl GlobalConfig {
    pub fn from_files<T: AsRef<Path> + Debug>(system_path: Option<T>, user_path: Option<T>) -> Self {
        println!("System file: {system_path:?}");
        println!("User file: {user_path:?}");
        GlobalConfig {
            system_config: load_ini_safe(system_path),
            user_config: load_ini_safe(user_path),
        }
    }

    pub fn from_default_files() -> Self {
        Self::from_files(Self::find_system_file(), Self::find_user_file())
    }

    pub fn get_setting_all(&self, section: &str, key: &str) -> Vec<String> {
        let mut vals = get_setting_from_ini(&self.system_config, section, key);
        vals.append(&mut get_setting_from_ini(&self.user_config, section, key));
        vals
    }

    pub fn get_setting(&self, section: &str, key: &str) -> Option<String> {
        let vals = self.get_setting_all(section, key);
        if vals.len() > 0 {
            Some(vals.last().unwrap().to_owned())
        } else {
            None
        }
    }

    pub fn user_name(&self) -> Option<String> {
        self.get_setting("user", "name")
    }

    pub fn user_email(&self) -> Option<String> {
        self.get_setting("user", "email")
    }

    pub fn author_name(&self) -> Option<String> {
        self.get_setting("author", "name")
    }

    pub fn author_email(&self) -> Option<String> {
        self.get_setting("author", "email")
    }

    pub fn committer_name(&self) -> Option<String> {
        self.get_setting("committer", "email")
    }

    pub fn committer_email(&self) -> Option<String> {
        self.get_setting("committer", "email")
    }

    pub fn author(&self) -> String {
        let author_name = get_setting_from_env("GIT_AUTHOR_NAME")
            .or_else(|| self.author_name())
            .or_else(|| self.user_name())
            .or_else(|| whoami::username().ok())
            .unwrap_or_else(|| "<unknown>".to_string());
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

    pub fn find_user_file() -> Option<PathBuf> {
        env::var("XDG_CONFIG_HOME")
            .ok()
            .and_then(|d| PathBuf::from_str(&d).ok())
            .and_then(|d| Some(d.join("git").join("config")))
            .or_else(|| env::home_dir().and_then(|hd| Some(hd.join(".config").join("git").join("config"))))
    }

    pub fn find_system_file() -> Option<PathBuf> {
        None
    }
}

pub fn default_repo_config() -> Ini {
    let mut conf = Ini::new();
    conf.with_section(Some("core"))
        .set("repositoryformatversion", "0")
        .set("filemode", "false")
        .set("bare", "false");
    conf
}

fn load_ini_safe<T: AsRef<Path>>(path: Option<T>) -> Ini {
    path.and_then(|p| Ini::load_from_file(p).ok())
        .unwrap_or_else(|| Ini::new())
}

fn get_setting_from_ini(ini: &Ini, section: &str, key: &str) -> Vec<String> {
    if let Some(sec) = ini.section(Some(section)) {
        sec.get_all(key)
            .map(|v| v.trim().to_string())
            .collect::<Vec<String>>()
    } else {
        Vec::<String>::new()
    }
}

fn get_setting_from_env<T: AsRef<OsStr>>(key: T) -> Option<String> {
    match env::var(key) {
        Ok(v) => Some(v),
        Err(_) => None,
    }
}
