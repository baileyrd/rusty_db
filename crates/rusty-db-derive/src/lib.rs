//! `#[derive(Mapped)]`: maps a plain struct onto a database table.
//!
//! ```ignore
//! #[derive(Mapped)]
//! #[table(name = "users")]
//! struct User {
//!     #[table(primary_key)]
//!     id: i64,
//!     name: String,
//!     active: bool,
//! }
//! ```
//!
//! generates:
//! - `impl Mapped for User` (`TABLE_NAME`, `COLUMNS`, `PRIMARY_KEY`)
//! - `impl FromRow for User` (decodes a `Row` by column name)
//! - `User::table() -> Table`
//! - `User::insert(&self) -> Insert`
//! - `User::update(&self) -> Update` and `User::delete_query(&self) -> Delete`,
//!   only when a field is marked `#[table(primary_key)]`
//!
//! Field types must implement `Into<Value>` on an owned clone (i.e. the set
//! of types `Value` already converts from: `bool`, `i64`, `i32`, `f64`,
//! `String`, `Vec<u8>`, and `Option<_>` of those).

use heck::ToSnakeCase;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields};

#[proc_macro_derive(Mapped, attributes(table))]
pub fn derive_mapped(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct FieldInfo {
    ident: syn::Ident,
    column: String,
    primary_key: bool,
}

fn expand(input: DeriveInput) -> syn::Result<TokenStream2> {
    let struct_ident = &input.ident;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Mapped)] only supports structs",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Mapped)] requires named fields",
        ));
    };

    let mut table_name: Option<String> = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("table") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                table_name = Some(lit.value());
                Ok(())
            } else {
                Err(meta.error("unsupported #[table(...)] attribute; expected `name = \"...\"`"))
            }
        })?;
    }
    let table_name = table_name.unwrap_or_else(|| struct_ident.to_string().to_snake_case());

    let mut fields = Vec::with_capacity(named.named.len());
    for field in &named.named {
        let ident = field.ident.clone().expect("named field");
        let mut column = ident.to_string();
        let mut primary_key = false;

        for attr in &field.attrs {
            if !attr.path().is_ident("table") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("column") {
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    column = lit.value();
                    Ok(())
                } else if meta.path.is_ident("primary_key") {
                    primary_key = true;
                    Ok(())
                } else {
                    Err(meta.error(
                        "unsupported #[table(...)] field attribute; expected `column = \"...\"` or `primary_key`",
                    ))
                }
            })?;
        }

        fields.push(FieldInfo {
            ident,
            column,
            primary_key,
        });
    }

    let primary_keys: Vec<&FieldInfo> = fields.iter().filter(|f| f.primary_key).collect();
    if primary_keys.len() > 1 {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "at most one field may be marked #[table(primary_key)]",
        ));
    }
    let primary_key = primary_keys.into_iter().next();

    let core = core_crate_path();

    let column_lits: Vec<&str> = fields.iter().map(|f| f.column.as_str()).collect();
    let from_row_fields = fields.iter().map(|f| {
        let ident = &f.ident;
        let column = &f.column;
        quote! { #ident: row.get_by_name(#column)? }
    });
    let insert_calls = fields.iter().map(|f| {
        let ident = &f.ident;
        let column = &f.column;
        quote! { .value(#column, ::std::clone::Clone::clone(&self.#ident)) }
    });

    let primary_key_const = match primary_key {
        Some(f) => {
            let column = &f.column;
            quote! { ::std::option::Option::Some(#column) }
        }
        None => quote! { ::std::option::Option::None },
    };

    let update_and_delete = match primary_key {
        Some(pk) => {
            let pk_ident = &pk.ident;
            let pk_column = &pk.column;
            let set_calls = fields.iter().filter(|f| !f.primary_key).map(|f| {
                let ident = &f.ident;
                let column = &f.column;
                quote! { .set(#column, ::std::clone::Clone::clone(&self.#ident)) }
            });
            quote! {
                impl #struct_ident {
                    /// `UPDATE <table> SET <every non-primary-key field> WHERE <primary key> = self.<primary key>`.
                    pub fn update(&self) -> #core::Update {
                        #core::Update::table(&Self::table())
                            #(#set_calls)*
                            .filter(Self::table().col(#pk_column).eq(::std::clone::Clone::clone(&self.#pk_ident)))
                    }

                    /// `DELETE FROM <table> WHERE <primary key> = self.<primary key>`.
                    pub fn delete_query(&self) -> #core::Delete {
                        #core::Delete::from(&Self::table())
                            .filter(Self::table().col(#pk_column).eq(::std::clone::Clone::clone(&self.#pk_ident)))
                    }
                }
            }
        }
        None => quote! {},
    };

    Ok(quote! {
        impl #core::Mapped for #struct_ident {
            const TABLE_NAME: &'static str = #table_name;
            const COLUMNS: &'static [&'static str] = &[#(#column_lits),*];
            const PRIMARY_KEY: ::std::option::Option<&'static str> = #primary_key_const;
        }

        impl #core::FromRow for #struct_ident {
            fn from_row(row: &#core::Row) -> #core::Result<Self> {
                ::std::result::Result::Ok(#struct_ident {
                    #(#from_row_fields),*
                })
            }
        }

        impl #struct_ident {
            /// The table this struct maps to.
            pub fn table() -> #core::Table {
                #core::Table::new(#table_name)
            }

            /// `INSERT INTO <table> (...) VALUES (...)` populated from `self`.
            pub fn insert(&self) -> #core::Insert {
                #core::Insert::into_table(&Self::table())
                    #(#insert_calls)*
            }
        }

        #update_and_delete
    })
}

/// Resolves the path to refer to rusty-db-core's items from the caller's
/// crate, whether they depend on `rusty-db-core` directly or only on the
/// `rusty-db` facade crate (which re-exports everything this macro needs).
fn core_crate_path() -> TokenStream2 {
    use proc_macro_crate::{crate_name, FoundCrate};

    if let Ok(found) = crate_name("rusty-db-core") {
        return match found {
            FoundCrate::Itself => quote!(crate),
            FoundCrate::Name(name) => {
                let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
                quote!(::#ident)
            }
        };
    }

    if let Ok(found) = crate_name("rusty-db") {
        return match found {
            FoundCrate::Itself => quote!(crate),
            FoundCrate::Name(name) => {
                let ident = syn::Ident::new(&name, proc_macro2::Span::call_site());
                quote!(::#ident)
            }
        };
    }

    // Best effort: neither dependency was found under its expected name
    // (e.g. a workspace-internal test); fall back to the default path.
    quote!(::rusty_db_core)
}
