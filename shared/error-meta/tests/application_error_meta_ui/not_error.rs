#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = Internal, reason = "not an error")]
struct NotAnError;

fn main() {}
