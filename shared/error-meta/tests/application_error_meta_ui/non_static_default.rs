#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = Internal, reason = self.reason())]
enum BorrowedReason {
    #[error("{0}")]
    Owned(String),
}

impl BorrowedReason {
    fn reason(&self) -> &str {
        let Self::Owned(reason) = self;
        reason
    }
}

fn main() {}
