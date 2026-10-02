#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
enum Missing {
    First,
    Second(u8),
}

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
struct MissingStruct;

fn main() {}
