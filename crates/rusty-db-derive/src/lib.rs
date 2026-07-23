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
//! A field additionally marked `#[table(default = "...")]` (a raw SQL
//! fragment, e.g. `"CURRENT_TIMESTAMP"` or `"'pending'"` — distinct from a
//! database-side column `DEFAULT`, which this crate already reflects but
//! never applies) makes `insert()` substitute that fragment for the column
//! whenever this struct's field currently equals `Default::default()` for
//! its type, so `Session::add` can leave a field at its type's default and
//! still get a real value in the row. Since Rust has no "unset" field
//! state, a genuine value equal to the type's default (e.g. an explicit
//! `0`) is indistinguishable from "left unset" and also gets the default
//! fragment — not usable on a `#[table(primary_key)]` field (a compile
//! error), since a primary key's value must always be supplied explicitly.
//!
//! Field types must implement `Into<Value>` on an owned clone (i.e. the set
//! of types `Value` already converts from: `bool`, `i64`, `i32`, `f64`,
//! `String`, `Vec<u8>`, `Uuid`, `BigDecimal`, `Json`, `NaiveDate`,
//! `NaiveTime`, `NaiveDateTime`, `DateTime<Utc>`, `Vec<T>` for a handful of
//! element types `T` (`bool`/`i64`/`f64`/`String`/`Uuid`/`BigDecimal`/the
//! four temporal types above/`Value` itself), and `Option<_>` of any of
//! those — plus any type carrying `#[derive(MappedEnum)]`/
//! `#[derive(MappedNewtype)]`, below, or implementing `Into<Value>`/
//! `FromValue` itself by hand). A
//! `#[table(version)]` field's type must also support `+ 1` (in practice,
//! `i64`/`i32`).
//!
//! `#[derive(MappedEnum)]`: maps a fieldless (unit-variant-only) enum onto
//! a single column, so it can be used directly as a `#[derive(Mapped)]`
//! field type.
//!
//! ```ignore
//! #[derive(Debug, Clone, Copy, PartialEq, MappedEnum)]
//! enum Status {
//!     Active,
//!     Inactive,
//!     #[mapped_enum(rename = "banned_user")]
//!     Banned,
//! }
//! ```
//!
//! generates `impl From<Status> for Value` and `impl FromValue for
//! Status`. By default each variant maps to its snake_case name as text
//! (`Active` -> `"active"`) — override one with `#[mapped_enum(rename =
//! "...")]` on that variant. `#[mapped_enum(as_int)]` on the enum itself
//! switches the whole thing to store each variant's own discriminant
//! (`v as i64`) instead of text; unlike the text form, this doesn't
//! survive the enum's variants being reordered or renumbered later, since
//! the stored value is just a bare integer with no record of which
//! variant it meant.
//!
//! `#[derive(MappedNewtype)]`: maps a single-field tuple struct onto
//! whatever `Value` its own field already converts to/from, so it can be
//! used directly as a `#[derive(Mapped)]` field type too — the
//! newtype/enum pairing named in the "map a newtype or enum onto a
//! `Value`" escape hatch this crate offers instead of waiting on every
//! type application code wants to become a built-in `Value` variant.
//!
//! ```ignore
//! #[derive(Debug, Clone, PartialEq, MappedNewtype)]
//! struct Email(String);
//! ```
//!
//! generates `impl From<Email> for Value` and `impl FromValue for Email`,
//! each delegating straight to the wrapped field's own conversion (here,
//! `String`'s). This is only a boilerplate-avoider for the common case of
//! a newtype around one already-`Value`-compatible type (including
//! another `MappedEnum`/`MappedNewtype`, so these compose); anything more
//! involved — validating a raw value while decoding it, or combining more
//! than one column into a single field — still means implementing
//! `Into<Value>`/`FromValue` by hand, which needs no macro and no special
//! support from this crate at all: both traits are ordinary public traits
//! any downstream type can implement itself.

use heck::ToSnakeCase;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{parse_macro_input, Data, DeriveInput, Fields, Token};

#[proc_macro_derive(
    Mapped,
    attributes(table, has_many, has_one, belongs_to, many_to_many, hybrid)
)]
pub fn derive_mapped(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[proc_macro_derive(MappedEnum, attributes(mapped_enum))]
pub fn derive_mapped_enum(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_mapped_enum(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[proc_macro_derive(MappedNewtype)]
pub fn derive_mapped_newtype(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_mapped_newtype(input)
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
    default: Option<syn::LitStr>,
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

/// `#[hybrid(name = "...", expr = "...", ty = "...")]`'s shape — a
/// struct-level, repeatable attribute (SQLAlchemy's `hybrid_property`
/// equivalent for arithmetic, and a single top-level comparison of two
/// such arithmetic expressions, over this struct's own sibling fields).
/// `ty` is optional: inferred as `bool` for a comparison expression, or
/// from the first field the expression references
/// when omitted.
struct HybridSpec {
    name: syn::LitStr,
    expr: syn::LitStr,
    ty: Option<syn::Type>,
}

impl Parse for HybridSpec {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name: Option<syn::LitStr> = None;
        let mut expr: Option<syn::LitStr> = None;
        let mut ty: Option<syn::Type> = None;

        loop {
            let key: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            if key == "name" {
                name = Some(input.parse()?);
            } else if key == "expr" {
                expr = Some(input.parse()?);
            } else if key == "ty" {
                let lit: syn::LitStr = input.parse()?;
                ty = Some(lit.parse()?);
            } else {
                return Err(syn::Error::new_spanned(
                    &key,
                    "expected `name`, `expr`, or `ty`",
                ));
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }

        Ok(HybridSpec {
            name: name.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "#[hybrid(...)] requires `name = \"...\"`",
                )
            })?,
            expr: expr.ok_or_else(|| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    "#[hybrid(...)] requires `expr = \"...\"`",
                )
            })?,
            ty,
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
    let mut hybrids: Vec<HybridSpec> = Vec::new();
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
        } else if attr.path().is_ident("hybrid") {
            hybrids.push(attr.parse_args::<HybridSpec>()?);
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
        let mut default: Option<syn::LitStr> = None;

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
                } else if meta.path.is_ident("default") {
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    default = Some(lit);
                    Ok(())
                } else {
                    Err(meta.error(
                        "unsupported #[table(...)] field attribute; expected `column = \"...\"`, `primary_key`, `version`, `soft_delete`, or `default = \"...\"`",
                    ))
                }
            })?;
        }

        if default.is_some() && primary_key {
            return Err(syn::Error::new_spanned(
                &ident,
                "#[table(default = \"...\")] cannot be combined with #[table(primary_key)] — \
                 a primary key's value must always be supplied explicitly, not left for the \
                 mapping-level default to fill in",
            ));
        }

        fields.push(FieldInfo {
            ident,
            ty: field.ty.clone(),
            column,
            primary_key,
            version,
            soft_delete,
            default,
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
    let column_specs = fields.iter().map(|f| {
        let column = &f.column;
        let (column_type, nullable) = column_type_and_nullable(&core, &f.ty);
        quote! {
            #core::ColumnSpec {
                name: #column,
                ty: #column_type,
                nullable: #nullable,
            }
        }
    });
    let from_row_fields = fields.iter().map(|f| {
        let ident = &f.ident;
        let column = &f.column;
        quote! { #ident: row.get_by_name(#column)? }
    });
    let insert_calls = fields.iter().map(|f| {
        let ident = &f.ident;
        let column = &f.column;
        match &f.default {
            Some(default_lit) => quote! {
                .maybe_raw_value(#column, #default_lit, ::std::clone::Clone::clone(&self.#ident))
            },
            None => quote! {
                .value(#column, ::std::clone::Clone::clone(&self.#ident))
            },
        }
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
        .map(|spec| {
            let select_in = expand_has_many(struct_ident, &core, primary_key, spec)?;
            let subquery = expand_has_many_via_subquery(struct_ident, &core, primary_key, spec)?;
            let joined = expand_has_many_joined(struct_ident, &core, primary_key, spec)?;
            let joined_from_query =
                expand_has_many_joined_from_query(struct_ident, &core, primary_key, spec)?;
            Ok(quote! { #select_in #subquery #joined #joined_from_query })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let has_one_impls = has_one
        .iter()
        .map(|spec| {
            let select_in = expand_has_one(struct_ident, &core, primary_key, spec)?;
            let subquery = expand_has_one_via_subquery(struct_ident, &core, primary_key, spec)?;
            let joined = expand_has_one_joined(struct_ident, &core, primary_key, spec)?;
            let joined_from_query =
                expand_has_one_joined_from_query(struct_ident, &core, primary_key, spec)?;
            Ok(quote! { #select_in #subquery #joined #joined_from_query })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let belongs_to_impls = belongs_to
        .iter()
        .map(|spec| {
            let select_in = expand_belongs_to(struct_ident, &core, &fields, spec)?;
            let subquery = expand_belongs_to_via_subquery(struct_ident, &core, &fields, spec)?;
            let joined = expand_belongs_to_joined(struct_ident, &core, &fields, spec)?;
            let joined_from_query =
                expand_belongs_to_joined_from_query(struct_ident, &core, &fields, spec)?;
            Ok(quote! { #select_in #subquery #joined #joined_from_query })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let many_to_many_impls = many_to_many
        .iter()
        .map(|spec| {
            let select_in = expand_many_to_many(struct_ident, &core, primary_key, spec)?;
            let subquery =
                expand_many_to_many_via_subquery(struct_ident, &core, primary_key, spec)?;
            let joined = expand_many_to_many_joined(struct_ident, &core, primary_key, spec)?;
            let joined_from_query =
                expand_many_to_many_joined_from_query(struct_ident, &core, primary_key, spec)?;
            Ok(quote! { #select_in #subquery #joined #joined_from_query })
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let hybrid_impls = hybrids
        .iter()
        .map(|spec| expand_hybrid(struct_ident, &core, &fields, spec))
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
            const COLUMN_SPECS: &'static [#core::ColumnSpec] = &[#(#column_specs),*];
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
        #(#hybrid_impls)*
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

/// "subqueryload"-style alternative to `expand_has_many`'s generated
/// method: instead of a batch of already-fetched parents, takes a
/// `Select` picking out the parent primary key, and joins `Child` rows
/// against it directly (see
/// `rusty_db_core::relations::load_many_via_subquery`).
fn expand_has_many_via_subquery(
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
    let pk_column = &pk.column;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}s_via_subquery", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "subqueryload"-style eager load of the `#child` rows
            /// referencing the parents selected by `parent_ids`, which
            /// must select a single column named `#pk_column` — see
            /// `rusty_db_core::relations::load_many_via_subquery`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parent_ids: #core::Select,
            ) -> #core::Result<::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#child>>> {
                #core::relations::load_many_via_subquery::<#child, #pk_ty>(
                    engine,
                    parent_ids,
                    #pk_column,
                    #fk_column,
                )
                .await
            }
        }
    })
}

/// "joined"-strategy alternative to `expand_has_many`'s generated method:
/// fetches every matching `Self` row (via an optional `filter` on `Self`'s
/// own table) together with its `Child` rows in a single `LEFT JOIN` query
/// instead of a second round trip — see
/// `rusty_db_core::relations::load_many_joined`.
fn expand_has_many_joined(
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
    let pk_column = &pk.column;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}s_joined", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every matching `#struct_ident`
            /// row (`filter` on this type's own table, `None` for no filter)
            /// together with its `#child` rows, in a single `LEFT JOIN`
            /// query — see `rusty_db_core::relations::load_many_joined`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                filter: ::std::option::Option<#core::Expr>,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#child>>,
            )> {
                #core::relations::load_many_joined::<#struct_ident, #child, #pk_ty>(
                    engine,
                    filter,
                    #pk_column,
                    #fk_column,
                )
                .await
            }
        }
    })
}

/// Like `expand_has_many_joined`'s generated method, but instead of a
/// plain `filter`, takes an arbitrary `Select` on `Self`'s own table —
/// see `rusty_db_core::relations::load_many_joined_from_query`.
fn expand_has_many_joined_from_query(
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
    let pk_column = &pk.column;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}s_joined_from_query", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every `#struct_ident` row
            /// selected by `parents` (which must select every one of this
            /// type's own columns, each under its own column name) together
            /// with its `#child` rows, in a single `LEFT JOIN` query — see
            /// `rusty_db_core::relations::load_many_joined_from_query`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parents: #core::Select,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#child>>,
            )> {
                #core::relations::load_many_joined_from_query::<#struct_ident, #child, #pk_ty>(
                    engine,
                    parents,
                    #pk_column,
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

/// "subqueryload"-style alternative to `expand_has_one`'s generated method
/// — see `expand_has_many_via_subquery` for what `parent_ids` must select,
/// and `rusty_db_core::relations::load_has_one_via_subquery` for the same
/// one-to-one conflict check `expand_has_one` shares.
fn expand_has_one_via_subquery(
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
    let pk_column = &pk.column;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}_via_subquery", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "subqueryload"-style eager load of the single `#child` row
            /// (if any) referencing each parent selected by `parent_ids`,
            /// which must select a single column named `#pk_column` — see
            /// `rusty_db_core::relations::load_has_one_via_subquery`.
            /// `Err(Error::Conflict)` if more than one `#child` row
            /// references the same parent.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parent_ids: #core::Select,
            ) -> #core::Result<::std::collections::HashMap<#pk_ty, #child>> {
                #core::relations::load_has_one_via_subquery::<#child, #pk_ty>(
                    engine,
                    parent_ids,
                    #pk_column,
                    #fk_column,
                )
                .await
            }
        }
    })
}

/// "joined"-strategy alternative to `expand_has_one`'s generated method —
/// see `expand_has_many_joined` for what `filter` means, and
/// `rusty_db_core::relations::load_has_one_joined` for the same one-to-one
/// conflict check `expand_has_one` shares.
fn expand_has_one_joined(
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
    let pk_column = &pk.column;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}_joined", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every matching `#struct_ident`
            /// row (`filter` on this type's own table, `None` for no filter)
            /// together with the single `#child` row (if any) referencing
            /// it, in a single `LEFT JOIN` query. `Err(Error::Conflict)` if
            /// more than one `#child` row references the same parent — see
            /// `rusty_db_core::relations::load_has_one_joined`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                filter: ::std::option::Option<#core::Expr>,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#pk_ty, #child>,
            )> {
                #core::relations::load_has_one_joined::<#struct_ident, #child, #pk_ty>(
                    engine,
                    filter,
                    #pk_column,
                    #fk_column,
                )
                .await
            }
        }
    })
}

