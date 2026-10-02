use error_meta::ErrorMeta;
use local::failure::{Classification, Classified, ErrorClass, Failure};

mod local {
    pub mod failure {
        #[derive(Debug, Clone, Copy)]
        pub enum ErrorClass {
            Internal,
            Overloaded,
        }

        #[derive(Debug, Clone, thiserror::Error)]
        pub enum Failure {
            #[error("invalid field: {field}")]
            InvalidRequest { field: String },
        }

        #[derive(Debug)]
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

        impl PublicFailure for Failure {
            fn failure(&self) -> Failure {
                self.clone()
            }
        }
    }
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
#[meta(crate = local)]
#[meta(private(Internal))]
enum OperationError {
    #[error("storage failed: {0}")]
    Storage(#[from] std::io::Error),
    #[error(transparent)]
    #[meta(failure)]
    Refused(#[from] Failure),
    #[error("too many operations")]
    #[meta(private(ErrorClass::Overloaded))]
    Overloaded,
}

#[derive(Debug, thiserror::Error, ErrorMeta)]
#[meta(crate = local)]
#[meta(transparent(field = source))]
#[error("{operation}: {source}")]
struct OperationContext {
    operation: &'static str,
    source: OperationError,
}

fn main() {
    for source in [
        OperationError::Storage(std::io::Error::other("disk error")),
        Failure::InvalidRequest {
            field: "name".into(),
        }
        .into(),
        OperationError::Overloaded,
    ] {
        let error = OperationContext {
            operation: "save",
            source,
        };
        match error.classify() {
            Classification::Public(failure) => println!("public: {failure}"),
            Classification::Private(class) => println!("private: {class:?}; diagnostic: {error}"),
        }
    }
}
