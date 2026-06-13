use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProfileMeta {
    pub name: String,
    pub locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Profile {
    path: PathBuf,
    meta: ProfileMeta,
}

impl Profile {
    pub(crate) fn new(path: PathBuf, meta: ProfileMeta) -> Self {
        Self { path, meta }
    }

    pub fn name(&self) -> &str {
        &self.meta.name
    }

    pub fn meta(&self) -> &ProfileMeta {
        &self.meta
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn lock(&mut self) -> Result<()> {
        self.meta.locked = true;
        self.write_meta()
    }

    pub fn unlock(&mut self) -> Result<()> {
        self.meta.locked = false;
        self.write_meta()
    }

    pub fn command<S>(&self, program: S) -> Command
    where
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(program);
        command.env("GOOSE_PATH_ROOT", &self.path);
        command
    }

    fn write_meta(&self) -> Result<()> {
        let contents = toml::to_string(&self.meta)?;
        fs::write(self.path.join("profile.toml"), contents)?;
        Ok(())
    }
}
