mod attributes;

use syn::{
    Attribute, Data, DeriveInput, Error, Expr, Fields, Generics, Ident, Index, Member, Path,
    Result, Type, spanned::Spanned,
};

use crate::error_class::ErrorClass;

pub(super) struct ErrorMetaInput<'a> {
    pub(super) ident: &'a Ident,
    pub(super) generics: &'a Generics,
    pub(super) facade: Path,
    pub(super) declarations: Vec<ClassifiedDeclaration<'a>>,
}

pub(super) struct ClassifiedDeclaration<'a> {
    pub(super) variant: Option<&'a Ident>,
    pub(super) classification: Classification<'a>,
}

pub(super) enum Classification<'a> {
    Failure(DelegatedField<'a>),
    FailureExpression(Box<Expr>),
    Transparent(DelegatedField<'a>),
    Private(ErrorClass),
}

pub(super) struct DelegatedField<'a> {
    pub(super) member: Member,
    pub(super) field_type: &'a Type,
}

#[derive(Clone)]
enum RawClassification {
    Failure(Option<Member>),
    FailureExpression(Box<Expr>),
    Transparent(Option<Member>),
    Private(ErrorClass),
}

impl<'a> ErrorMetaInput<'a> {
    pub(super) fn parse(input: &'a DeriveInput) -> Result<Self> {
        let mut errors = None;
        let mut facade_override = None;
        let mut item_metadata = Vec::new();
        for attribute in metadata_attributes(&input.attrs) {
            if crate::facade::is_override(attribute) {
                match attributes::facade_override(attribute, facade_override.is_some()) {
                    Ok(path) => facade_override = Some(path),
                    Err(error) => combine_error(&mut errors, error),
                }
            } else {
                item_metadata.push(attribute);
            }
        }

        let item_classification = match classification_attribute(&item_metadata, "item")
            .and_then(|attribute| attribute.map(RawClassification::parse).transpose())
        {
            Ok(metadata) => metadata,
            Err(error) => {
                combine_error(&mut errors, error);
                None
            }
        };
        let mut declarations = Vec::new();
        match &input.data {
            Data::Enum(data) => {
                for variant in &data.variants {
                    reject_field_metadata(&variant.fields, &mut errors);
                    let declaration = classification_attribute(
                        &metadata_attributes(&variant.attrs).collect::<Vec<_>>(),
                        "variant",
                    )
                    .and_then(|attribute| match attribute {
                        Some(attribute) => RawClassification::parse(attribute).map(Some),
                        None => Ok(item_classification.clone()),
                    })
                    .and_then(|metadata| {
                        ClassifiedDeclaration::parse(
                            &variant.ident,
                            &variant.fields,
                            Some(&variant.ident),
                            metadata,
                        )
                    });
                    match declaration {
                        Ok(declaration) => declarations.push(declaration),
                        Err(error) => combine_error(&mut errors, error),
                    }
                }
            }
            Data::Struct(data) => {
                reject_field_metadata(&data.fields, &mut errors);
                match ClassifiedDeclaration::parse(
                    &input.ident,
                    &data.fields,
                    None,
                    item_classification,
                ) {
                    Ok(declaration) => declarations.push(declaration),
                    Err(error) => combine_error(&mut errors, error),
                }
            }
            Data::Union(_) => combine_error(
                &mut errors,
                Error::new(
                    input.ident.span(),
                    "ErrorMeta can only be derived for enums and structs",
                ),
            ),
        }
        if let Some(errors) = errors {
            return Err(errors);
        }

        Ok(Self {
            ident: &input.ident,
            generics: &input.generics,
            facade: crate::facade::resolve(facade_override, input.ident.span())?,
            declarations,
        })
    }
}

impl<'a> ClassifiedDeclaration<'a> {
    fn parse(
        ident: &'a Ident,
        fields: &'a Fields,
        variant: Option<&'a Ident>,
        metadata: Option<RawClassification>,
    ) -> Result<Self> {
        let metadata = metadata.ok_or_else(|| {
            let kind = if variant.is_some() {
                "variant"
            } else {
                "struct"
            };
            Error::new(
                ident.span(),
                format!(
                    "{kind} `{ident}` needs a classification: {}",
                    attributes::META_USAGE
                ),
            )
        })?;
        let classification = match metadata {
            RawClassification::Failure(selection) => Classification::Failure(
                DelegatedField::parse(ident, fields, variant.is_some(), "failure", selection)?,
            ),
            RawClassification::FailureExpression(reason) => {
                Classification::FailureExpression(reason)
            }
            RawClassification::Transparent(selection) => Classification::Transparent(
                DelegatedField::parse(ident, fields, variant.is_some(), "transparent", selection)?,
            ),
            RawClassification::Private(class) => Classification::Private(class),
        };
        Ok(Self {
            variant,
            classification,
        })
    }
}

impl<'a> DelegatedField<'a> {
    fn parse(
        ident: &Ident,
        fields: &'a Fields,
        is_variant: bool,
        meta: &str,
        selection: Option<Member>,
    ) -> Result<Self> {
        let kind = if is_variant { "variant" } else { "struct" };
        let Some(member) = selection else {
            let mut fields = fields.iter();
            return match (fields.next(), fields.next()) {
                (Some(field), None) => Ok(Self {
                    member: match &field.ident {
                        Some(ident) => Member::Named(ident.clone()),
                        None => Member::Unnamed(Index::from(0)),
                    },
                    field_type: &field.ty,
                }),
                _ => Err(Error::new(
                    ident.span(),
                    format!("`#[meta({meta})]` requires a {kind} with exactly one field"),
                )),
            };
        };

        let field = match (fields, &member) {
            (Fields::Named(fields), Member::Named(name)) => fields
                .named
                .iter()
                .find(|field| field.ident.as_ref() == Some(name)),
            (Fields::Unnamed(fields), Member::Unnamed(index)) => usize::try_from(index.index)
                .ok()
                .and_then(|index| fields.unnamed.iter().nth(index)),
            _ => None,
        };
        let Some(field) = field else {
            return Err(Error::new(
                member.span(),
                format!("{kind} `{ident}` has no {}", member_description(&member)),
            ));
        };
        Ok(Self {
            member,
            field_type: &field.ty,
        })
    }
}

fn member_description(member: &Member) -> String {
    match member {
        Member::Named(name) => format!("named field `{name}`"),
        Member::Unnamed(index) => format!("tuple field at index {}", index.index),
    }
}

fn classification_attribute<'a>(
    attributes: &[&'a Attribute],
    kind: &str,
) -> Result<Option<&'a Attribute>> {
    let mut errors = None;
    for duplicate in attributes.iter().skip(1) {
        combine_error(
            &mut errors,
            Error::new_spanned(
                duplicate,
                format!("each {kind} takes at most one #[meta(...)] attribute"),
            ),
        );
    }
    if let Some(error) = errors {
        return Err(error);
    }
    Ok(attributes.first().copied())
}

fn reject_field_metadata(fields: &Fields, errors: &mut Option<Error>) {
    for attribute in fields
        .iter()
        .flat_map(|field| metadata_attributes(&field.attrs))
    {
        combine_error(
            errors,
            Error::new_spanned(
                attribute,
                "classification attributes belong on enum variants or structs, not fields",
            ),
        );
    }
}

fn metadata_attributes(attributes: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("meta"))
}

fn combine_error(errors: &mut Option<Error>, error: Error) {
    match errors {
        Some(errors) => errors.combine(error),
        None => *errors = Some(error),
    }
}
