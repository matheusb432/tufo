mod input;

use input::{ApplicationInput, Declaration, Metadata};
use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::{DeriveInput, Path, Result, spanned::Spanned};

pub(super) fn expand(input: &DeriveInput) -> Result<TokenStream> {
    let input = ApplicationInput::parse(input)?;
    let ident = input.ident;
    let facade = &input.facade;
    let mut generics = input.generics.clone();
    let (_, type_generics, _) = input.generics.split_for_impl();
    let predicates = &mut generics.make_where_clause().predicates;
    predicates.push(syn::parse_quote!(#ident #type_generics: ::std::error::Error));
    for declaration in &input.declarations {
        if let Metadata::Transparent(field) = &declaration.metadata {
            let ty = field.ty;
            predicates
                .push(syn::parse_quote_spanned!(ty.span()=> #ty: #facade::error::ApplicationError));
        }
    }
    let (impl_generics, type_generics, where_clause) = generics.split_for_impl();
    let arms = input
        .declarations
        .iter()
        .map(|declaration| arm(declaration, facade));
    Ok(quote! {
        #[automatically_derived]
        impl #impl_generics #facade::error::ApplicationError for #ident #type_generics #where_clause {
            fn metadata(&self) -> #facade::error::ErrorMetadata {
                match self { #(#arms)* }
            }
        }
    })
}

fn arm(declaration: &Declaration<'_>, facade: &Path) -> TokenStream {
    let constructor = declaration
        .variant
        .map_or_else(|| quote!(Self), |ident| quote!(Self::#ident));
    let (pattern, value) = match &declaration.metadata {
        Metadata::Fixed { class, reason } => {
            let class = class.expression(&quote!(#facade::error::ErrorClass));
            (
                quote!(#constructor { .. }),
                quote!(#facade::error::ErrorMetadata {
                    class: #class,
                    reason: #reason,
                }),
            )
        }
        Metadata::Transparent(field) => {
            let member = &field.member;
            (
                quote!(#constructor { #member: inner, .. }),
                quote_spanned!(field.ty.span()=> #facade::error::ApplicationError::metadata(inner)),
            )
        }
    };
    quote!(#pattern => #value,)
}
