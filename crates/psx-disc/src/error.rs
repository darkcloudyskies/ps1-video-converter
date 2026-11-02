use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("CUE sheet {path}, line {line}: {msg}")]
    Cue {
        path: PathBuf,
        line: usize,
        msg: String,
    },
    #[error("unsupported disc image: {0}")]
    Unsupported(String),
    #[error("sector {0} is out of range")]
    OutOfRange(u32),
    #[error("ISO9660: {0}")]
    Iso(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Error {
    let path = path.into();
    move |source| Error::Io { path, source }
}
