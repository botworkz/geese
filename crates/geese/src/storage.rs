use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
    config::GlobalConfig,
    error::{Error, Result},
    profile::{Profile, ProfileMeta},
};

#[derive(Clone, Debug)]
pub struct Storage {
    root: PathBuf,
}

impl Storage {
    pub fn from_env() -> Result<Self> {
        match env::var_os("GEESE_ROOT") {
            Some(path) => Ok(Self::at(path.into())),
            None => {
                let data_dir = dirs::data_dir().ok_or(Error::NoDataDir)?;
                Ok(Self::at(data_dir.join("geese")))
            }
        }
    }

    pub fn at(path: PathBuf) -> Self {
        Self {
            root: absolutize(path),
        }
    }

    pub fn list(&self) -> Result<Vec<ProfileMeta>> {
        let profiles_dir = self.profiles_dir();
        if !profiles_dir.exists() {
            return Ok(Vec::new());
        }

        let mut profiles = Vec::new();
        for entry in fs::read_dir(profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let meta_path = path.join("profile.toml");
            if !meta_path.is_file() {
                continue;
            }

            profiles.push(self.read_meta(&meta_path)?);
        }

        profiles.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(profiles)
    }

    /// Like [`list`] but also returns each profile's on-disk path, in a
    /// single fs-read pass.
    ///
    /// [`list`] returns metadata only; callers who need the path normally
    /// follow up with [`get`] per entry, paying a second `profile.toml`
    /// read each. This variant returns `(ProfileMeta, PathBuf)` tuples
    /// from the same directory walk so listing N profiles costs N reads
    /// rather than 2N.
    pub fn list_full(&self) -> Result<Vec<(ProfileMeta, PathBuf)>> {
        let profiles_dir = self.profiles_dir();
        if !profiles_dir.exists() {
            return Ok(Vec::new());
        }

        let mut profiles = Vec::new();
        for entry in fs::read_dir(profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let meta_path = path.join("profile.toml");
            if !meta_path.is_file() {
                continue;
            }

            let meta = self.read_meta(&meta_path)?;
            profiles.push((meta, path));
        }

        profiles.sort_by(|left, right| left.0.name.cmp(&right.0.name));
        Ok(profiles)
    }

    pub fn get(&self, name: &str) -> Result<Profile> {
        validate_name(name)?;
        let profile_dir = self.profile_dir(name);
        let meta_path = profile_dir.join("profile.toml");
        if !meta_path.is_file() {
            return Err(Error::ProfileNotFound(name.to_owned()));
        }

        Ok(Profile::new(profile_dir, self.read_meta(&meta_path)?))
    }

    pub fn create(&self, name: &str) -> Result<Profile> {
        validate_name(name)?;
        let profile_dir = self.profile_dir(name);
        if profile_dir.exists() {
            return Err(Error::ProfileExists(name.to_owned()));
        }

        fs::create_dir_all(profile_dir.join("config"))?;
        fs::create_dir_all(profile_dir.join("data"))?;
        fs::create_dir_all(profile_dir.join("state"))?;

        let meta = ProfileMeta {
            name: name.to_owned(),
            locked: false,
            parent: None,
            cwd: None,
        };
        self.write_meta(&profile_dir, &meta)?;

        Ok(Profile::new(profile_dir, meta))
    }

    pub fn copy(&self, src: &str, dest: &str) -> Result<Profile> {
        let source = self.get(src)?;
        validate_name(dest)?;

        let dest_dir = self.profile_dir(dest);
        if dest_dir.exists() {
            return Err(Error::ProfileExists(dest.to_owned()));
        }

        fs::create_dir_all(dest_dir.join("config"))?;
        fs::create_dir_all(dest_dir.join("data"))?;
        fs::create_dir_all(dest_dir.join("state"))?;

        let source_config = source.path().join("config").join("config.yaml");
        let dest_config = dest_dir.join("config").join("config.yaml");
        if source_config.is_file() {
            fs::copy(source_config, dest_config)?;
        }

        let meta = ProfileMeta {
            name: dest.to_owned(),
            locked: false,
            parent: Some(source.name().to_owned()),
            cwd: None,
        };
        self.write_meta(&dest_dir, &meta)?;

        Ok(Profile::new(dest_dir, meta))
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let profile = self.get(name)?;
        if profile.meta().locked {
            return Err(Error::ProfileLocked(name.to_owned()));
        }

        fs::remove_dir_all(profile.path())?;
        Ok(())
    }

