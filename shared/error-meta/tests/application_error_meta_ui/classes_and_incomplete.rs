#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
enum Invalid {
    #[meta(class = Cancelled, reason = "wrong vocabulary")]
    Unknown,
    #[meta(class = Internal)]
    MissingReason,
    #[meta(reason = "missing class")]
    MissingClass,
    #[meta()]
    Empty,
    #[meta(other)]
    UnknownAttribute,
    #[meta(private(Internal))]
    WrongContract,
}

fn main() {}
