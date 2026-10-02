#[path = "../application_support/mod.rs"]
mod application_support;

use error_meta::ApplicationErrorMeta;

#[derive(Debug, thiserror::Error, ApplicationErrorMeta)]
#[meta(crate = crate::application_support)]
#[meta(class = Internal, reason = self.reason.as_str())]
#[error("dynamic")]
struct Dynamic { reason: String }

fn main() {}
