//! `#[derive(Mapped)]`: maps a plain struct onto a database table.
//!
//! ```ignore
//! #[derive(Mapped)]
//! #[table(name = "users")]
//! #[has_many(Order, foreign_key = "user_id")]
//! struct User {
//!     #[table(primary_key)]
//!     id: i64,
//!     name: String,
//!     active: bool,
//! }
//!
//! #[derive(Mapped)]
//! #[table(name = "orders")]
//! #[belongs_to(User, foreign_key = "user_id")]
//! struct Order {
//!     #[table(primary_key)]
//!     id: i64,
//!     user_id: i64,
//!     amount: i64,
//! }
//! ```
//!
//! generates:
//! - `impl Mapped for User` (`TABLE_NAME`, `COLUMNS`, `PRIMARY_KEY`, `VERSION_COLUMN`, `SOFT_DELETE_COLUMN`)
//! - `impl FromRow for User` (decodes a `Row` by column name)
//! - `impl Entity for User` (so `Session::add` can queue it generically)
//! - `User::table() -> Table`
//! - `User::insert(&self) -> Insert`
//! - `User::update(&self) -> Update`, `User::delete_query(&self) -> Delete`,
//!   and `impl Identifiable for User` (so `Session::update`/`delete` can
//!   queue it generically), only when a field is marked
//!   `#[table(primary_key)]`
//! - one `load_<child>s` async method per `#[has_many(Child, foreign_key =
//!   "...")]` attribute, batch-loading `Child` rows for a slice of `Self`
//!   (see `rusty_db_core::relations::load_many`)
//! - one `load_<child>` async method per `#[has_one(Child, foreign_key =
//!   "...")]` attribute — same direction as `#[has_many(...)]`, but for a
//!   relationship expected to have at most one matching row per parent:
//!   `Child` directly rather than a `Vec<Child>`, and a runtime
//!   `Error::Conflict` if a second row for the same parent ever turns up
//!   (see `rusty_db_core::relations::load_has_one`)
//! - one `load_<parent>` async method per `#[belongs_to(Parent, foreign_key
//!   = "...")]` attribute, batch-loading the referenced `Parent` rows for a
//!   slice of `Self` (see `rusty_db_core::relations::load_one`)
//! - one `load_<target>s` async method per `#[many_to_many(Target, through
//!   = "...", local_key = "...", foreign_key = "...")]` attribute,
//!   batch-loading every `Target` row joined to `Self` through a join
//!   table (see `rusty_db_core::relations::load_many_to_many`)
//! - `Self::delete_cascading(&self, engine)`, but only if at least one
//!   `has_many`/`has_one`/`many_to_many` attribute also carries a `cascade
//!   = "delete"` or `cascade = "orphan"` parameter — deletes (or, in
//!   `"orphan"` mode, nulls the foreign key of) every cascading
//!   relationship's rows, then deletes `self`, all in one transaction
//!
//! A field additionally marked `#[table(version)]` (requires
//! `#[table(primary_key)]` too) turns on optimistic locking: `update`'s
//! `WHERE` clause also requires that column to still match this struct's
//! own value (and its `SET` clause increments it), and the same column is
//! added to `delete_query`'s `WHERE` clause unchanged. See
//! `Session::update`/`delete`, which turn a resulting zero-rows-affected
//! outcome into `Error::Conflict` — someone else changed or deleted the
//! row since this struct was loaded.
//!
//! A field marked `#[table(soft_delete)]` (a `bool` column; requires
//! `#[table(primary_key)]` too) turns on soft deletes: `Session::delete`
//! marks the row (`SET <column> = true`) instead of removing it, and
//! `Session::get` treats an already-marked row as not found. `delete_query`
//! itself is unaffected — it's always a real `DELETE`, for explicit/direct
//! use outside a `Session`. See `Mapped::not_deleted_filter` for building
//! the same "still active" condition into your own queries.
//!
//! Field types must implement `Into<Value>` on an owned clone (i.e. the set
//! of types `Value` already converts from: `bool`, `i64`, `i32`, `f64`,
//! `String`, `Vec<u8>`, `Uuid`, and `Option<_>` of those). A
//! `#[table(version)]` field's type must also support `+ 1` (in practice,
//! `i64`/`i32`).

use heck::ToSnakeCase;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Data, DeriveInput, Fields, Token};