/// Like `expand_has_one_joined`'s generated method, but instead of a
/// plain `filter`, takes an arbitrary `Select` on `Self`'s own table —
/// see `rusty_db_core::relations::load_has_one_joined_from_query`.
fn expand_has_one_joined_from_query(
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
    let pk_column = &pk.column;
    let pk_ty = &pk.ty;
    let child = &spec.target;
    let fk_column = &spec.foreign_key;

    let child_name = child
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}_joined_from_query", child_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every `#struct_ident` row
            /// selected by `parents` (which must select every one of this
            /// type's own columns, each under its own column name) together
            /// with the single `#child` row (if any) referencing it, in a
            /// single `LEFT JOIN` query. `Err(Error::Conflict)` if more than
            /// one `#child` row references the same parent — see
            /// `rusty_db_core::relations::load_has_one_joined_from_query`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parents: #core::Select,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#pk_ty, #child>,
            )> {
                #core::relations::load_has_one_joined_from_query::<#struct_ident, #child, #pk_ty>(
                    engine,
                    parents,
                    #pk_column,
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

/// "subqueryload"-style alternative to `expand_many_to_many`'s generated
/// method — see `expand_has_many_via_subquery` for what `parent_ids` must
/// select, and
/// `rusty_db_core::relations::load_many_to_many_via_subquery` for the rest.
fn expand_many_to_many_via_subquery(
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
    let pk_column = &pk.column;
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
    let method_ident = format_ident!("load_{}s_via_subquery", target_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "subqueryload"-style eager load of every `#target` row
            /// related to the parents selected by `parent_ids`, which must
            /// select a single column named `#pk_column`, through the
            /// `#through` join table.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parent_ids: #core::Select,
            ) -> #core::Result<::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#target>>> {
                let target_key_column = <#target as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[many_to_many(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_many_to_many_via_subquery::<#target, #pk_ty>(
                    engine,
                    parent_ids,
                    #pk_column,
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

/// "joined"-strategy alternative to `expand_many_to_many`'s generated
/// method — see `expand_has_many_joined` for what `filter` means, and
/// `rusty_db_core::relations::load_many_to_many_joined` for the rest.
fn expand_many_to_many_joined(
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
    let pk_column = &pk.column;
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
    let method_ident = format_ident!("load_{}s_joined", target_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every matching `#struct_ident`
            /// row (`filter` on this type's own table, `None` for no filter)
            /// together with every `#target` row related to it through the
            /// `#through` join table, in a single two-hop `LEFT JOIN` query
            /// — see `rusty_db_core::relations::load_many_to_many_joined`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                filter: ::std::option::Option<#core::Expr>,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#target>>,
            )> {
                let target_key_column = <#target as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[many_to_many(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_many_to_many_joined::<#struct_ident, #target, #pk_ty>(
                    engine,
                    filter,
                    #pk_column,
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

/// Like `expand_many_to_many_joined`'s generated method, but instead of a
/// plain `filter`, takes an arbitrary `Select` on `Self`'s own table —
/// see `rusty_db_core::relations::load_many_to_many_joined_from_query`.
fn expand_many_to_many_joined_from_query(
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
    let pk_column = &pk.column;
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
    let method_ident = format_ident!("load_{}s_joined_from_query", target_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every `#struct_ident` row
            /// selected by `parents` (which must select every one of this
            /// type's own columns, each under its own column name) together
            /// with every `#target` row related to it through the
            /// `#through` join table, in a single two-hop `LEFT JOIN` query
            /// — see
            /// `rusty_db_core::relations::load_many_to_many_joined_from_query`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                parents: #core::Select,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#pk_ty, ::std::vec::Vec<#target>>,
            )> {
                let target_key_column = <#target as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[many_to_many(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_many_to_many_joined_from_query::<#struct_ident, #target, #pk_ty>(
                    engine,
                    parents,
                    #pk_column,
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

/// "subqueryload"-style alternative to `expand_belongs_to`'s generated
/// method: instead of a batch of already-fetched children, takes a
/// `Select` picking out the children's own `foreign_key` column, and joins
/// `Parent` rows against it directly (see
/// `rusty_db_core::relations::load_one_via_subquery`). Doesn't re-validate
/// `cascade` — `expand_belongs_to` already rejects it for this attribute,
/// and runs first (see the `?` chaining at the call site).
fn expand_belongs_to_via_subquery(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    fields: &[FieldInfo],
    spec: &RelationSpec,
) -> syn::Result<TokenStream2> {
    let fk_column_value = spec.foreign_key.value();
    let fk_field = fields.iter().find(|f| f.column == fk_column_value).ok_or_else(|| {
        syn::Error::new_spanned(
            &spec.foreign_key,
            format!("no field maps to column {fk_column_value:?}; #[belongs_to(...)]'s foreign_key must name a column on this struct"),
        )
    })?;
    let fk_ty = &fk_field.ty;
    let fk_column = &spec.foreign_key;
    let parent = &spec.target;

    let parent_name = parent
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}_via_subquery", parent_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "subqueryload"-style eager load of the `#parent` rows
            /// referenced by the children selected by `foreign_key_ids`,
            /// which must select a single column named `#fk_column` — see
            /// `rusty_db_core::relations::load_one_via_subquery`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                foreign_key_ids: #core::Select,
            ) -> #core::Result<::std::collections::HashMap<#fk_ty, #parent>> {
                let parent_key_column = <#parent as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[belongs_to(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_one_via_subquery::<#parent, #fk_ty>(
                    engine,
                    foreign_key_ids,
                    #fk_column,
                    parent_key_column,
                )
                .await
            }
        }
    })
}

