use std::{
    env, fs,
    path::{Path, PathBuf},
};

use crate::{
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
    use std::{env, ffi::OsStr, fs, path::PathBuf};

    use tempfile::tempdir;

    use super::Storage;
    use crate::error::Error;

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
}