#[proc_macro_derive(Mapped, attributes(table, has_many, has_one, belongs_to, many_to_many))]
pub fn derive_mapped(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

struct FieldInfo {
    ident: syn::Ident,
    ty: syn::Type,
    column: String,
    primary_key: bool,
    version: bool,
    soft_delete: bool,
}

/// The shared shape of `#[has_many(Target, foreign_key = "...")]`,
/// `#[has_one(Target, foreign_key = "...")]`, and `#[belongs_to(Target,
/// foreign_key = "...")]` — all three also accept an optional `cascade =
/// "delete"` or `cascade = "orphan"` (meaningless, and rejected, on
/// `belongs_to`; see `expand_cascade_delete`).
struct RelationSpec {
    target: syn::Path,
    foreign_key: syn::LitStr,
    cascade: Option<syn::LitStr>,
}

impl Parse for RelationSpec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let target: syn::Path = input.parse()?;
        input.parse::<Token![,]>()?;

        let mut foreign_key: Option<syn::LitStr> = None;
        let mut cascade: Option<syn::LitStr> = None;

        loop {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: syn::LitStr = input.parse()?;
            if key == "foreign_key" {
                foreign_key = Some(value);
            } else if key == "cascade" {
                cascade = Some(value);
            } else {
                return Err(syn::Error::new_spanned(
                    &key,
                    "expected `foreign_key` or `cascade`",
                ));
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }

        Ok(RelationSpec {
            target,
            foreign_key: foreign_key.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "expected `foreign_key = \"...\"`",
                )
            })?,
            cascade,
        })
    }
}

/// `#[many_to_many(Target, through = "...", local_key = "...", foreign_key
/// = "...")]`'s shape: a join table (`through`) with a column referencing
/// this struct (`local_key`) and a column referencing `Target`
/// (`foreign_key`). The three required named parameters, plus the optional
/// `cascade = "delete"` (the only mode it supports — see
/// `expand_cascade_delete`), may appear in any order.
struct ManyToManySpec {
    target: syn::Path,
    through: syn::LitStr,
    local_key: syn::LitStr,
    foreign_key: syn::LitStr,
    cascade: Option<syn::LitStr>,
}

