#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
enum Selectors {
    #[meta(transparent)]
    Empty,
    #[meta(transparent)]
    Multiple(u8, u8),
    #[meta(transparent(field = absent))]
    Missing { source: u8 },
    #[meta(transparent(field = 2))]
    OutOfRange(u8),
    #[meta(transparent(field = source))]
    WrongKind(u8),
    #[meta(transparent(field = 0))]
    Named { source: u8 },
    #[meta(transparent())]
    NoSelection(u8),
    #[meta(transparent(field = 0, field = 1))]
    Duplicated(u8, u8),
}

#[derive(ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(transparent(field = absent))]
struct StructSelection { source: u8 }

fn main() {}
