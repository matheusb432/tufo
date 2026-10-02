pub mod error {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ErrorClass {
        InvalidArgument,
        NotFound,
        FailedPrecondition,
        Unavailable,
        Internal,
        Conflict,
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
