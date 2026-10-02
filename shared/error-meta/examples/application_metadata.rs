use std::path::PathBuf;

use error_meta::ApplicationErrorMeta;
use local::error::{ApplicationError, ErrorClass};

mod local {
    pub mod error {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ErrorClass {
            Internal,
            NotFound,
            Overloaded,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct ErrorMetadata {
            pub class: ErrorClass,
            pub reason: &'static str,
        }

        pub trait ApplicationError: std::error::Error {
            fn metadata(&self) -> ErrorMetadata;
        }
    }
}

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = local)]
#[meta(class = Internal)]
enum ReadError {
    #[error("file does not exist")]
    #[meta(class = NotFound, reason = "file.missing")]
    Missing,
    #[error("cannot read file: {0}")]
    #[meta(reason = "file.unreadable")]
    Unreadable(#[from] std::io::Error),
    #[error("too many reads")]
    #[meta(class = ErrorClass::Overloaded, reason = "file.overloaded")]
    Overloaded,
}

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = local)]
#[meta(transparent(field = source))]
#[error("{path}: {source}", path = path.display())]
struct ReadContext {
    path: PathBuf,
    source: ReadError,
}

fn main() {
    for source in [
        ReadError::Missing,
        std::io::Error::other("disk error").into(),
        ReadError::Overloaded,
    ] {
        let error = ReadContext {
            path: PathBuf::from("settings.toml"),
            source,
        };
        println!("{error}: {:?}", error.metadata());
    }
}
