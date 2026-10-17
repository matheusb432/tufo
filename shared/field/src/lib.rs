// Copyright 2023 Indraneel Mahendrakumar.
//
// Licensed under either the MIT License or Apache License 2.0, at your option.
// This file may not be copied, modified, or distributed except according to
// those terms.
//
// Modified in 2026: upgraded to Rust 2024 and the workspace lint policy, and
// removed a broad lint suppression from the macro expansion.

//! Statically verified `struct` field names as strings.
//!
//! The [`field!`] macro evaluates to a field's name after verifying that the
//! field exists on the specified `struct`. The type and field must be visible
//! at the point where the macro is invoked.
//!
//! # Usage
//!
//! Invoke the macro as `field!(field @ Struct)`:
//!
//! ```
//! use field::field;
//!
//! struct User {
//!     name: String,
//!     age: u32,
//! }
//!
//! let name = field!(name @ User);
//! assert_eq!(name, "name");
//!
//! let age = field!(age @ User);
//! assert_eq!(age, "age");
//! ```
//!
//! A field that is not on the specified `struct`, or a type that is not in
//! scope, causes a compilation error:
//!
//! ```compile_fail
//! use field::field;
//!
//! struct User {
//!     name: String,
//! }
//!
//! let address = field!(address @ User);
//! ```
//!
//! # Generics
//!
//! [`field!`] works with generic types when concrete type parameters are
//! provided:
//!
//! ```
//! use field::field;
//!
//! struct Pair<T> {
//!     first: T,
//!     second: T,
//! }
//!
//! let first = field!(first @ Pair<()>);
//! assert_eq!(first, "first");
//!
//! let second = field!(second @ Pair<i32>);
//! assert_eq!(second, "second");
//! ```
//!
//! # Paths and visibility
//!
//! Paths are accepted, but both the type and field must be accessible at the
//! invocation site:
//!
//! ```
//! use field::field;
//!
//! mod wrapper {
//!     pub struct Wrapper<T> {
//!         pub inner: T,
//!     }
//! }
//!
//! let inner = field!(inner @ wrapper::Wrapper<()>);
//! assert_eq!(inner, "inner");
//! ```
//!
//! ```compile_fail
//! use field::field;
//!
//! mod example {
//!     pub struct Secret {
//!         value: String,
//!     }
//! }
//!
//! let value = field!(value @ example::Secret);
//! ```
//!
//! # Dependencies
//!
//! This crate has no dependencies and supports `#![no_std]` consumers.

#![warn(missing_docs)]
#![no_std]

/// Verifies a field on a `struct` at compile time and returns its name.
///
/// Invoke the macro as `field!(field @ Struct)`. The expansion verifies that
/// `field` exists on `Struct` and is visible at the invocation site, then
/// evaluates to the field name as a `&'static str`.
#[macro_export]
macro_rules! field {
    ($field:ident @ $structure:ty) => {{
        fn _assert_field_exists(value: $structure) {
            let _ = value.$field;
        }
        ::core::stringify!($field)
    }};
}
