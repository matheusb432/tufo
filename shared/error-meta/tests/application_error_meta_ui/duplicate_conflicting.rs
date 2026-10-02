#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(crate = crate::application_support)]
enum Conflicting {
    #[meta(class = Internal, reason = "first")]
    #[meta(transparent)]
    Attributes(u8),
    #[meta(class = Internal, class = NotFound, reason = "duplicate")]
    Class,
    #[meta(reason = "one", reason = "two", class = Internal)]
    Reason,
    #[meta(transparent, class = Internal, reason = "conflict")]
    TransparentFirst(u8),
    #[meta(class = Internal, reason = "conflict", transparent)]
    FixedFirst(u8),
    #[meta(transparent, transparent)]
    DuplicateTransparent(u8),
}

fn main() {}
