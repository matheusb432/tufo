#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = Internal, reason = "enum")]
enum Misplaced {
    #[meta(class = Internal, reason = "field")]
    Field(#[meta(transparent)] u8),
    #[meta(crate = crate::application_support)]
    Facade,
}

fn main() {}
