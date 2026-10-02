#[cfg(feature = "crate-discovery")]
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream};
use syn::{Attribute, Error, Ident, Path, Result, ext::IdentExt};

pub(crate) fn is_override(attribute: &Attribute) -> bool {
    attribute
        .parse_args_with(|input: syn::parse::ParseStream<'_>| {
            let key = input.call(Ident::parse_any)?;
            let _: TokenStream = input.parse()?;
            Ok(key == "crate")
        })
        .unwrap_or(false)
}

pub(crate) fn resolve(path_override: Option<Path>, span: Span) -> Result<Path> {
    resolve_for(path_override, span, "gtl-models", "gtl_models")
}

pub(crate) fn resolve_for(
    path_override: Option<Path>,
    span: Span,
    package: &str,
    self_alias: &str,
) -> Result<Path> {
    if let Some(path) = path_override {
        return Ok(path);
    }

    resolve_default(span, package, self_alias)
}

#[cfg(feature = "crate-discovery")]
fn resolve_default(span: Span, package: &str, self_alias: &str) -> Result<Path> {
    let name = match crate_name(package).map_err(|error| {
        Error::new(
            span,
            format!("cannot resolve {package}: {error}; specify #[meta(crate = path)]"),
        )
    })? {
        // The facade aliases itself so this path also works in its integration tests.
        FoundCrate::Itself => self_alias.to_owned(),
        FoundCrate::Name(name) => name,
    };
    syn::parse_str(&format!("::{name}")).or_else(|_| syn::parse_str(&format!("::r#{name}")))
}

#[cfg(not(feature = "crate-discovery"))]
fn resolve_default(span: Span, _package: &str, _self_alias: &str) -> Result<Path> {
    Err(Error::new(
        span,
        "automatic facade discovery is disabled; specify #[meta(crate = path)] or enable the `crate-discovery` feature",
    ))
}
