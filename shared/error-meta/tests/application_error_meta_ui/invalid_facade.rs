#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(crate = "invalid")]
#[meta(class = Internal, reason = "bad")]
struct InvalidFacade;

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support, class = Internal)]
#[meta(class = Internal, reason = "mixed")]
struct Mixed;

fn main() {}
