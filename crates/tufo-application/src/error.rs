//! Metadata owned by application failures, independent of transport and presentation.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    InvalidArgument,
    NotFound,
    FailedPrecondition,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorMetadata {
    pub class: ErrorClass,
    pub reason: &'static str,
}

/// Transparent errors forward the wrapped failure's metadata unchanged.
pub trait ApplicationError: std::error::Error {
    fn metadata(&self) -> ErrorMetadata;
}