/// "joined"-strategy alternative to `expand_belongs_to`'s generated method
/// — the deduplication direction flips relative to
/// `expand_has_many_joined`/`expand_has_one_joined` (see
/// `rusty_db_core::relations::load_one_joined`'s own doc for why). Doesn't
/// re-validate `cascade` — `expand_belongs_to` already rejects it for this
/// attribute, and runs first (see the `?` chaining at the call site).
fn expand_belongs_to_joined(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    fields: &[FieldInfo],
    spec: &RelationSpec,
) -> syn::Result<TokenStream2> {
    let fk_column_value = spec.foreign_key.value();
    let fk_field = fields.iter().find(|f| f.column == fk_column_value).ok_or_else(|| {
        syn::Error::new_spanned(
            &spec.foreign_key,
            format!("no field maps to column {fk_column_value:?}; #[belongs_to(...)]'s foreign_key must name a column on this struct"),
        )
    })?;
    let fk_ty = &fk_field.ty;
    let fk_column = &spec.foreign_key;
    let parent = &spec.target;

    let parent_name = parent
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}_joined", parent_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every matching `#struct_ident`
            /// row (`filter` on this type's own table, `None` for no filter)
            /// together with the `#parent` row it references, in a single
            /// `LEFT JOIN` query — see
            /// `rusty_db_core::relations::load_one_joined`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                filter: ::std::option::Option<#core::Expr>,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#fk_ty, #parent>,
            )> {
                let parent_key_column = <#parent as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[belongs_to(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_one_joined::<#struct_ident, #parent, #fk_ty>(
                    engine,
                    filter,
                    #fk_column,
                    parent_key_column,
                )
                .await
            }
        }
    })
}

/// Like `expand_belongs_to_joined`'s generated method, but instead of a
/// plain `filter`, takes an arbitrary `Select` on `Self`'s own table —
/// see `rusty_db_core::relations::load_one_joined_from_query`. Doesn't
/// re-validate `cascade` — `expand_belongs_to` already rejects it for
/// this attribute, and runs first (see the `?` chaining at the call site).
fn expand_belongs_to_joined_from_query(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    fields: &[FieldInfo],
    spec: &RelationSpec,
) -> syn::Result<TokenStream2> {
    let fk_column_value = spec.foreign_key.value();
    let fk_field = fields.iter().find(|f| f.column == fk_column_value).ok_or_else(|| {
        syn::Error::new_spanned(
            &spec.foreign_key,
            format!("no field maps to column {fk_column_value:?}; #[belongs_to(...)]'s foreign_key must name a column on this struct"),
        )
    })?;
    let fk_ty = &fk_field.ty;
    let fk_column = &spec.foreign_key;
    let parent = &spec.target;

    let parent_name = parent
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default();
    let method_ident = format_ident!("load_{}_joined_from_query", parent_name.to_snake_case());

    Ok(quote! {
        impl #struct_ident {
            /// "joined"-strategy eager load of every `#struct_ident` row
            /// selected by `children` (which must select every one of this
            /// type's own columns, each under its own column name) together
            /// with the `#parent` row it references, in a single
            /// `LEFT JOIN` query — see
            /// `rusty_db_core::relations::load_one_joined_from_query`.
            pub async fn #method_ident(
                engine: &#core::Engine,
                children: #core::Select,
            ) -> #core::Result<(
                ::std::vec::Vec<#struct_ident>,
                ::std::collections::HashMap<#fk_ty, #parent>,
            )> {
                let parent_key_column = <#parent as #core::Mapped>::PRIMARY_KEY.expect(
                    "#[belongs_to(...)] target must have a #[table(primary_key)] field",
                );
                #core::relations::load_one_joined_from_query::<#struct_ident, #parent, #fk_ty>(
                    engine,
                    children,
                    #fk_column,
                    parent_key_column,
                )
                .await
            }
        }
    })
}

/// A parsed `#[hybrid(...)]` expression token.
#[derive(Debug, Clone, PartialEq)]
enum HybridToken {
    Ident(String),
    Number(String),
    Str(String),
    Plus,
    Minus,
    Star,
    Slash,
    LParen,
    RParen,
    Comma,
    Lt,
    Le,
    Gt,
    Ge,
    EqEq,
    NotEq,
    AndAnd,
    OrOr,
}

fn tokenize_hybrid_expr(src: &str) -> Result<Vec<HybridToken>, String> {
    let mut tokens = Vec::new();
    let mut chars = src.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut ident = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_alphanumeric() || c == '_' {
                    ident.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(HybridToken::Ident(ident));
        } else if c.is_ascii_digit() {
            let mut number = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_ascii_digit() || c == '.' {
                    number.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(HybridToken::Number(number));
        } else if c == '"' {
            chars.next();
            let mut text = String::new();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some(c) => text.push(c),
                    None => {
                        return Err("unterminated string literal in hybrid expression".to_string())
                    }
                }
            }
            tokens.push(HybridToken::Str(text));
        } else {
            chars.next();
            let token = match c {
                '+' => HybridToken::Plus,
                '-' => HybridToken::Minus,
                '*' => HybridToken::Star,
                '/' => HybridToken::Slash,
                '(' => HybridToken::LParen,
                ')' => HybridToken::RParen,
                ',' => HybridToken::Comma,
                '<' => {
                    if chars.next_if_eq(&'=').is_some() {
                        HybridToken::Le
                    } else {
                        HybridToken::Lt
                    }
                }
                '>' => {
                    if chars.next_if_eq(&'=').is_some() {
                        HybridToken::Ge
                    } else {
                        HybridToken::Gt
                    }
                }
                '=' if chars.next_if_eq(&'=').is_some() => HybridToken::EqEq,
                '!' if chars.next_if_eq(&'=').is_some() => HybridToken::NotEq,
                '&' if chars.next_if_eq(&'&').is_some() => HybridToken::AndAnd,
                '|' if chars.next_if_eq(&'|').is_some() => HybridToken::OrOr,
                other => {
                    return Err(format!(
                        "unexpected character {other:?} in hybrid expression"
                    ))
                }
            };
            tokens.push(token);
        }
    }
    Ok(tokens)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum HybridOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// `<`/`<=`/`>`/`>=`/`==`/`!=` — always compares two arithmetic
/// sub-expressions and produces a `bool`. A comparison can appear only at
/// the top level of a hybrid expression or as a direct operand of
/// `HybridBoolOp` (optionally wrapped in its own parentheses to make a
/// `&&`/`||` grouping unambiguous) — never nested inside an arithmetic
/// sub-expression, another comparison, or a string function's argument
/// (enforced by `reject_nested_boolean`, since the grammar alone can't
/// rule it out once parentheses can hold a comparison at all). Chaining
/// two comparisons directly (`a < b < c`) is rejected too, the same
/// restriction Rust's own grammar puts on it — only `HybridBoolOp` can
/// combine more than one.
#[derive(Debug, Clone, Copy, PartialEq)]
enum HybridCompareOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// `&&`/`||`, combining two `bool`-producing sub-expressions (ordinarily
/// `HybridNode::Compare` or another `HybridBoolOp`) into a `bool`. `&&`
/// binds tighter than `||`, matching Rust's own precedence. Either operand
/// can be parenthesized as its own group (`(price > 50) && (qty > 1)`,
/// or a fully parenthesized chain like `(price > 50 || price < 5) &&
/// qty > 1`) — same as `HybridCompareOp`, this is only ever valid at the
/// top level or as a `BoolOp` operand, never nested inside arithmetic,
/// a comparison, or a string function's argument.
#[derive(Debug, Clone, Copy, PartialEq)]
enum HybridBoolOp {
    And,
    Or,
}

/// The parsed shape of a `#[hybrid(expr = "...")]` string: a small
/// expression tree over field names, integer/float/string literals, `+`/
/// `-`/`*`/`/`, `upper(x)`/`lower(x)`/`concat(a, b)` string functions,
/// parentheses for grouping, and — at the top level, or nested inside a
/// parenthesized `&&`/`||` group — a `<`/`<=`/`>`/`>=`/`==`/`!=` comparison
/// of two such arithmetic sub-expressions, optionally chained/grouped
/// with `&&`/`||` into more than one. Deliberately nothing richer still
/// (no `CASE`/`COALESCE`, or references to a joined table's columns) —
/// `CASE`/`COALESCE` in particular are skipped because they fundamentally
/// operate on NULL-able SQL values, which map to `Option<T>` Rust fields,
/// but this design's arithmetic operators implicitly assume plain,
/// non-`Option` field types (there's no natural "first non-null" analog
/// once a value might not be an `Option`). String functions map cleanly
/// to Rust's `.to_uppercase()`/`.to_lowercase()`/`format!()` and the query
/// builder's `Expr::upper()`/`Expr::lower()`/`Expr::concat()` without
/// touching nullability at all, so they're supported while
/// `CASE`/`COALESCE` aren't.
#[derive(Debug, Clone)]
enum HybridNode {
    Field(String),
    IntLit(i64),
    FloatLit(f64),
    StringLit(String),
    Op(Box<HybridNode>, HybridOp, Box<HybridNode>),
    Compare(Box<HybridNode>, HybridCompareOp, Box<HybridNode>),
    BoolOp(Box<HybridNode>, HybridBoolOp, Box<HybridNode>),
    Upper(Box<HybridNode>),
    Lower(Box<HybridNode>),
    Concat(Box<HybridNode>, Box<HybridNode>),
}

struct HybridParser<'a> {
    tokens: &'a [HybridToken],
    pos: usize,
}

impl<'a> HybridParser<'a> {
    fn parse(tokens: &'a [HybridToken]) -> Result<HybridNode, String> {
        let mut parser = HybridParser { tokens, pos: 0 };
        let node = parser.parse_or()?;
        if parser.pos != parser.tokens.len() {
            return Err("unexpected trailing tokens in hybrid expression".to_string());
        }
        Ok(node)
    }

