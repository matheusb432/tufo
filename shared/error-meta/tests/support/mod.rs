pub mod failure {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ErrorClass {
        Internal,
        Unavailable,
        ResourceExhausted,
        Throttled,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Classification {
        Public(Failure),
        Private(ErrorClass),
    }

    pub trait Classified {
        fn classify(&self) -> Classification;
    }

    pub trait PublicFailure {
        fn failure(&self) -> Failure;
    }

    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum Failure {
        #[error("state changed")]
        Changed,
        #[error("resource gone: {resource:?}")]
        Gone { resource: Resource },
        #[error("invalid field: {field}")]
        InvalidRequest { field: String },
        #[error(transparent)]
        Push(PushFailure),
    }

    impl PublicFailure for Failure {
        fn failure(&self) -> Failure {
            self.clone()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Resource {
        PushOperation,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
    pub enum PushFailure {
        #[error("detached")]
        Detached,
        #[error("review expired")]
        ReviewExpired,
    }

    impl PublicFailure for PushFailure {
        fn failure(&self) -> Failure {
            Failure::Push(*self)
        }
    }

    impl From<PushFailure> for Failure {
        fn from(failure: PushFailure) -> Self {
            Self::Push(failure)
        }
    }
}
