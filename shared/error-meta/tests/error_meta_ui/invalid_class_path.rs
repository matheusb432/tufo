#[path = "../support/mod.rs"]
mod support;

use error_meta::ErrorMeta;

struct Policy;
impl Policy {
    const CLASS: u8 = 1;
}

#[derive(ErrorMeta)]
#[meta(crate = crate::support)]
#[meta(private(Policy::CLASS))]
struct WrongType;

#[derive(ErrorMeta)]
#[meta(crate = crate::support)]
#[meta(private(crate::support::failure::ErrorClass::Typo))]
struct MissingVariant;

fn main() {}
