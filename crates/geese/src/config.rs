use std::{env, fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Global geese configuration, loaded from `$XDG_CONFIG_HOME/geese/config.toml`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct GlobalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
}

impl GlobalConfig {
    /// Load from the standard location, returning a default config if the file
    /// does not exist.
    pub fn load() -> Result<Self> {
        let path = global_config_path()?;
        if !path.is_file() {
            return Ok(GlobalConfig::default());
        }

        let contents = fs::read_to_string(&path)?;
        Ok(toml::from_str(&contents)?)
    }

    /// Persist to the standard location, creating parent directories as needed.
    pub fn save(&self) -> Result<()> {
        let path = global_config_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, toml::to_string(self)?)?;
        Ok(())
    }
}

/// Returns the path to the global config file:
/// `$XDG_CONFIG_HOME/geese/config.toml`, falling back to
/// `$HOME/.config/geese/config.toml`.
pub fn global_config_path() -> Result<PathBuf> {
    let config_home = match env::var_os("XDG_CONFIG_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => {
            let home = dirs::home_dir().ok_or(Error::NoDataDir)?;
            home.join(".config")
        }
    };

    Ok(config_home.join("geese").join("config.toml"))
}
