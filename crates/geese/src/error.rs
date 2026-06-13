use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to resolve a data directory for geese")]
    NoDataDir,
    #[error("invalid profile name: {0}")]
    InvalidName(String),
    #[error("profile already exists: {0}")]
    ProfileExists(String),
    #[error("profile not found: {0}")]
    ProfileNotFound(String),
    #[error("profile is locked: {0}")]
    ProfileLocked(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    TomlDe(#[from] toml::de::Error),
    #[error(transparent)]
    TomlSer(#[from] toml::ser::Error),
}