impl Parse for ManyToManySpec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let target: syn::Path = input.parse()?;
        input.parse::<Token![,]>()?;

        let mut through: Option<syn::LitStr> = None;
        let mut local_key: Option<syn::LitStr> = None;
        let mut foreign_key: Option<syn::LitStr> = None;
        let mut cascade: Option<syn::LitStr> = None;

        loop {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: syn::LitStr = input.parse()?;
            if key == "through" {
                through = Some(value);
            } else if key == "local_key" {
                local_key = Some(value);
            } else if key == "foreign_key" {
                foreign_key = Some(value);
            } else if key == "cascade" {
                cascade = Some(value);
            } else {
                return Err(syn::Error::new_spanned(
                    &key,
                    "expected `through`, `local_key`, `foreign_key`, or `cascade`",
                ));
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }

        Ok(ManyToManySpec {
            target,
            through: through.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "#[many_to_many(...)] requires `through = \"...\"`",
                )
            })?,
            local_key: local_key.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "#[many_to_many(...)] requires `local_key = \"...\"`",
                )
            })?,
            foreign_key: foreign_key.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "#[many_to_many(...)] requires `foreign_key = \"...\"`",
                )
            })?,
            cascade,
        })
    }
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
    let mut has_many: Vec<RelationSpec> = Vec::new();
    let mut has_one: Vec<RelationSpec> = Vec::new();
    let mut belongs_to: Vec<RelationSpec> = Vec::new();
    let mut many_to_many: Vec<ManyToManySpec> = Vec::new();
    for attr in &input.attrs {
        if attr.path().is_ident("table") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    table_name = Some(lit.value());
                    Ok(())
                } else {
                    Err(meta
                        .error("unsupported #[table(...)] attribute; expected `name = \"...\"`"))
                }
            })?;
        } else if attr.path().is_ident("has_many") {
            has_many.push(attr.parse_args::<RelationSpec>()?);
        } else if attr.path().is_ident("has_one") {
            has_one.push(attr.parse_args::<RelationSpec>()?);
        } else if attr.path().is_ident("belongs_to") {
            belongs_to.push(attr.parse_args::<RelationSpec>()?);
        } else if attr.path().is_ident("many_to_many") {
            many_to_many.push(attr.parse_args::<ManyToManySpec>()?);
        }
    }
    let table_name = table_name.unwrap_or_else(|| struct_ident.to_string().to_snake_case());

    let mut fields = Vec::with_capacity(named.named.len());
    for field in &named.named {
        let ident = field.ident.clone().expect("named field");
        let mut column = ident.to_string();
        let mut primary_key = false;
        let mut version = false;
        let mut soft_delete = false;

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
                } else if meta.path.is_ident("version") {
                    version = true;
                    Ok(())
                } else if meta.path.is_ident("soft_delete") {
                    soft_delete = true;
                    Ok(())
                } else {
                    Err(meta.error(
                        "unsupported #[table(...)] field attribute; expected `column = \"...\"`, `primary_key`, `version`, or `soft_delete`",
                    ))
                }
            })?;
        }

        fields.push(FieldInfo {
            ident,
            ty: field.ty.clone(),
            column,
            primary_key,
            version,
            soft_delete,
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

    let version_fields: Vec<&FieldInfo> = fields.iter().filter(|f| f.version).collect();
    if version_fields.len() > 1 {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "at most one field may be marked #[table(version)]",
        ));
    }
    let version_field = version_fields.into_iter().next();
    if version_field.is_some() && primary_key.is_none() {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "#[table(version)] requires a #[table(primary_key)] field too",
        ));
    }

    let soft_delete_fields: Vec<&FieldInfo> = fields.iter().filter(|f| f.soft_delete).collect();
    if soft_delete_fields.len() > 1 {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "at most one field may be marked #[table(soft_delete)]",
        ));
    }
    let soft_delete_field = soft_delete_fields.into_iter().next();
    if soft_delete_field.is_some() && primary_key.is_none() {
        return Err(syn::Error::new_spanned(
            struct_ident,
            "#[table(soft_delete)] requires a #[table(primary_key)] field too",
        ));
    }

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

    let version_column_const = match version_field {
        Some(f) => {
            let column = &f.column;
            quote! { ::std::option::Option::Some(#column) }
        }
        None => quote! { ::std::option::Option::None },
    };

    let soft_delete_column_const = match soft_delete_field {
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
                if f.version {
                    // The stored value becomes one more than what this
                    // struct was loaded with — the `WHERE` clause below
                    // makes that a no-op unless the row still has the old
                    // version, i.e. nobody else has changed it since.
                    quote! { .set(#column, ::std::clone::Clone::clone(&self.#ident) + 1) }
                } else {
                    quote! { .set(#column, ::std::clone::Clone::clone(&self.#ident)) }
                }
            });

            let filter_expr = match version_field {
                Some(vf) => {
                    let v_ident = &vf.ident;
                    let v_column = &vf.column;
                    quote! {
                        Self::table().col(#pk_column).eq(::std::clone::Clone::clone(&self.#pk_ident))
                            .and(Self::table().col(#v_column).eq(::std::clone::Clone::clone(&self.#v_ident)))
                    }
                }
                None => quote! {
                    Self::table().col(#pk_column).eq(::std::clone::Clone::clone(&self.#pk_ident))
                },
            };

            quote! {
                impl #struct_ident {
                    /// `UPDATE <table> SET <every non-primary-key field> WHERE <primary key> = self.<primary key>`
                    /// (and, with `#[table(version)]`, `AND <version> = self.<version>`, incrementing the
                    /// stored version — see `Session::update`, which turns a resulting zero-rows-affected
                    /// into `Error::Conflict`).
                    pub fn update(&self) -> #core::Update {
                        #core::Update::table(&Self::table())
                            #(#set_calls)*
                            .filter(#filter_expr)
                    }

                    /// `DELETE FROM <table> WHERE <primary key> = self.<primary key>`
                    /// (and, with `#[table(version)]`, `AND <version> = self.<version>`).
                    pub fn delete_query(&self) -> #core::Delete {
                        #core::Delete::from(&Self::table())
                            .filter(#filter_expr)
                    }

                    /// The value of the `#[table(primary_key)]` field.
                    pub fn primary_key_value(&self) -> #core::Value {
                        ::std::convert::Into::into(::std::clone::Clone::clone(&self.#pk_ident))
                    }
                }

                impl #core::Identifiable for #struct_ident {
                    fn update(&self) -> #core::Update {
                        Self::update(self)
                    }

                    fn delete_query(&self) -> #core::Delete {
                        Self::delete_query(self)
                    }

                    fn primary_key_value(&self) -> #core::Value {
                        Self::primary_key_value(self)
                    }
                }
            }
        }
        None => quote! {},
    };

    let has_many_impls = has_many
        .iter()
        .map(|spec| expand_has_many(struct_ident, &core, primary_key, spec))
        .collect::<syn::Result<Vec<_>>>()?;

    let has_one_impls = has_one
        .iter()
        .map(|spec| expand_has_one(struct_ident, &core, primary_key, spec))
        .collect::<syn::Result<Vec<_>>>()?;

    let belongs_to_impls = belongs_to
        .iter()
        .map(|spec| expand_belongs_to(struct_ident, &core, &fields, spec))
        .collect::<syn::Result<Vec<_>>>()?;

    let many_to_many_impls = many_to_many
        .iter()
        .map(|spec| expand_many_to_many(struct_ident, &core, primary_key, spec))
        .collect::<syn::Result<Vec<_>>>()?;

    let cascade_delete_impl = expand_cascade_delete(
        struct_ident,
        &core,
        primary_key,
        &has_many,
        &has_one,
        &many_to_many,
    )?;

    Ok(quote! {
        impl #core::Mapped for #struct_ident {
            const TABLE_NAME: &'static str = #table_name;
            const COLUMNS: &'static [&'static str] = &[#(#column_lits),*];
            const PRIMARY_KEY: ::std::option::Option<&'static str> = #primary_key_const;
            const VERSION_COLUMN: ::std::option::Option<&'static str> = #version_column_const;
            const SOFT_DELETE_COLUMN: ::std::option::Option<&'static str> = #soft_delete_column_const;
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

        impl #core::Entity for #struct_ident {
            fn insert(&self) -> #core::Insert {
                Self::insert(self)
            }
        }

        #update_and_delete
        #(#has_many_impls)*
        #(#has_one_impls)*
        #(#belongs_to_impls)*
        #(#many_to_many_impls)*
        #cascade_delete_impl
    })
}