    /// Set the per-profile `cwd` field and persist to `profile.toml`.
    pub fn set_profile_cwd(&self, name: &str, cwd: PathBuf) -> Result<()> {
        let profile = self.get(name)?;
        let mut meta = profile.meta().clone();
        meta.cwd = Some(cwd);
        self.write_meta(profile.path(), &meta)
    }

    /// Clear the per-profile `cwd` field and persist to `profile.toml`.
    pub fn unset_profile_cwd(&self, name: &str) -> Result<()> {
        let profile = self.get(name)?;
        let mut meta = profile.meta().clone();
        meta.cwd = None;
        self.write_meta(profile.path(), &meta)
    }

    /// Load the global geese config from `$XDG_CONFIG_HOME/geese/config.toml`.
    pub fn load_global_config(&self) -> Result<GlobalConfig> {
        GlobalConfig::load()
    }

    /// Persist the global geese config to `$XDG_CONFIG_HOME/geese/config.toml`.
    pub fn save_global_config(&self, config: &GlobalConfig) -> Result<()> {
        config.save()
    }

    /// Resolve the effective working directory for `name` by walking:
    ///
    /// 1. Per-profile `cwd` in `profile.toml`
    /// 2. `GEESE_PROFILE_CWD_<NAME>` env var (name uppercased, `-` → `_`)
    /// 3. Global `cwd` in `~/.config/geese/config.toml`
    /// 4. `GEESE_CWD` env var
    /// 5. `dirs::home_dir()`, or `/` as last resort
    pub fn resolve_cwd(&self, name: &str) -> PathBuf {
        // 1. Per-profile field
        if let Ok(profile) = self.get(name)
            && let Some(cwd) = profile.meta().cwd.clone()
        {
            return cwd;
        }

        // 2. Per-profile env var: GEESE_PROFILE_CWD_<NAME>
        //    Uppercased, hyphens replaced with underscores
        let env_key = format!(
            "GEESE_PROFILE_CWD_{}",
            name.to_uppercase().replace('-', "_")
        );
        if let Some(val) = env::var_os(&env_key) {
            return PathBuf::from(val);
        }

        // 3. Global config cwd
        if let Ok(config) = GlobalConfig::load()
            && let Some(cwd) = config.cwd
        {
            return cwd;
        }

        // 4. Global env var
        if let Some(val) = env::var_os("GEESE_CWD") {
            return PathBuf::from(val);
        }

        // 5. Home directory, falling back to /
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }

    fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    fn profile_dir(&self, name: &str) -> PathBuf {
        self.profiles_dir().join(name)
    }

    fn read_meta(&self, meta_path: &Path) -> Result<ProfileMeta> {
        let contents = fs::read_to_string(meta_path)?;
        Ok(toml::from_str(&contents)?)
    }

    fn write_meta(&self, profile_dir: &Path, meta: &ProfileMeta) -> Result<()> {
        fs::create_dir_all(self.profiles_dir())?;
        fs::write(profile_dir.join("profile.toml"), toml::to_string(meta)?)?;
        Ok(())
    }
}

