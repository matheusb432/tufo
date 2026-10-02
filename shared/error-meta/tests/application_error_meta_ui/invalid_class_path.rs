#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

struct Policy;
impl Policy {
    const CLASS: u8 = 1;
}

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = Policy::CLASS, reason = "wrong")]
#[error("wrong type")]
struct WrongType;

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = crate::application_support::error::ErrorClass::Typo, reason = "missing")]
#[error("missing variant")]
struct MissingVariant;

fn main() {}