/// `#[has_many(Child, foreign_key = "...")]` generates a batched loader
/// keyed by `Self`'s own primary key (which the children's `foreign_key`
/// column references).
fn expand_has_many(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    primary_key: Option<&FieldInfo>,
    spec: &RelationSpec,
) -> syn::Result<TokenStream2> {
    let pk = primary_key.ok_or_else(|| {
        syn::Error::new_spanned(
            &spec.target,
            "#[has_many(...)] requires a #[table(primary_key)] field on this struct",
        )
    })?;
    let pk_ident = &pk.ident;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}s", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// Batched ("select-in") eager load of the `#child` rows
            /// referencing these `#struct_ident`s, in a single extra query.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parents: &[#struct_ident],
            ) -> #core::Result<::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#child>>> {
                #core::relations::load_many::<#child, #pk_ty>(
                    engine,
                    parents.iter().map(|p| ::std::clone::Clone::clone(&p.#pk_ident)),
                    #fk_column,
                )
                .await
            }
        }
    })
}

/// `#[has_one(Child, foreign_key = "...")]` generates a batched loader keyed
/// by `Self`'s own primary key (which the child's `foreign_key` column
/// references) — the same direction as `#[has_many(...)]`, but for a
/// relationship expected to have at most one matching row per parent:
/// `Child` is returned directly rather than wrapped in a `Vec`, and a second
/// matching row for the same parent is a runtime `Error::Conflict` (see
/// `rusty_db_core::relations::load_has_one`) rather than something either
/// silently kept or silently dropped.
fn expand_has_one(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    primary_key: Option<&FieldInfo>,
    spec: &RelationSpec,
) -> syn::Result<TokenStream2> {
    let pk = primary_key.ok_or_else(|| {
        syn::Error::new_spanned(
            &spec.target,
            "#[has_one(...)] requires a #[table(primary_key)] field on this struct",
        )
    })?;
    let pk_ident = &pk.ident;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// Batched ("select-in") eager load of the single `#child` row
            /// (if any) referencing each of these `#struct_ident`s, in one
            /// extra query. `Err(Error::Conflict)` if more than one `#child`
            /// row references the same parent — this relationship is
            /// supposed to be one-to-one.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parents: &[#struct_ident],
            ) -> #core::Result<::std::collections::HashMap<#pk_ty, #child>> {
                #core::relations::load_has_one::<#child, #pk_ty>(
                    engine,
                    parents.iter().map(|p| ::std::clone::Clone::clone(&p.#pk_ident)),
                    #fk_column,
                )
                .await
            }
        }
    })
}