    /// The true top-level entry point: one or more `parse_and` operands
    /// joined by `||`, left-associative.
    fn parse_or(&mut self) -> Result<HybridNode, String> {
        let mut node = self.parse_and()?;
        while matches!(self.tokens.get(self.pos), Some(HybridToken::OrOr)) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            node = HybridNode::BoolOp(Box::new(node), HybridBoolOp::Or, Box::new(rhs));
        }
        Ok(node)
    }

    /// One or more `parse_comparison` operands joined by `&&`,
    /// left-associative — binds tighter than `||` since it's the level
    /// `parse_or` calls into, not the other way around.
    fn parse_and(&mut self) -> Result<HybridNode, String> {
        let mut node = self.parse_comparison()?;
        while matches!(self.tokens.get(self.pos), Some(HybridToken::AndAnd)) {
            self.pos += 1;
            let rhs = self.parse_comparison()?;
            node = HybridNode::BoolOp(Box::new(node), HybridBoolOp::And, Box::new(rhs));
        }
        Ok(node)
    }

    /// An arithmetic expression, optionally followed by exactly one
    /// comparison operator and a second arithmetic expression. Never
    /// recurses back into itself directly, so a comparison can only ever
    /// appear once per `&&`/`||` operand from the grammar alone — though a
    /// parenthesized comparison/boolean group can still smuggle one in via
    /// `parse_expr`/`parse_factor`'s own `LParen` arm; `reject_nested_boolean`
    /// is what actually rules that out (see `HybridCompareOp`'s own doc).
    fn parse_comparison(&mut self) -> Result<HybridNode, String> {
        let lhs = self.parse_expr()?;
        let op = match self.tokens.get(self.pos) {
            Some(HybridToken::Lt) => HybridCompareOp::Lt,
            Some(HybridToken::Le) => HybridCompareOp::Le,
            Some(HybridToken::Gt) => HybridCompareOp::Gt,
            Some(HybridToken::Ge) => HybridCompareOp::Ge,
            Some(HybridToken::EqEq) => HybridCompareOp::Eq,
            Some(HybridToken::NotEq) => HybridCompareOp::Ne,
            _ => return Ok(lhs),
        };
        self.pos += 1;
        let rhs = self.parse_expr()?;
        Ok(HybridNode::Compare(Box::new(lhs), op, Box::new(rhs)))
    }

    fn parse_expr(&mut self) -> Result<HybridNode, String> {
        let mut node = self.parse_term()?;
        loop {
            match self.tokens.get(self.pos) {
                Some(HybridToken::Plus) => {
                    self.pos += 1;
                    let rhs = self.parse_term()?;
                    node = HybridNode::Op(Box::new(node), HybridOp::Add, Box::new(rhs));
                }
                Some(HybridToken::Minus) => {
                    self.pos += 1;
                    let rhs = self.parse_term()?;
                    node = HybridNode::Op(Box::new(node), HybridOp::Sub, Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(node)
    }

    fn parse_term(&mut self) -> Result<HybridNode, String> {
        let mut node = self.parse_factor()?;
        loop {
            match self.tokens.get(self.pos) {
                Some(HybridToken::Star) => {
                    self.pos += 1;
                    let rhs = self.parse_factor()?;
                    node = HybridNode::Op(Box::new(node), HybridOp::Mul, Box::new(rhs));
                }
                Some(HybridToken::Slash) => {
                    self.pos += 1;
                    let rhs = self.parse_factor()?;
                    node = HybridNode::Op(Box::new(node), HybridOp::Div, Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(node)
    }

    fn parse_factor(&mut self) -> Result<HybridNode, String> {
        match self.tokens.get(self.pos) {
            Some(HybridToken::Ident(name)) => {
                let name = name.clone();
                self.pos += 1;
                if matches!(self.tokens.get(self.pos), Some(HybridToken::LParen)) {
                    self.parse_call(&name)
                } else {
                    Ok(HybridNode::Field(name))
                }
            }
            Some(HybridToken::Number(text)) => {
                let text = text.clone();
                self.pos += 1;
                if text.contains('.') {
                    text.parse::<f64>()
                        .map(HybridNode::FloatLit)
                        .map_err(|_| format!("invalid number {text:?} in hybrid expression"))
                } else {
                    text.parse::<i64>()
                        .map(HybridNode::IntLit)
                        .map_err(|_| format!("invalid number {text:?} in hybrid expression"))
                }
            }
            Some(HybridToken::Str(text)) => {
                let text = text.clone();
                self.pos += 1;
                Ok(HybridNode::StringLit(text))
            }
            Some(HybridToken::LParen) => {
                self.pos += 1;
                // Parses at the top of the precedence chain (`parse_or`),
                // not just `parse_expr` (arithmetic) — this is what lets a
                // parenthesized group hold a comparison or `&&`/`||` chain,
                // not only arithmetic. A group containing pure arithmetic
                // still round-trips through unaffected, since `parse_or`
                // falls straight back down to `parse_expr` when no
                // comparison/boolean operator follows.
                let node = self.parse_or()?;
                match self.tokens.get(self.pos) {
                    Some(HybridToken::RParen) => {
                        self.pos += 1;
                        Ok(node)
                    }
                    _ => Err("expected closing `)` in hybrid expression".to_string()),
                }
            }
            other => Err(format!(
                "expected a field name, number, string, or `(` in hybrid expression, found {other:?}"
            )),
        }
    }

    /// Parses `name(arg, arg, ...)` — the `(` after `name` has already been
    /// peeked but not yet consumed. Each argument is parsed at `parse_expr`
    /// level (arithmetic, no comparisons/booleans), matching what makes
    /// sense inside `upper`/`lower`/`concat`. Rejects unknown function
    /// names and wrong argument counts eagerly, rather than deferring to
    /// codegen.
    fn parse_call(&mut self, name: &str) -> Result<HybridNode, String> {
        self.pos += 1; // consume `(`
        let mut args = Vec::new();
        if !matches!(self.tokens.get(self.pos), Some(HybridToken::RParen)) {
            loop {
                args.push(self.parse_expr()?);
                match self.tokens.get(self.pos) {
                    Some(HybridToken::Comma) => {
                        self.pos += 1;
                    }
                    _ => break,
                }
            }
        }
        match self.tokens.get(self.pos) {
            Some(HybridToken::RParen) => self.pos += 1,
            _ => {
                return Err(format!(
                    "expected closing `)` after `{name}(...)` arguments"
                ))
            }
        }

        match (name, args.len()) {
            ("upper", 1) => Ok(HybridNode::Upper(Box::new(args.remove(0)))),
            ("lower", 1) => Ok(HybridNode::Lower(Box::new(args.remove(0)))),
            ("concat", 2) => {
                let rhs = args.remove(1);
                let lhs = args.remove(0);
                Ok(HybridNode::Concat(Box::new(lhs), Box::new(rhs)))
            }
            ("upper" | "lower", n) => Err(format!(
                "`{name}(...)` takes exactly 1 argument in a hybrid expression, found {n}"
            )),
            ("concat", n) => Err(format!(
                "`concat(...)` takes exactly 2 arguments in a hybrid expression, found {n}"
            )),
            (other, _) => Err(format!(
                "unknown function `{other}` in hybrid expression; expected `upper`, `lower`, or `concat`"
            )),
        }
    }
}

fn parse_hybrid_expr(src: &str) -> Result<HybridNode, String> {
    let tokens = tokenize_hybrid_expr(src)?;
    let node = HybridParser::parse(&tokens)?;
    reject_nested_boolean(&node, true)?;
    Ok(node)
}

/// Rejects a `Compare`/`BoolOp` node anywhere except the top level of the
/// whole expression or as a direct operand of another `BoolOp` — i.e. a
/// parenthesized comparison/boolean group (`(price > 50) && (qty > 1)`)
/// is fine, but nesting one as an arithmetic operand, a comparison's own
/// operand, or a string function's argument (`(price > 50) + 1`,
/// `(price > 50) > 2`, `upper((price > 50))`) isn't. Only the parenthesized
/// grammar in `parse_factor`'s `LParen` arm can produce this shape at all
/// — before it called `parse_or` (to support the parenthesized group in
/// the first place) it was structurally impossible, since `parse_expr`/
/// `parse_term`/`parse_factor` never produced a `Compare`/`BoolOp` node on
/// their own. The reason this needs rejecting explicitly rather than left
/// to just fail at compile time: the query builder's `Expr` has no type
/// distinguishing "arithmetic" from "boolean" — `(a > 1) + 5` would still
/// build a syntactically valid `Expr` tree (`Expr::lt_expr(...).add(...)`)
/// even though the Rust side hard-fails to compile (`bool + i32`), which
/// is exactly the kind of Rust-side/SQL-side disagreement this design is
/// meant to rule out everywhere else.
fn reject_nested_boolean(node: &HybridNode, top_level: bool) -> Result<(), String> {
    match node {
        HybridNode::Field(_)
        | HybridNode::IntLit(_)
        | HybridNode::FloatLit(_)
        | HybridNode::StringLit(_) => Ok(()),
        HybridNode::Op(lhs, _, rhs) | HybridNode::Concat(lhs, rhs) => {
            reject_nested_boolean(lhs, false)?;
            reject_nested_boolean(rhs, false)
        }
        HybridNode::Upper(inner) | HybridNode::Lower(inner) => reject_nested_boolean(inner, false),
        HybridNode::Compare(lhs, _, rhs) => {
            if !top_level {
                return Err(
                    "a comparison can only appear at the top level of a hybrid expression or \
                     as an operand of `&&`/`||`, not nested inside arithmetic, another \
                     comparison, or a string function's argument"
                        .to_string(),
                );
            }
            reject_nested_boolean(lhs, false)?;
            reject_nested_boolean(rhs, false)
        }
        HybridNode::BoolOp(lhs, _, rhs) => {
            if !top_level {
                return Err(
                    "a `&&`/`||` combinator can only appear at the top level of a hybrid \
                     expression, not nested inside arithmetic, a comparison, or a string \
                     function's argument"
                        .to_string(),
                );
            }
            reject_nested_boolean(lhs, true)?;
            reject_nested_boolean(rhs, true)
        }
    }
}

/// Every field name a hybrid expression tree references, in the order
/// they first appear (left to right) — used both to validate every name
/// against the struct's own fields and to infer a return type from the
/// first one when `ty` isn't given explicitly.
fn hybrid_field_names(node: &HybridNode) -> Vec<String> {
    match node {
        HybridNode::Field(name) => vec![name.clone()],
        HybridNode::IntLit(_) | HybridNode::FloatLit(_) | HybridNode::StringLit(_) => Vec::new(),
        HybridNode::Op(lhs, _, rhs)
        | HybridNode::Compare(lhs, _, rhs)
        | HybridNode::BoolOp(lhs, _, rhs)
        | HybridNode::Concat(lhs, rhs) => {
            let mut names = hybrid_field_names(lhs);
            names.extend(hybrid_field_names(rhs));
            names
        }
        HybridNode::Upper(inner) | HybridNode::Lower(inner) => hybrid_field_names(inner),
    }
}

/// The Rust-side computation (`self.field <op> self.other_field`).
/// Numeric literals are emitted unsuffixed so they adapt to whatever
/// numeric type the referenced fields turn out to be, rather than forcing
/// `i64`/`f64` regardless of the struct's own field types.
fn hybrid_rust_tokens(node: &HybridNode, fields: &[FieldInfo]) -> TokenStream2 {
    match node {
        HybridNode::Field(name) => {
            let field = fields
                .iter()
                .find(|f| f.ident == name)
                .expect("field name already validated against the struct's own fields");
            let ident = &field.ident;
            quote! { (self.#ident) }
        }
        HybridNode::IntLit(n) => {
            let lit = proc_macro2::Literal::i64_unsuffixed(*n);
            quote! { (#lit) }
        }
        HybridNode::FloatLit(n) => {
            let lit = proc_macro2::Literal::f64_unsuffixed(*n);
            quote! { (#lit) }
        }
        HybridNode::StringLit(s) => quote! { (#s.to_string()) },
        HybridNode::Op(lhs, op, rhs) => {
            let lhs = hybrid_rust_tokens(lhs, fields);
            let rhs = hybrid_rust_tokens(rhs, fields);
            match op {
                HybridOp::Add => quote! { (#lhs + #rhs) },
                HybridOp::Sub => quote! { (#lhs - #rhs) },
                HybridOp::Mul => quote! { (#lhs * #rhs) },
                HybridOp::Div => quote! { (#lhs / #rhs) },
            }
        }
        HybridNode::Compare(lhs, op, rhs) => {
            let lhs = hybrid_rust_tokens(lhs, fields);
            let rhs = hybrid_rust_tokens(rhs, fields);
            match op {
                HybridCompareOp::Lt => quote! { (#lhs < #rhs) },
                HybridCompareOp::Le => quote! { (#lhs <= #rhs) },
                HybridCompareOp::Gt => quote! { (#lhs > #rhs) },
                HybridCompareOp::Ge => quote! { (#lhs >= #rhs) },
                HybridCompareOp::Eq => quote! { (#lhs == #rhs) },
                HybridCompareOp::Ne => quote! { (#lhs != #rhs) },
            }
        }
        HybridNode::BoolOp(lhs, op, rhs) => {
            let lhs = hybrid_rust_tokens(lhs, fields);
            let rhs = hybrid_rust_tokens(rhs, fields);
            match op {
                HybridBoolOp::And => quote! { (#lhs && #rhs) },
                HybridBoolOp::Or => quote! { (#lhs || #rhs) },
            }
        }
        HybridNode::Upper(inner) => {
            let inner = hybrid_rust_tokens(inner, fields);
            quote! { (#inner.to_uppercase()) }
        }
        HybridNode::Lower(inner) => {
            let inner = hybrid_rust_tokens(inner, fields);
            quote! { (#inner.to_lowercase()) }
        }
        HybridNode::Concat(lhs, rhs) => {
            let lhs = hybrid_rust_tokens(lhs, fields);
            let rhs = hybrid_rust_tokens(rhs, fields);
            quote! { (format!("{}{}", #lhs, #rhs)) }
        }
    }
}

/// The SQL-side equivalent, built from the same tree via `Expr::col`/
/// `Expr::lit`/`Expr::add`/`sub`/`mul`/`div` — the same public
/// constructors any hand-written query would use, so this renders
/// correctly on every dialect the query builder already supports.
fn hybrid_sql_tokens(node: &HybridNode, core: &TokenStream2, fields: &[FieldInfo]) -> TokenStream2 {
    match node {
        HybridNode::Field(name) => {
            let field = fields
                .iter()
                .find(|f| f.ident == name)
                .expect("field name already validated against the struct's own fields");
            let column = &field.column;
            quote! { #core::Expr::col(Self::table().col(#column)) }
        }
        HybridNode::IntLit(n) => quote! { #core::Expr::lit(#n) },
        HybridNode::FloatLit(n) => quote! { #core::Expr::lit(#n) },
        HybridNode::StringLit(s) => quote! { #core::Expr::lit(#s) },
        HybridNode::Op(lhs, op, rhs) => {
            let lhs = hybrid_sql_tokens(lhs, core, fields);
            let rhs = hybrid_sql_tokens(rhs, core, fields);
            match op {
                HybridOp::Add => quote! { (#lhs).add(#rhs) },
                HybridOp::Sub => quote! { (#lhs).sub(#rhs) },
                HybridOp::Mul => quote! { (#lhs).mul(#rhs) },
                HybridOp::Div => quote! { (#lhs).div(#rhs) },
            }
        }
        HybridNode::Compare(lhs, op, rhs) => {
            let lhs = hybrid_sql_tokens(lhs, core, fields);
            let rhs = hybrid_sql_tokens(rhs, core, fields);
            match op {
                HybridCompareOp::Lt => quote! { (#lhs).lt_expr(#rhs) },
                HybridCompareOp::Le => quote! { (#lhs).lte_expr(#rhs) },
                HybridCompareOp::Gt => quote! { (#lhs).gt_expr(#rhs) },
                HybridCompareOp::Ge => quote! { (#lhs).gte_expr(#rhs) },
                HybridCompareOp::Eq => quote! { (#lhs).eq_expr(#rhs) },
                HybridCompareOp::Ne => quote! { (#lhs).ne_expr(#rhs) },
            }
        }
        HybridNode::BoolOp(lhs, op, rhs) => {
            let lhs = hybrid_sql_tokens(lhs, core, fields);
            let rhs = hybrid_sql_tokens(rhs, core, fields);
            match op {
                HybridBoolOp::And => quote! { (#lhs).and(#rhs) },
                HybridBoolOp::Or => quote! { (#lhs).or(#rhs) },
            }
        }
        HybridNode::Upper(inner) => {
            let inner = hybrid_sql_tokens(inner, core, fields);
            quote! { (#inner).upper() }
        }
        HybridNode::Lower(inner) => {
            let inner = hybrid_sql_tokens(inner, core, fields);
            quote! { (#inner).lower() }
        }
        HybridNode::Concat(lhs, rhs) => {
            let lhs = hybrid_sql_tokens(lhs, core, fields);
            let rhs = hybrid_sql_tokens(rhs, core, fields);
            quote! { (#lhs).concat(#rhs) }
        }
    }
}

/// `#[hybrid(name = "total", expr = "price * quantity")]` generates both
/// halves SQLAlchemy's single `@hybrid_property` method splits into: a
/// plain `fn total(&self) -> T` computing the value from this instance's
/// own fields, and `fn total_expr() -> Expr` — the same computation as a
/// portable SQL expression, usable in `.filter()`/`.columns()` (anywhere
/// an `Expr` is accepted; not `.order_by()`, which only accepts a bare
/// `Column` today). The two are generated from one parsed expression tree
/// so they can't drift apart from each other, though nothing checks the
/// expression string itself is correct SQL until it actually runs.
fn expand_hybrid(
    struct_ident: &syn::Ident,
    core: &TokenStream2,
    fields: &[FieldInfo],
    spec: &HybridSpec,
) -> syn::Result<TokenStream2> {
    let node = parse_hybrid_expr(&spec.expr.value())
        .map_err(|msg| syn::Error::new_spanned(&spec.expr, msg))?;

    let referenced = hybrid_field_names(&node);
    if referenced.is_empty() {
        return Err(syn::Error::new_spanned(
            &spec.expr,
            "a #[hybrid(...)] expression must reference at least one field",
        ));
    }
    for name in &referenced {
        if !fields.iter().any(|f| f.ident == name) {
            return Err(syn::Error::new_spanned(
                &spec.expr,
                format!("#[hybrid(...)] expression references unknown field `{name}`"),
            ));
        }
    }

    let name = spec.name.value();
    if fields.iter().any(|f| f.ident == name) {
        return Err(syn::Error::new_spanned(
            &spec.name,
            format!(
                "#[hybrid(name = \"{name}\")] collides with an existing field of the same name"
            ),
        ));
    }
    let method_ident = syn::Ident::new(&name, spec.name.span());
    let expr_method_ident = format_ident!("{}_expr", name);

    let ty = match &spec.ty {
        Some(ty) => ty.clone(),
        None if matches!(node, HybridNode::Compare(..) | HybridNode::BoolOp(..)) => {
            syn::parse_quote!(bool)
        }
        None if matches!(
            node,
            HybridNode::Upper(..)
                | HybridNode::Lower(..)
                | HybridNode::Concat(..)
                | HybridNode::StringLit(..)
        ) =>
        {
            syn::parse_quote!(String)
        }
        None => {
            let first = referenced.first().expect("checked non-empty above");
            fields
                .iter()
                .find(|f| f.ident == first)
                .expect("already validated")
                .ty
                .clone()
        }
    };

    let rust_expr = hybrid_rust_tokens(&node, fields);
    let sql_expr = hybrid_sql_tokens(&node, core, fields);

    Ok(quote! {
        impl #struct_ident {
            /// Computed from this instance's own fields — see
            /// `#expr_method_ident` for the SQL-side equivalent, usable in
            /// a filter/query instead.
            pub fn #method_ident(&self) -> #ty {
                #rust_expr
            }

            /// The SQL expression computing the same value as
            /// `#method_ident`, for use in `.filter()`/`.columns()` (or
            /// anywhere else an `Expr` is accepted).
            pub fn #expr_method_ident() -> #core::Expr {
                #sql_expr
            }
        }
    })
}

/// A single unit variant's own mapped text form (default: its snake_case
/// name, overridable per-variant with `#[mapped_enum(rename = "...")]`).
struct EnumVariantInfo {
    ident: syn::Ident,
    text: String,
}

/// `#[derive(MappedEnum)]`: maps a fieldless enum onto a single `Value`
/// (`Value::Text` by default, one variant's snake_case name per case;
/// `Value::I64` — each variant's own discriminant — if the enum carries
/// `#[mapped_enum(as_int)]`), so it can be used directly as a
/// `#[derive(Mapped)]` field type.
fn expand_mapped_enum(input: DeriveInput) -> syn::Result<TokenStream2> {
    let enum_ident = &input.ident;

    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(MappedEnum)] only supports enums",
        ));
    };

    let mut as_int = false;
    for attr in &input.attrs {
        if attr.path().is_ident("mapped_enum") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("as_int") {
                    as_int = true;
                    Ok(())
                } else {
                    Err(meta.error("unsupported #[mapped_enum(...)] attribute; expected `as_int`"))
                }
            })?;
        }
    }

    let mut variants = Vec::with_capacity(data.variants.len());
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                &variant.ident,
                "#[derive(MappedEnum)] only supports fieldless (unit) variants",
            ));
        }

        let mut text = variant.ident.to_string().to_snake_case();
        for attr in &variant.attrs {
            if attr.path().is_ident("mapped_enum") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("rename") {
                        let lit: syn::LitStr = meta.value()?.parse()?;
                        text = lit.value();
                        Ok(())
                    } else {
                        Err(meta.error(
                            "unsupported #[mapped_enum(...)] variant attribute; expected \
                             `rename = \"...\"`",
                        ))
                    }
                })?;
            }
        }

        variants.push(EnumVariantInfo {
            ident: variant.ident.clone(),
            text,
        });
    }

    let core = core_crate_path();

    let (from_impl, from_value_impl) = if as_int {
        let to_int_arms = variants.iter().map(|v| {
            let ident = &v.ident;
            quote! { #enum_ident::#ident => #enum_ident::#ident as i64 }
        });
        let from_int_checks = variants.iter().map(|v| {
            let ident = &v.ident;
            quote! {
                if *i == (#enum_ident::#ident as i64) {
                    return ::std::result::Result::Ok(#enum_ident::#ident);
                }
            }
        });

        (
            quote! {
                impl ::std::convert::From<#enum_ident> for #core::Value {
                    fn from(v: #enum_ident) -> Self {
                        #core::Value::I64(match v { #(#to_int_arms),* })
                    }
                }
            },
            quote! {
                impl #core::FromValue for #enum_ident {
                    fn from_value(value: &#core::Value) -> ::std::result::Result<Self, ::std::string::String> {
                        match value {
                            #core::Value::I64(i) => {
                                #(#from_int_checks)*
                                ::std::result::Result::Err(::std::format!(
                                    "unknown {} discriminant: {i}",
                                    ::std::stringify!(#enum_ident),
                                ))
                            }
                            other => ::std::result::Result::Err(::std::format!(
                                "expected an integer for {}, got {other}",
                                ::std::stringify!(#enum_ident),
                            )),
                        }
                    }
                }
            },
        )
    } else {
        let to_text_arms = variants.iter().map(|v| {
            let ident = &v.ident;
            let text = &v.text;
            quote! { #enum_ident::#ident => #text }
        });
        let from_text_arms = variants.iter().map(|v| {
            let ident = &v.ident;
            let text = &v.text;
            quote! { #text => ::std::result::Result::Ok(#enum_ident::#ident) }
        });

        (
            quote! {
                impl ::std::convert::From<#enum_ident> for #core::Value {
                    fn from(v: #enum_ident) -> Self {
                        #core::Value::Text(match v { #(#to_text_arms),* }.to_string())
                    }
                }
            },
            quote! {
                impl #core::FromValue for #enum_ident {
                    fn from_value(value: &#core::Value) -> ::std::result::Result<Self, ::std::string::String> {
                        match value {
                            #core::Value::Text(s) => match s.as_str() {
                                #(#from_text_arms,)*
                                other => ::std::result::Result::Err(::std::format!(
                                    "unknown {} variant: {other:?}",
                                    ::std::stringify!(#enum_ident),
                                )),
                            },
                            other => ::std::result::Result::Err(::std::format!(
                                "expected text for {}, got {other}",
                                ::std::stringify!(#enum_ident),
                            )),
                        }
                    }
                }
            },
        )
    };

    Ok(quote! {
        #from_impl
        #from_value_impl
    })
}

/// `#[derive(MappedNewtype)]`: maps a single-field tuple struct onto
/// whatever `Value` its own field already converts to/from, delegating
/// straight through — so it can be used directly as a `#[derive(Mapped)]`
/// field type.
fn expand_mapped_newtype(input: DeriveInput) -> syn::Result<TokenStream2> {
    let ident = &input.ident;

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(MappedNewtype)] only supports tuple structs with exactly one field",
        ));
    };
    let Fields::Unnamed(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(MappedNewtype)] only supports tuple structs with exactly one field",
        ));
    };
    if fields.unnamed.len() != 1 {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(MappedNewtype)] only supports tuple structs with exactly one field",
        ));
    }

    let core = core_crate_path();

    Ok(quote! {
        impl ::std::convert::From<#ident> for #core::Value {
            fn from(v: #ident) -> Self {
                #core::Value::from(v.0)
            }
        }

        impl #core::FromValue for #ident {
            fn from_value(value: &#core::Value) -> ::std::result::Result<Self, ::std::string::String> {
                ::std::result::Result::Ok(#ident(<_ as #core::FromValue>::from_value(value)?))
            }
        }
    })
}

/// Infers `(ColumnType tokens, nullable)` for a `#[derive(Mapped)]` field's
/// own Rust type, for the `Mapped::COLUMN_SPECS` const schema autogenerate
/// diffs against a live database. Unwraps `Option<T>` for nullability,
/// then matches `T`'s own last path segment against every field type this
/// derive macro documents support for (see the module doc comment).
/// Anything unrecognized — a `#[derive(MappedEnum)]`/`#[derive(MappedNewtype)]`
/// custom type, a hand-written `Into<Value>` impl, or a `Vec<T>` array of
/// anything but `u8` (no portable array column type exists yet) — falls
/// back to `ColumnType::Text`, mirroring `automap::rust_type_for`'s own
/// fallback-to-`String` policy in the reverse direction. `BigDecimal`'s
/// precision/scale is a guess (`38, 10`) since Rust's own `BigDecimal`
/// carries no static precision/scale to read at macro-expansion time.
fn column_type_and_nullable(core: &TokenStream2, ty: &syn::Type) -> (TokenStream2, bool) {
    if let Some(inner) = option_inner_type(ty) {
        let (tokens, _) = column_type_and_nullable(core, inner);
        return (tokens, true);
    }
    (column_type_tokens(core, ty), false)
}

fn option_inner_type(ty: &syn::Type) -> Option<&syn::Type> {
    let syn::Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    args.args.iter().find_map(|arg| match arg {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    })
}

fn column_type_tokens(core: &TokenStream2, ty: &syn::Type) -> TokenStream2 {
    match type_last_segment_ident(ty).as_deref() {
        Some("bool") => quote! { #core::ColumnType::Bool },
        Some("i64") | Some("i32") => quote! { #core::ColumnType::I64 },
        Some("f64") | Some("f32") => quote! { #core::ColumnType::F64 },
        Some("String") => quote! { #core::ColumnType::Text },
        Some("Uuid") => quote! { #core::ColumnType::Uuid },
        Some("BigDecimal") => quote! { #core::ColumnType::Decimal { precision: 38, scale: 10 } },
        Some("Json") => quote! { #core::ColumnType::Json },
        Some("NaiveDate") => quote! { #core::ColumnType::Date },
        Some("NaiveTime") => quote! { #core::ColumnType::Time },
        Some("NaiveDateTime") => quote! { #core::ColumnType::DateTime },
        Some("DateTime") => quote! { #core::ColumnType::TimestampTz },
        Some("Vec") if is_vec_u8(ty) => quote! { #core::ColumnType::Bytes },
        _ => quote! { #core::ColumnType::Text },
    }
}

fn type_last_segment_ident(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(type_path) = ty else {
        return None;
    };
    type_path.path.segments.last().map(|s| s.ident.to_string())
}

fn is_vec_u8(ty: &syn::Type) -> bool {
    let syn::Type::Path(type_path) = ty else {
        return false;
    };
    let Some(segment) = type_path.path.segments.last() else {
        return false;
    };
    if segment.ident != "Vec" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return false;
    };
    args.args.iter().any(|arg| {
        matches!(arg, syn::GenericArgument::Type(syn::Type::Path(p)) if p.path.is_ident("u8"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ty(src: &str) -> syn::Type {
        syn::parse_str(src).unwrap()
    }

    fn infer(src: &str) -> (String, bool) {
        let core = quote! { rusty_db_core };
        let (tokens, nullable) = column_type_and_nullable(&core, &parse_ty(src));
        (tokens.to_string(), nullable)
    }

    #[test]
    fn infers_every_documented_scalar_field_type() {
        assert_eq!(
            infer("bool"),
            ("rusty_db_core :: ColumnType :: Bool".to_string(), false)
        );
        assert_eq!(
            infer("i64"),
            ("rusty_db_core :: ColumnType :: I64".to_string(), false)
        );
        assert_eq!(
            infer("i32"),
            ("rusty_db_core :: ColumnType :: I64".to_string(), false)
        );
        assert_eq!(
            infer("f64"),
            ("rusty_db_core :: ColumnType :: F64".to_string(), false)
        );
        assert_eq!(
            infer("String"),
            ("rusty_db_core :: ColumnType :: Text".to_string(), false)
        );
        assert_eq!(
            infer("Uuid"),
            ("rusty_db_core :: ColumnType :: Uuid".to_string(), false)
        );
        assert_eq!(
            infer("Json"),
            ("rusty_db_core :: ColumnType :: Json".to_string(), false)
        );
        assert_eq!(
            infer("NaiveDate"),
            ("rusty_db_core :: ColumnType :: Date".to_string(), false)
        );
        assert_eq!(
            infer("NaiveTime"),
            ("rusty_db_core :: ColumnType :: Time".to_string(), false)
        );
        assert_eq!(
            infer("NaiveDateTime"),
            ("rusty_db_core :: ColumnType :: DateTime".to_string(), false)
        );
        assert_eq!(
            infer("DateTime<Utc>"),
            (
                "rusty_db_core :: ColumnType :: TimestampTz".to_string(),
                false
            )
        );
        assert_eq!(
            infer("Vec<u8>"),
            ("rusty_db_core :: ColumnType :: Bytes".to_string(), false)
        );
    }

    #[test]
    fn bigdecimal_gets_a_default_precision_and_scale() {
        let (tokens, _) = infer("BigDecimal");
        assert_eq!(
            tokens,
            "rusty_db_core :: ColumnType :: Decimal { precision : 38 , scale : 10 }"
        );
    }

    #[test]
    fn option_wrapping_marks_nullable_but_keeps_the_inner_types_mapping() {
        let (tokens, nullable) = infer("Option<String>");
        assert_eq!(tokens, "rusty_db_core :: ColumnType :: Text");
        assert!(nullable);

        let (tokens, nullable) = infer("Option<i64>");
        assert_eq!(tokens, "rusty_db_core :: ColumnType :: I64");
        assert!(nullable);
    }

    #[test]
    fn a_vec_of_anything_but_u8_falls_back_to_text_no_portable_array_type_yet() {
        let (tokens, nullable) = infer("Vec<i64>");
        assert_eq!(tokens, "rusty_db_core :: ColumnType :: Text");
        assert!(!nullable);
    }

    #[test]
    fn an_unrecognized_custom_type_falls_back_to_text() {
        let (tokens, nullable) = infer("MyCustomEnum");
        assert_eq!(tokens, "rusty_db_core :: ColumnType :: Text");
        assert!(!nullable);
    }

    fn rust_tokens_for(expr: &str, fields: &[FieldInfo]) -> String {
        let node = parse_hybrid_expr(expr).unwrap();
        hybrid_rust_tokens(&node, fields).to_string()
    }

    fn sql_tokens_for(expr: &str, fields: &[FieldInfo]) -> String {
        let node = parse_hybrid_expr(expr).unwrap();
        let core = quote! { rusty_db_core };
        hybrid_sql_tokens(&node, &core, fields).to_string()
    }

    fn field(name: &str, column: &str, ty: &str) -> FieldInfo {
        FieldInfo {
            ident: syn::Ident::new(name, proc_macro2::Span::call_site()),
            ty: parse_ty(ty),
            column: column.to_string(),
            primary_key: false,
            version: false,
            soft_delete: false,
            default: None,
        }
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        let fields = vec![
            field("a", "a", "i64"),
            field("b", "b", "i64"),
            field("c", "c", "i64"),
        ];
        // `a + b * c` should parse as `a + (b * c)`, not `(a + b) * c`.
        assert_eq!(
            rust_tokens_for("a + b * c", &fields),
            rust_tokens_for("a + (b * c)", &fields),
        );
        assert_ne!(
            rust_tokens_for("a + b * c", &fields),
            rust_tokens_for("(a + b) * c", &fields),
        );
    }

    #[test]
    fn parentheses_override_default_precedence() {
        let node = parse_hybrid_expr("(a + b) * c").unwrap();
        match node {
            HybridNode::Op(_, HybridOp::Mul, _) => {}
            other => panic!("expected a top-level multiplication, got {other:?}"),
        }
    }

    #[test]
    fn integer_literals_are_emitted_unsuffixed_on_the_rust_side() {
        let fields = vec![field("price", "price", "i64")];
        let tokens = rust_tokens_for("price * 2", &fields);
        assert!(
            !tokens.contains("i64") && !tokens.contains("i32"),
            "expected an unsuffixed literal so it adapts to the field's own type, got: {tokens}"
        );
    }

    #[test]
    fn float_literals_parse_and_render_on_both_sides() {
        let fields = vec![field("price", "price", "f64")];
        let node = parse_hybrid_expr("price * 1.5").unwrap();
        match node {
            HybridNode::Op(_, HybridOp::Mul, rhs) => {
                assert!(matches!(*rhs, HybridNode::FloatLit(n) if n == 1.5));
            }
            other => panic!("expected a multiplication, got {other:?}"),
        }
        let sql = sql_tokens_for("price * 1.5", &fields);
        assert!(sql.contains("Expr :: lit"));
    }

    #[test]
    fn sql_side_uses_the_column_name_not_the_rust_field_name() {
        let fields = vec![field("price", "unit_price", "i64")];
        let sql = sql_tokens_for("price", &fields);
        assert!(sql.contains("\"unit_price\""));
        assert!(!sql.contains("\"price\""));
    }

    #[test]
    fn division_and_subtraction_are_left_associative() {
        // `a - b - c` must parse as `(a - b) - c`, not `a - (b - c)` — these
        // give different results, so getting associativity backwards would
        // be a silent correctness bug, not just a cosmetic rendering choice.
        let node = parse_hybrid_expr("a - b - c").unwrap();
        match node {
            HybridNode::Op(lhs, HybridOp::Sub, rhs) => {
                assert!(matches!(*rhs, HybridNode::Field(name) if name == "c"));
                assert!(matches!(*lhs, HybridNode::Op(_, HybridOp::Sub, _)));
            }
            other => panic!("expected a subtraction, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_field_reference_is_a_valid_expression() {
        let fields = vec![field("price", "price", "i64")];
        assert!(parse_hybrid_expr("price").is_ok());
        let _ = rust_tokens_for("price", &fields);
    }

    #[test]
    fn unknown_characters_are_rejected() {
        assert!(tokenize_hybrid_expr("price % 2").is_err());
    }

    #[test]
    fn trailing_garbage_after_a_complete_expression_is_rejected() {
        let tokens = tokenize_hybrid_expr("price + quantity )").unwrap();
        assert!(HybridParser::parse(&tokens).is_err());
    }

    #[test]
    fn an_unopened_or_unclosed_paren_is_rejected() {
        assert!(parse_hybrid_expr("(price + quantity").is_err());
        assert!(parse_hybrid_expr("price + quantity)").is_err());
    }

    #[test]
    fn every_referenced_field_name_is_collected_in_left_to_right_order() {
        let node = parse_hybrid_expr("(a + b) * c").unwrap();
        assert_eq!(
            hybrid_field_names(&node),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn a_pure_literal_expression_references_no_fields() {
        let node = parse_hybrid_expr("2 * (3 + 4)").unwrap();
        assert!(hybrid_field_names(&node).is_empty());
    }

    #[test]
    fn every_comparison_operator_tokenizes_and_parses() {
        for (src, expected) in [
            ("price < 10", HybridCompareOp::Lt),
            ("price <= 10", HybridCompareOp::Le),
            ("price > 10", HybridCompareOp::Gt),
            ("price >= 10", HybridCompareOp::Ge),
            ("price == 10", HybridCompareOp::Eq),
            ("price != 10", HybridCompareOp::Ne),
        ] {
            match parse_hybrid_expr(src).unwrap() {
                HybridNode::Compare(_, op, _) => assert_eq!(op, expected, "for {src:?}"),
                other => panic!("expected a comparison for {src:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_comparisons_operands_can_be_arbitrary_arithmetic() {
        // `price * quantity > 100` — both sides of the comparison are
        // themselves full arithmetic sub-expressions, not just bare fields.
        match parse_hybrid_expr("price * quantity > 100").unwrap() {
            HybridNode::Compare(lhs, HybridCompareOp::Gt, rhs) => {
                assert!(matches!(*lhs, HybridNode::Op(_, HybridOp::Mul, _)));
                assert!(matches!(*rhs, HybridNode::IntLit(100)));
            }
            other => panic!("expected a comparison, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_equals_sign_is_rejected_not_silently_treated_as_assignment() {
        assert!(tokenize_hybrid_expr("price = 10").is_err());
    }

    #[test]
    fn chained_comparisons_are_rejected() {
        // `a < b < c` isn't meaningful the way it might look — same
        // restriction Rust's own grammar puts on it.
        let tokens = tokenize_hybrid_expr("a < b < c").unwrap();
        assert!(HybridParser::parse(&tokens).is_err());
    }

    #[test]
    fn a_comparison_defaults_to_a_bool_return_type_not_the_operands_type() {
        let fields = vec![field("price", "price", "i64")];
        let node = parse_hybrid_expr("price > 100").unwrap();
        assert!(matches!(node, HybridNode::Compare(..)));
        // The actual `ty` inference lives in `expand_hybrid`, which this
        // unit test doesn't invoke directly (it needs a full `HybridSpec` +
        // struct context) — but the Rust/SQL rendering below is what that
        // inferred `bool` return type is built from, so cover those here.
        let rust = rust_tokens_for("price > 100", &fields);
        assert!(rust.contains('>'));
        let sql = sql_tokens_for("price > 100", &fields);
        assert!(sql.contains("gt_expr"));
    }

    #[test]
    fn every_comparison_operator_renders_the_matching_expr_method() {
        let fields = vec![field("price", "price", "i64")];
        for (src, method) in [
            ("price < 10", "lt_expr"),
            ("price <= 10", "lte_expr"),
            ("price > 10", "gt_expr"),
            ("price >= 10", "gte_expr"),
            ("price == 10", "eq_expr"),
            ("price != 10", "ne_expr"),
        ] {
            let sql = sql_tokens_for(src, &fields);
            assert!(
                sql.contains(method),
                "expected {src:?} to render `{method}`, got: {sql}"
            );
        }
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // `a > 1 || b > 2 && c > 3` should parse as `a > 1 || (b > 2 && c > 3)`,
        // not `(a > 1 || b > 2) && c > 3`.
        match parse_hybrid_expr("a > 1 || b > 2 && c > 3").unwrap() {
            HybridNode::BoolOp(lhs, HybridBoolOp::Or, rhs) => {
                assert!(matches!(*lhs, HybridNode::Compare(..)));
                assert!(matches!(*rhs, HybridNode::BoolOp(_, HybridBoolOp::And, _)));
            }
            other => panic!("expected a top-level ||, got {other:?}"),
        }
    }

    #[test]
    fn and_and_or_are_left_associative() {
        let node = parse_hybrid_expr("a > 1 && b > 2 && c > 3").unwrap();
        match node {
            HybridNode::BoolOp(lhs, HybridBoolOp::And, rhs) => {
                assert!(matches!(*rhs, HybridNode::Compare(..)));
                assert!(matches!(*lhs, HybridNode::BoolOp(_, HybridBoolOp::And, _)));
            }
            other => panic!("expected a top-level &&, got {other:?}"),
        }
    }

    #[test]
    fn a_bare_ampersand_or_pipe_is_rejected() {
        assert!(tokenize_hybrid_expr("price & 1").is_err());
        assert!(tokenize_hybrid_expr("price | 1").is_err());
    }

    #[test]
    fn a_bool_combinator_expression_defaults_to_a_bool_return_type() {
        let fields = vec![field("price", "price", "i64"), field("qty", "qty", "i64")];
        let node = parse_hybrid_expr("price > 10 && qty > 1").unwrap();
        assert!(matches!(node, HybridNode::BoolOp(..)));
        let rust = rust_tokens_for("price > 10 && qty > 1", &fields);
        assert!(rust.contains("&&"));
        let sql = sql_tokens_for("price > 10 && qty > 1", &fields);
        assert!(sql.contains(". and"));

        let sql_or = sql_tokens_for("price > 10 || qty > 1", &fields);
        assert!(sql_or.contains(". or"));
    }

    #[test]
    fn every_field_referenced_across_a_bool_combinator_is_collected() {
        let node = parse_hybrid_expr("a > 1 && b < 2 || c == 3").unwrap();
        assert_eq!(
            hybrid_field_names(&node),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn a_string_literal_tokenizes_and_parses() {
        let tokens = tokenize_hybrid_expr("\"hello\"").unwrap();
        assert_eq!(tokens, vec![HybridToken::Str("hello".to_string())]);
        let node = parse_hybrid_expr("\"hello\"").unwrap();
        assert!(matches!(node, HybridNode::StringLit(s) if s == "hello"));
    }

    #[test]
    fn an_unterminated_string_literal_is_rejected() {
        assert!(tokenize_hybrid_expr("\"hello").is_err());
    }

    #[test]
    fn upper_and_lower_parse_as_single_argument_calls() {
        let fields = vec![field("name", "name", "String")];
        match parse_hybrid_expr("upper(name)").unwrap() {
            HybridNode::Upper(inner) => {
                assert!(matches!(*inner, HybridNode::Field(f) if f == "name"))
            }
            other => panic!("expected Upper, got {other:?}"),
        }
        match parse_hybrid_expr("lower(name)").unwrap() {
            HybridNode::Lower(inner) => {
                assert!(matches!(*inner, HybridNode::Field(f) if f == "name"))
            }
            other => panic!("expected Lower, got {other:?}"),
        }
        let _ = rust_tokens_for("upper(name)", &fields);
        let _ = rust_tokens_for("lower(name)", &fields);
    }

    #[test]
    fn concat_parses_as_a_two_argument_call() {
        let fields = vec![
            field("first", "first", "String"),
            field("last", "last", "String"),
        ];
        match parse_hybrid_expr("concat(first, last)").unwrap() {
            HybridNode::Concat(lhs, rhs) => {
                assert!(matches!(*lhs, HybridNode::Field(f) if f == "first"));
                assert!(matches!(*rhs, HybridNode::Field(f) if f == "last"));
            }
            other => panic!("expected Concat, got {other:?}"),
        }
        let _ = rust_tokens_for("concat(first, last)", &fields);
    }

    #[test]
    fn upper_lower_and_concat_reject_the_wrong_argument_count() {
        assert!(parse_hybrid_expr("upper(name, name)").is_err());
        assert!(parse_hybrid_expr("upper()").is_err());
        assert!(parse_hybrid_expr("concat(name)").is_err());
        assert!(parse_hybrid_expr("concat(name, name, name)").is_err());
    }

    #[test]
    fn an_unknown_function_name_is_rejected() {
        let err = parse_hybrid_expr("frobnicate(name)").unwrap_err();
        assert!(err.contains("unknown function"), "got: {err}");
    }

    #[test]
    fn upper_lower_and_concat_render_the_matching_rust_and_sql_calls() {
        let fields = vec![
            field("first", "first", "String"),
            field("last", "last", "String"),
        ];

        let rust = rust_tokens_for("upper(first)", &fields);
        assert!(rust.contains("to_uppercase"));
        let sql = sql_tokens_for("upper(first)", &fields);
        assert!(sql.contains(". upper"));

        let rust = rust_tokens_for("lower(first)", &fields);
        assert!(rust.contains("to_lowercase"));
        let sql = sql_tokens_for("lower(first)", &fields);
        assert!(sql.contains(". lower"));

        let rust = rust_tokens_for("concat(first, last)", &fields);
        assert!(rust.contains("format !"));
        let sql = sql_tokens_for("concat(first, last)", &fields);
        assert!(sql.contains(". concat"));
    }

    #[test]
    fn a_string_function_expression_defaults_to_a_string_return_type() {
        let node = parse_hybrid_expr("upper(name)").unwrap();
        assert!(matches!(node, HybridNode::Upper(..)));
        let node = parse_hybrid_expr("concat(first, last)").unwrap();
        assert!(matches!(node, HybridNode::Concat(..)));
    }

    #[test]
    fn a_string_function_can_be_used_inside_arithmetic_free_comparisons() {
        // concat/upper/lower nest fine as comparison operands since
        // parse_comparison calls parse_expr on each side, same as any
        // other arithmetic sub-expression.
        let node = parse_hybrid_expr("upper(name) == \"ADA\"").unwrap();
        match node {
            HybridNode::Compare(lhs, HybridCompareOp::Eq, rhs) => {
                assert!(matches!(*lhs, HybridNode::Upper(..)));
                assert!(matches!(*rhs, HybridNode::StringLit(s) if s == "ADA"));
            }
            other => panic!("expected a comparison, got {other:?}"),
        }
    }

    #[test]
    fn a_parenthesized_boolean_group_overrides_the_default_flat_precedence() {
        // Without parens, `a > 1 || b > 2 && c > 3` parses as
        // `a > 1 || (b > 2 && c > 3)` (see `and_binds_tighter_than_or`).
        // With parens around the first two, the grouping flips.
        let node = parse_hybrid_expr("(a > 1 || b > 2) && c > 3").unwrap();
        match node {
            HybridNode::BoolOp(lhs, HybridBoolOp::And, rhs) => {
                assert!(matches!(*lhs, HybridNode::BoolOp(_, HybridBoolOp::Or, _)));
                assert!(matches!(*rhs, HybridNode::Compare(..)));
            }
            other => panic!("expected a top-level &&, got {other:?}"),
        }
    }

    #[test]
    fn a_single_parenthesized_comparison_is_equivalent_to_the_unparenthesized_form() {
        let fields = vec![field("price", "price", "i64")];
        assert_eq!(
            rust_tokens_for("(price > 50)", &fields),
            rust_tokens_for("price > 50", &fields),
        );
        assert_eq!(
            sql_tokens_for("(price > 50)", &fields),
            sql_tokens_for("price > 50", &fields),
        );
    }

    #[test]
    fn a_parenthesized_boolean_group_still_infers_a_bool_return_type() {
        let node = parse_hybrid_expr("(a > 1 || b > 2) && c > 3").unwrap();
        assert!(matches!(node, HybridNode::BoolOp(..)));
    }

    #[test]
    fn a_comparison_nested_inside_arithmetic_via_parens_is_rejected() {
        // `(a > 1) + 5` is syntactically reachable now that parens can
        // hold a comparison, but must still be rejected: the query
        // builder's `Expr` has no type distinguishing arithmetic from
        // boolean, so this would build a valid `Expr` tree while only
        // failing (a `bool + i32` type error) on the Rust side.
        let err = parse_hybrid_expr("(a > 1) + 5").unwrap_err();
        assert!(err.contains("comparison"), "got: {err}");
    }

    #[test]
    fn a_comparison_nested_inside_another_comparison_via_parens_is_rejected() {
        let err = parse_hybrid_expr("(a > 1) > 2").unwrap_err();
        assert!(err.contains("comparison"), "got: {err}");
    }

    #[test]
    fn a_bool_combinator_nested_inside_arithmetic_via_parens_is_rejected() {
        let err = parse_hybrid_expr("(a > 1 && b > 2) + 5").unwrap_err();
        assert!(err.contains("&&"), "got: {err}");
    }

    #[test]
    fn a_comparison_nested_inside_a_string_function_argument_via_parens_is_rejected() {
        let err = parse_hybrid_expr("upper((a > 1))").unwrap_err();
        assert!(err.contains("comparison"), "got: {err}");
    }
}
