//! Column mappings for validated domain values.
//!
//! Toasty's tuple-newtype `Embed` derive builds the wrapper without its constructor, so a value
//! with an invariant maps through [`checked_column!`] instead: every row and projection decodes
//! through the constructor, and a stored value that breaks the invariant fails the read.

/// Stores `$domain` as one `$stored` column. `$decode` turns the stored value into the domain
/// value or an error to report, and `$encode` borrows the domain value into the stored one.
macro_rules! checked_column {
    ($domain:ty, $stored:ty, decode = $decode:expr, encode = $encode:expr $(,)?) => {
        impl ::toasty::schema::Load for $domain {
            type Output = Self;

            fn ty() -> ::toasty::stmt::Type {
                <$stored as ::toasty::schema::Load>::ty()
            }

            fn load(value: ::toasty::stmt::Value) -> ::toasty::Result<Self> {
                let stored = <$stored as ::toasty::schema::Load>::load(value)?;
                let decode: fn($stored) -> Result<Self, _> = $decode;
                decode(stored).map_err(|error| {
                    ::toasty::Error::invalid_result(format!(
                        "invalid stored {}: {error}",
                        stringify!($domain)
                    ))
                })
            }

            fn reload(target: &mut Self, value: ::toasty::stmt::Value) -> ::toasty::Result<()> {
                *target = Self::load(value)?;
                Ok(())
            }
        }

        impl ::toasty::schema::Field for $domain {
            type ExprTarget = Self;
            type Path<Origin> = ::toasty::stmt::Path<Origin, Self>;
            type ListPath<Origin> = ::toasty::stmt::Path<Origin, ::toasty::stmt::List<Self>>;
            type Update<'a> = ();
            type Inner = Self;

            fn new_path<Origin>(path: ::toasty::stmt::Path<Origin, Self>) -> Self::Path<Origin> {
                path
            }

            fn new_list_path<Origin>(
                path: ::toasty::stmt::Path<Origin, ::toasty::stmt::List<Self>>,
            ) -> Self::ListPath<Origin> {
                path
            }

            fn new_update(
                _: &mut ::toasty::codegen_support::core::stmt::Assignments,
                _: ::toasty::stmt::Projection,
            ) -> Self::Update<'_> {
            }

            fn key_constraint<Origin>(
                &self,
                target: ::toasty::stmt::Path<Origin, Self>,
            ) -> ::toasty::stmt::Expr<bool> {
                target.eq(self)
            }
        }

        impl ::toasty::codegen_support::index::IndexableField for $domain {}

        impl ::toasty::stmt::IntoExpr<Self> for $domain {
            fn into_expr(self) -> ::toasty::stmt::Expr<Self> {
                self.by_ref()
            }

            fn by_ref(&self) -> ::toasty::stmt::Expr<Self> {
                let encode: fn(&Self) -> $stored = $encode;
                ::toasty::stmt::Expr::from_untyped(::toasty::stmt::Value::from(encode(self)))
            }
        }

        impl ::toasty::stmt::Assign<Self> for $domain {
            fn into_assignment(self) -> ::toasty::stmt::Assignment<Self> {
                ::toasty::stmt::set(self)
            }
        }
    };
}

pub(crate) use checked_column;

macro_rules! uuid_identifier {
    ($(#[$attribute:meta])* $name:ident) => {
        $(#[$attribute])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, toasty::Embed)]
        pub struct $name(::toasty::stmt::Uuid);

        impl $name {
            #[must_use]
            pub fn from_uuid(uuid: ::toasty::stmt::Uuid) -> Self {
                Self(uuid)
            }

            #[must_use]
            pub fn into_uuid(self) -> ::toasty::stmt::Uuid {
                self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                self.0.hyphenated().fmt(formatter)
            }
        }
    };
}

pub(crate) use uuid_identifier;