/// `#[many_to_many(Target, through = "...", local_key = "...", foreign_key
/// = "...")]` generates a batched loader keyed by `Self`'s own primary key,
/// fetching every `Target` row joined to it through the `through` table in
/// a single query (a real SQL `JOIN`, not two round trips).
fn expand_many_to_many(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    primary_key: Option<&FieldInfo>,
    spec: &ManyToManySpec,
) -> syn::Result<TokenStream2> {
    let pk = primary_key.ok_or_else(|| {
        syn::Error::new_spanned(
            &spec.target,
            "#[many_to_many(...)] requires a #[table(primary_key)] field on this struct",
        )
    })?;
    let pk_ident = &pk.ident;
    let pk_ty = &pk.ty;
    let target = &spec.target;
    let through = &spec.through;
    let local_key = &spec.local_key;
    let foreign_key = &spec.foreign_key;

    let target_name = target
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}s", target_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// Batched eager load of every `#target` row related to these
            /// `#struct_ident`s through the `#through` join table, in a
            /// single extra query (a real SQL `JOIN`, not select-in).
            pub async fn #method_ident(
                engine: &#core::Engine,
                parents: &[#struct_ident],
            ) -> #core::Result<::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#target>>> {
                let target_key_column = <#target as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[many_to_many(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_many_to_many::<#target, #pk_ty>(
                    engine,
                    parents.iter().map(|p| ::std::clone::Clone::clone(&p.#pk_ident)),
                    #through,
                    #local_key,
                    #foreign_key,
                    target_key_column,
                )
                .await
            }
        }
    })
}

