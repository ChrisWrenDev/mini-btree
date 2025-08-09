use thiserror::Error;

#[derive(Debug, Error)]
pub enum CustomError {
    #[error("Not support: {0}")]
    NotSupport(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type CustomResult<T, E = CustomError> = Result<T, E>;
