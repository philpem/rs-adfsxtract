use thiserror::Error;

#[derive(Debug, Error)]
pub enum FcError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("checksum mismatch: {0}")]
    Checksum(String),
    #[error("broken directory: {0}")]
    BrokenDirectory(String),
    #[error("unsupported format: {0}")]
    Unsupported(String),
    #[error("not a recognised disc image")]
    NotRecognised,
}

pub type Result<T> = std::result::Result<T, FcError>;