/// Generates `Self::delete_cascading`, but only if at least one
/// `#[has_many(...)]`/`#[has_one(...)]`/`#[many_to_many(...)]` attribute on
/// this struct carries a `cascade = "..."` parameter — otherwise emits
/// nothing (an empty token stream), the same "only if the attribute is
/// present" shape every other generated method already follows.
///
/// `delete_cascading` runs every cascading relationship's cleanup query
/// first (a `has_many`/`has_one` in `cascade = "delete"` mode issues a
/// `DELETE`; in `cascade = "orphan"` mode, an `UPDATE ... SET <foreign_key>
/// = NULL`; a `many_to_many` in `cascade = "delete"` mode — its only
/// supported mode — deletes the join-table rows, never the `Target` rows
/// themselves, since those may still be referenced by other parents), then
/// deletes `self`, all inside one transaction: a failure at any point
/// rolls the whole thing back, leaving nothing changed.
fn expand_cascade_delete(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    primary_key: Option<&FieldInfo>,
    has_many: &[RelationSpec],
    has_one: &[RelationSpec],
    many_to_many: &[ManyToManySpec],
) -> syn::Result<TokenStream2> {
    let cascading_relations: Vec<&RelationSpec> = has_many
        .iter()
        .chain(has_one.iter())
        .filter(|spec| spec.cascade.is_some())
        .collect();
    let cascading_many_to_many: Vec<&ManyToManySpec> = many_to_many
        .iter()
        .filter(|spec| spec.cascade.is_some())
        .collect();

    if cascading_relations.is_empty() && cascading_many_to_many.is_empty() {
        return Ok(quote! {});
    }

    let pk = primary_key.ok_or_else(|| {
        syn::Error::new_spanned(
            struct_ident,
            "`cascade = \"...\"` requires a #[table(primary_key)] field on this struct",
        )
    })?;
    let pk_ident = &pk.ident;

    let mut cascade_stmts = Vec::new();

    for spec in &cascading_relations {
        let child = &spec.target;
        let fk_column = &spec.foreign_key;
        let cascade_lit = spec.cascade.as_ref().expect("filtered to Some above");
        let stmt = match cascade_lit.value().as_str() {
            "delete" => quote! {
                let child_table = #core::Table::new(<#child as #core::Mapped>::TABLE_NAME);
                let query = #core::Delete::from(&child_table)
                    .filter(child_table.col(#fk_column).eq(::std::clone::Clone::clone(&self.#pk_ident)));
                if let ::std::result::Result::Err(err) = txn.execute_query(&query, engine.dialect()).await {
                    txn.rollback().await?;
                    return ::std::result::Result::Err(err);
                }
            },
            "orphan" => quote! {
                let child_table = #core::Table::new(<#child as #core::Mapped>::TABLE_NAME);
                let query = #core::Update::table(&child_table)
                    .set(#fk_column, #core::Value::Null)
                    .filter(child_table.col(#fk_column).eq(::std::clone::Clone::clone(&self.#pk_ident)));
                if let ::std::result::Result::Err(err) = txn.execute_query(&query, engine.dialect()).await {
                    txn.rollback().await?;
                    return ::std::result::Result::Err(err);
                }
            },
            other => {
                return Err(syn::Error::new_spanned(
                    cascade_lit,
                    format!(
                        "unsupported cascade mode {other:?}; expected \"delete\" or \"orphan\""
                    ),
                ));
            }
        };
        cascade_stmts.push(quote! { { #stmt } });
    }

    for spec in &cascading_many_to_many {
        let through = &spec.through;
        let local_key = &spec.local_key;
        let cascade_lit = spec.cascade.as_ref().expect("filtered to Some above");
        if cascade_lit.value() != "delete" {
            return Err(syn::Error::new_spanned(
                cascade_lit,
                format!(
                    "unsupported cascade mode {:?} for #[many_to_many(...)]; only \"delete\" \
                     is supported there (it deletes the join-table rows, never the target's own \
                     rows, which may still be referenced by other parents)",
                    cascade_lit.value()
                ),
            ));
        }
        cascade_stmts.push(quote! {
            {
                let through_table = #core::Table::new(#through);
                let query = #core::Delete::from(&through_table)
                    .filter(through_table.col(#local_key).eq(::std::clone::Clone::clone(&self.#pk_ident)));
                if let ::std::result::Result::Err(err) = txn.execute_query(&query, engine.dialect()).await {
                    txn.rollback().await?;
                    return ::std::result::Result::Err(err);
                }
            }
        });
    }

    Ok(quote! {
        impl #struct_ident {
            /// Deletes this row, first running every cascading relationship's
            /// cleanup query (see each `cascade = "..."` relation attribute
            /// for what it does), all inside one transaction — a failure at
            /// any point rolls the whole thing back, leaving nothing changed.
            ///
            /// A plain `Engine`-based alternative to `Session::delete`, not
            /// integrated with it (no identity-map eviction, no audit
            /// logging, no soft-delete) — call this directly when you want
            /// cascading, the same way `delete_query()` is a direct
            /// alternative to going through a `Session` at all.
            pub async fn delete_cascading(&self, engine: &#core::Engine) -> #core::Result<()> {
                let mut txn = engine.begin().await?;
                #(#cascade_stmts)*
                if let ::std::result::Result::Err(err) =
                    txn.execute_query(&self.delete_query(), engine.dialect()).await
                {
                    txn.rollback().await?;
                    return ::std::result::Result::Err(err);
                }
                txn.commit().await
            }
        }
    })
}

/// `#[belongs_to(Parent, foreign_key = "...")]` generates a batched loader
/// keyed by the `Parent`'s primary key, using `Self`'s own `foreign_key`
/// field as the value to look up.
fn expand_belongs_to(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    fields: &[FieldInfo],
    spec: &RelationSpec,
) -> syn::Result<TokenStream2> {
    if let Some(cascade) = &spec.cascade {
        return Err(syn::Error::new_spanned(
            cascade,
            "#[belongs_to(...)] doesn't support `cascade` — cascade rules belong on the \
             has_many/has_one side (the parent whose delete triggers the cascade), not on \
             belongs_to (the child side)",
        ));
    }
    let fk_column_value = spec.foreign_key.value();
    let fk_field = fields.iter().find(|f| f.column == fk_column_value).ok_or_else(|| {
        syn::Error::new_spanned(
            &spec.foreign_key,
            format!("no field maps to column {fk_column_value:?}; #[belongs_to(...)]'s foreign_key must name a column on this struct"),
        )
    })?;
    let fk_ident = &fk_field.ident;
    let fk_ty = &fk_field.ty;
    let parent = &spec.target;

    let parent_name = parent
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}", parent_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// Batched eager load of the `#parent` rows these
            /// `#struct_ident`s reference, in a single extra query.
            pub async fn #method_ident(
                engine: &#core::Engine,
                children: &[#struct_ident],
            ) -> #core::Result<::std::collections::HashMap<#fk_ty, #parent>> {
                let parent_key_column = <#parent as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[belongs_to(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_one::<#parent, #fk_ty>(
                    engine,
                    children.iter().map(|c| ::std::clone::Clone::clone(&c.#fk_ident)),
                    parent_key_column,
                )
                .await
            }
        }
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
