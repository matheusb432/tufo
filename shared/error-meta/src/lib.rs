#![doc = include_str!("../README.md")]

mod application_error_meta;
mod error_class;
mod error_meta;
mod facade;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Implements a caller-owned `failure::Classified` for enums and structs.
///
/// Structs declare metadata at item level. Enum-level metadata supplies a default;
/// a variant's explicit declaration replaces that default. Without a default,
/// every variant needs its own declaration.
///
/// - `#[meta(failure)]`: the only field is a public reason implementing `failure::PublicFailure`.
/// - `#[meta(failure = expr)]`: evaluates `expr` on every `classify()` call and converts its result
///   with `Into<failure::Failure>`. The expression can use `self` and surrounding names; fields are
///   not bound to local names.
/// - `#[meta(transparent)]`: the only field is itself `Classified`.
/// - `#[meta(private(Class))]`: only the `failure::ErrorClass` crosses the boundary. A qualified
///   path, such as `#[meta(private(crate::ErrorClass::Custom))]`, selects a caller-defined class or
///   associated constant directly; Rust checks its type.
///
/// Delegation can select one field from a variant or struct with other context fields:
/// `#[meta(failure(field = reason))]` or `#[meta(transparent(field = source))]` for named fields,
/// and `#[meta(failure(field = 0))]` or `#[meta(transparent(field = 1))]` for tuple fields.
/// The generated implementation adds `PublicFailure` or `Classified` bounds only for delegated
/// field types, preserving the type's declared bounds and leaving context fields unconstrained.
///
/// The derive resolves the `gtl-models` dependency name, including Cargo aliases. An item-level
/// `#[meta(crate = path)]` overrides the facade path; that path must expose the `failure` module
/// and its `Classified`, `PublicFailure`, `Classification`, `Failure`, and `ErrorClass` contracts.
/// Automatic discovery is enabled by default through the `crate-discovery` feature. Without
/// that feature, every type requires an explicit facade path.
/// Declare the facade override separately from classification metadata. Metadata on fields
/// is rejected.
#[proc_macro_derive(ErrorMeta, attributes(meta))]
pub fn derive_error_meta(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    error_meta::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Implements a caller-owned `error::ApplicationError` for enums and structs.
///
/// Fixed metadata uses `#[meta(class = Class, reason = expression)]`; delegation uses
/// `#[meta(transparent)]`. An enum can supply defaults for class, reason, or both;
/// variants override individual fixed values. Transparent declarations replace fixed
/// defaults, and fixed declarations cannot inherit values from a transparent default.
/// Each variant's resolved fixed metadata must contain both class and reason.
/// Delegation can select a named or tuple
/// field with `#[meta(transparent(field = source))]` or `field = 0`.
/// Only delegated field types receive `ApplicationError` bounds. The complete
/// type must implement `std::error::Error`; reason expressions must be static strings.
/// Expressions can use `self` and surrounding names; fields are not bound to local names.
/// Qualified class paths select caller-defined variants or associated constants directly.
///
/// Resolves the `tufo-application` Cargo dependency, including aliases, by default.
/// `#[meta(crate = path)]` overrides the facade, which must expose an `error` module
/// containing `ApplicationError`, `ErrorMetadata`, and `ErrorClass`. Short class names are
/// `InvalidArgument`, `NotFound`, `FailedPrecondition`, `Unavailable`, and `Internal`.
#[proc_macro_derive(ApplicationErrorMeta, attributes(meta))]
pub fn derive_application_error_meta(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    application_error_meta::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