fn absolutize(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .map(|current_dir| current_dir.join(&path))
            .unwrap_or(path)
    }
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'));

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use std::{env, ffi::OsStr, fs, path::PathBuf, sync::Mutex};

    use tempfile::tempdir;

    use super::Storage;
    use crate::{GlobalConfig, error::Error};

    /// Serialize all tests that touch process-wide environment variables so
    /// they don't interfere with each other when run in parallel.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn manages_profile_lifecycle() {
        let tempdir = tempdir().unwrap();
        let storage = Storage::at(tempdir.path().join("geese-root"));

        assert!(storage.list().unwrap().is_empty());

        let mut profile = storage.create("work-stable").unwrap();
        assert_eq!(profile.name(), "work-stable");
        assert!(!profile.meta().locked);
        assert!(profile.path().is_absolute());
        assert!(profile.path().join("config").is_dir());
        assert!(profile.path().join("data").is_dir());
        assert!(profile.path().join("state").is_dir());

        profile.lock().unwrap();
        assert!(storage.get("work-stable").unwrap().meta().locked);
        assert!(
            matches!(storage.delete("work-stable"), Err(Error::ProfileLocked(name)) if name == "work-stable")
        );

        profile.unlock().unwrap();
        storage.delete("work-stable").unwrap();
        assert!(storage.list().unwrap().is_empty());
    }

    #[test]
    fn rejects_invalid_names_and_duplicate_profiles() {
        let tempdir = tempdir().unwrap();
        let storage = Storage::at(tempdir.path().join("geese-root"));

        assert!(matches!(
            storage.create("bad.name"),
            Err(Error::InvalidName(name)) if name == "bad.name"
        ));

        storage.create("source").unwrap();
        assert!(matches!(
            storage.create("source"),
            Err(Error::ProfileExists(name)) if name == "source"
        ));
    }

    #[test]
    fn absolutizes_relative_roots_and_preserves_absolute_roots() {
        let current_dir = env::current_dir().unwrap();

        let relative = Storage::at(PathBuf::from("relative-geese-root"));
        let profile = relative.create("source").unwrap();
        assert_eq!(
            profile.path(),
            current_dir
                .join("relative-geese-root")
                .join("profiles")
                .join("source")
        );
        fs::remove_dir_all(current_dir.join("relative-geese-root")).unwrap();

        let tempdir = tempdir().unwrap();
        let absolute_root = tempdir.path().join("absolute-geese-root");
        let absolute = Storage::at(absolute_root.clone());
        let absolute_profile = absolute.create("target").unwrap();
        assert_eq!(
            absolute_profile.path(),
            absolute_root.join("profiles").join("target")
        );
    }

    #[test]
    fn copies_only_config_yaml_and_prepares_goose_command() {
        let tempdir = tempdir().unwrap();
        let storage = Storage::at(tempdir.path().join("geese-root"));

        let source = storage.create("source").unwrap();
        fs::write(
            source.path().join("config").join("config.yaml"),
            "model = \"gpt\"\n",
        )
        .unwrap();
        fs::write(
            source.path().join("config").join("other.toml"),
            "ignored = true\n",
        )
        .unwrap();
        fs::write(
            source.path().join("data").join("session.txt"),
            "ignore me\n",
        )
        .unwrap();

        let target = storage.copy("source", "target").unwrap();
        assert_eq!(target.meta().parent.as_deref(), Some("source"));
        assert!(!target.meta().locked);
        assert_eq!(
            fs::read_to_string(target.path().join("config").join("config.yaml")).unwrap(),
            "model = \"gpt\"\n"
        );
        assert!(!target.path().join("config").join("other.toml").exists());
        assert!(!target.path().join("data").join("session.txt").exists());

        let command = target.command("goose");
        let goose_path_root = command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("GOOSE_PATH_ROOT"))
            .and_then(|(_, value)| value);
        assert_eq!(goose_path_root, Some(target.path().as_os_str()));
    }

    /// resolve_cwd tier 1: per-profile field wins over everything else.
    #[test]
    fn resolve_cwd_tier1_per_profile_field() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();
        let config_dir = tempdir.path().join("config");
        let storage = Storage::at(tempdir.path().join("geese-root"));
        // Isolate XDG and home-dir env vars
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &config_dir);
            env::remove_var("GEESE_CWD");
            env::remove_var("GEESE_PROFILE_CWD_WORK");
        }

        storage.create("work").unwrap();
        let profile_cwd = tempdir.path().join("profile-cwd");
        storage
            .set_profile_cwd("work", profile_cwd.clone())
            .unwrap();

        // Write a global config pointing elsewhere
        GlobalConfig {
            cwd: Some(tempdir.path().join("global-cwd")),
        }
        .save()
        .unwrap();

        assert_eq!(storage.resolve_cwd("work"), profile_cwd);
    }

    /// resolve_cwd tier 2: per-profile env var.
    #[test]
    fn resolve_cwd_tier2_per_profile_env_var() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();
        let config_dir = tempdir.path().join("config");
        let storage = Storage::at(tempdir.path().join("geese-root"));
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &config_dir);
            env::remove_var("GEESE_CWD");
        }

        storage.create("work").unwrap();
        // No per-profile field set
        let env_cwd = tempdir.path().join("env-cwd");
        unsafe { env::set_var("GEESE_PROFILE_CWD_WORK", &env_cwd) };

        // Global config points elsewhere — should be ignored
        GlobalConfig {
            cwd: Some(tempdir.path().join("global-cwd")),
        }
        .save()
        .unwrap();

        let resolved = storage.resolve_cwd("work");
        unsafe { env::remove_var("GEESE_PROFILE_CWD_WORK") };
        assert_eq!(resolved, env_cwd);
    }

    /// resolve_cwd tier 3: global config cwd.
    #[test]
    fn resolve_cwd_tier3_global_config() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();
        let config_dir = tempdir.path().join("config");
        let storage = Storage::at(tempdir.path().join("geese-root"));
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &config_dir);
            env::remove_var("GEESE_CWD");
            env::remove_var("GEESE_PROFILE_CWD_WORK");
        }

        storage.create("work").unwrap();
        // No per-profile field, no per-profile env var

        let global_cwd = tempdir.path().join("global-cwd");
        GlobalConfig {
            cwd: Some(global_cwd.clone()),
        }
        .save()
        .unwrap();

        assert_eq!(storage.resolve_cwd("work"), global_cwd);
    }

    /// resolve_cwd tier 4: GEESE_CWD env var.
    #[test]
    fn resolve_cwd_tier4_global_env_var() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();
        let config_dir = tempdir.path().join("config");
        let storage = Storage::at(tempdir.path().join("geese-root"));
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &config_dir);
            env::remove_var("GEESE_PROFILE_CWD_WORK");
        }

        storage.create("work").unwrap();
        // No per-profile field, no per-profile env var, no global config

        let global_env_cwd = tempdir.path().join("geese-cwd");
        unsafe { env::set_var("GEESE_CWD", &global_env_cwd) };

        let resolved = storage.resolve_cwd("work");
        unsafe { env::remove_var("GEESE_CWD") };
        assert_eq!(resolved, global_env_cwd);
    }

    /// resolve_cwd tier 5: home directory fallback.
    #[test]
    fn resolve_cwd_tier5_home_dir_fallback() {
        let _env_guard = ENV_LOCK.lock().unwrap();
        let tempdir = tempdir().unwrap();
        let config_dir = tempdir.path().join("config");
        let storage = Storage::at(tempdir.path().join("geese-root"));
        unsafe {
            env::set_var("XDG_CONFIG_HOME", &config_dir);
            env::remove_var("GEESE_CWD");
            env::remove_var("GEESE_PROFILE_CWD_WORK");
        }

        storage.create("work").unwrap();
        // Nothing set — should fall back to home dir

        let resolved = storage.resolve_cwd("work");
        let expected = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        assert_eq!(resolved, expected);
    }

    /// set/unset per-profile cwd round-trips through profile.toml.
    #[test]
    fn set_and_unset_profile_cwd() {
        let tempdir = tempdir().unwrap();
        let storage = Storage::at(tempdir.path().join("geese-root"));

        storage.create("test").unwrap();
        assert!(storage.get("test").unwrap().meta().cwd.is_none());

        let cwd = tempdir.path().join("mydir");
        storage.set_profile_cwd("test", cwd.clone()).unwrap();
        assert_eq!(
            storage.get("test").unwrap().meta().cwd.as_deref(),
            Some(cwd.as_path())
        );

        storage.unset_profile_cwd("test").unwrap();
        assert!(storage.get("test").unwrap().meta().cwd.is_none());
    }
}
