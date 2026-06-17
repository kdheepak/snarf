use std::fmt;

use color_eyre::eyre;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug)]
pub enum AppError {
    Validation(String),
    NotFound(String),
    Upstream(String),
    Precondition(String),
    RegexUnsupported,
}

impl AppError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound(message.into())
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::Upstream(message.into())
    }

    pub fn precondition(message: impl Into<String>) -> Self {
        Self::Precondition(message.into())
    }

    pub fn with_upstream_context(self, context: &str) -> Self {
        match self {
            Self::Upstream(message) => Self::Upstream(format!("{context}: {message}")),
            other => other,
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message)
            | Self::NotFound(message)
            | Self::Upstream(message)
            | Self::Precondition(message) => f.write_str(message),
            Self::RegexUnsupported => f.write_str("regex unsupported"),
        }
    }
}

impl std::error::Error for AppError {}

impl From<eyre::Report> for AppError {
    fn from(error: eyre::Report) -> Self {
        Self::upstream(error.to_string())
    }
}
