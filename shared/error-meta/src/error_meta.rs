mod input;

use input::{Classification, ClassifiedDeclaration, ErrorMetaInput};
use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::{DeriveInput, Path, Result, spanned::Spanned};

pub(super) fn expand(input: &DeriveInput) -> Result<TokenStream> {
    let input = ErrorMetaInput::parse(input)?;
    let ident = input.ident;
    let facade = &input.facade;
    let (impl_generics, type_generics, _) = input.generics.split_for_impl();
    let where_clause = classification_where_clause(&input);
    let arms = input
        .declarations
        .iter()
        .map(|declaration| declaration_arm(declaration, facade));

    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #facade::failure::Classified for #ident #type_generics #where_clause {
            fn classify(&self) -> #facade::failure::Classification {
                match self {
                    #(#arms)*
                }
            }
        }
    })
}

fn declaration_arm(declaration: &ClassifiedDeclaration<'_>, facade: &Path) -> TokenStream {
    let constructor = declaration
        .variant
        .map_or_else(|| quote!(Self), |ident| quote!(Self::#ident));
    let (pattern, value) = match &declaration.classification {
        Classification::Failure(field) => (
            field_pattern(&constructor, &field.member),
            quote! {
                #facade::failure::Classification::Public(
                    #facade::failure::PublicFailure::failure(inner),
                )
            },
        ),
        Classification::FailureExpression(reason) => (
            quote!(#constructor { .. }),
            quote! {
                #facade::failure::Classification::Public(
                    ::core::convert::Into::<#facade::failure::Failure>::into(#reason),
                )
            },
        ),
        Classification::Transparent(field) => (
            field_pattern(&constructor, &field.member),
            quote!(#facade::failure::Classified::classify(inner)),
        ),
        Classification::Private(class) => {
            let class = class.expression(&quote!(#facade::failure::ErrorClass));
            (
                quote!(#constructor { .. }),
                quote!(#facade::failure::Classification::Private(#class)),
            )
        }
    };
    quote!(#pattern => #value,)
}

fn field_pattern(constructor: &TokenStream, member: &syn::Member) -> TokenStream {
    quote!(#constructor { #member: inner, .. })
}

fn classification_where_clause(input: &ErrorMetaInput<'_>) -> TokenStream {
    let facade = &input.facade;
    let bounds: Vec<_> = input
        .declarations
        .iter()
        .filter_map(|declaration| classification_bound(declaration, facade))
        .collect();
    if input.generics.where_clause.is_none() && bounds.is_empty() {
        return TokenStream::new();
    }
    let predicates = input
        .generics
        .where_clause
        .iter()
        .flat_map(|clause| &clause.predicates);
    quote!(where #(#predicates,)* #(#bounds,)*)
}

fn classification_bound(
    declaration: &ClassifiedDeclaration<'_>,
    facade: &Path,
) -> Option<TokenStream> {
    let (field, trait_name) = match &declaration.classification {
        Classification::Failure(field) => (field, quote!(PublicFailure)),
        Classification::Transparent(field) => (field, quote!(Classified)),
        Classification::FailureExpression(_) | Classification::Private(_) => {
            return None;
        }
    };
    let field_type = field.field_type;
    Some(quote_spanned!(field_type.span()=> #field_type: #facade::failure::#trait_name))
}
