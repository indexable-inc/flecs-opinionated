//! `#[derive(Describe)]` for flecs-describe. See that crate for the layouts it writes.
//!
//! Leniency:
//! - a member whose type does not implement `Describe` is a runtime part (layout `{"runtime": "<its Rust type>"}`,
//!   value null, restored as its `Default`, or an error when it has none) instead of a compile error, so every
//!   component can derive it; `layouts.lock` shows each such part;
//! - a runtime part (marked or not) needs no `Default` to compile: restoring one without it is the error;
//! - the default name is the Rust path (`module_path!()` and the name, `my_crate::base::Uid`), so two types with
//!   one name in two modules never meet under `flecs.rust.described`;
//! - each non-generic type is listed in `flecs_describe::__private::DESCRIBED`, which `flecs_describe::describe_all`
//!   walks.
//!
//! Attributes, all under `#[describe(...)]`:
//! - on the type: `name = "Path"` (the layout's and Flecs type's name; default the Rust path with its type
//!   arguments' names), `runtime` (the whole type holds process-local state: layout `{"runtime": name}`, value null,
//!   restored as `Default::default()`), `transparent` (a struct of one field is described as that field: its
//!   layout, value and Flecs meta, so a newtype component `Ship(sim::Ship)` shows `Ship`'s members, not a
//!   member `0` holding them);
//! - on a field: `merge = "Rule"` (the member's rule slot: LastWriter, AddWins, Counter, KeepBoth or Text; not inside
//!   an enum's variants), `runtime` (the field's value is not saved; restored as its `Default`), `with = path` (or
//!   `with = "path"`: a module with `layout`, `read` and `from_value` for the field, see `flecs_describe::with`),
//!   `name = "member"` (the member's name; default the field's name, or `0`, `1`, ... in a tuple);
//!   `range(min = 1, max = 100)` (bounds are integer or float literals; either may be left out), `unit =
//!   "flecs.units.Length.Meters"` and `finite` (the member's logical rules, SPEC.md "Logical types": checked on
//!   every save, restore and set, and shown to Flecs as the member's range and unit);
//! - on a type also: `validate = path` (a `fn(&Self) -> Result<(), String>`: `from_value` refuses what it refuses,
//!   and for a non-generic type it is the check `sync.checks.<type name>` the layout names, run on every restore
//!   and set), and with `transparent` only, `logical = "Name"`, `range`, `unit` and `finite` (the type is a
//!   logical type over its field's: `#[describe(transparent, logical = "Percent", range(max = 100))]`);
//! - on a variant: `name = "Variant"`.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as Ts;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Error, Fields, GenericParam, Ident, LitStr, Path, Type, parse_macro_input,
};

const RULES: &[&str] = &["LastWriter", "AddWins", "Counter", "KeepBoth", "Text"];

#[proc_macro_derive(Describe, attributes(describe))]
pub fn derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(&input) {
        Ok(t) => t.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

enum Kind {
    Plain,
    Runtime,
    With(Path),
}

struct Field {
    /// How the field is reached: `self.a`, `self.0`, or in a variant the binding.
    member: syn::Member,
    bind: Ident,
    name: String,
    ty: Type,
    merge: Option<String>,
    kind: Kind,
    rules: Rules,
}

/// The logical rules an attribute declares (SPEC.md, "Logical types"): each an expression of type
/// `(&'static str, Ipld)` for `__private::refine`.
#[derive(Default)]
struct Rules(Vec<Ts>);

impl Rules {
    /// `range(min = .., max = ..)`, `unit = "path"` or `finite`; false when `m` is none of them.
    fn parse(&mut self, m: &syn::meta::ParseNestedMeta) -> syn::Result<bool> {
        let p = krate();
        if m.path.is_ident("range") {
            let (mut min, mut max) = (
                quote!(#p::__private::Bound::None),
                quote!(#p::__private::Bound::None),
            );
            m.parse_nested_meta(|b| {
                let v = bound(&b.value()?)?;
                if b.path.is_ident("min") {
                    min = v;
                } else if b.path.is_ident("max") {
                    max = v;
                } else {
                    return Err(b.error("range takes min and max"));
                }
                Ok(())
            })?;
            self.0.push(quote!(#p::__private::range_rule(#min, #max)));
        } else if m.path.is_ident("unit") {
            let u = m.value()?.parse::<LitStr>()?.value();
            self.0.push(quote!(("unit", #p::__private::text(#u))));
        } else if m.path.is_ident("finite") {
            self.0.push(quote!(("finite", #p::Ipld::Null)));
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// `l` (an `Ipld` layout expression) refined by the rules, with the logical name `name` (an `Option<String>`
    /// expression), or `l` itself when there is nothing to add.
    fn wrap(&self, l: Ts, name: Option<Ts>, extra: Option<Ts>) -> Ts {
        if self.0.is_empty() && name.is_none() && extra.is_none() {
            return l;
        }
        let p = krate();
        let rules = &self.0;
        let name = name.unwrap_or(quote!(::core::option::Option::None));
        let extra = extra.map(|e| quote!(__r.push(#e);));
        quote!({
            #[allow(unused_mut)]
            let mut __r: ::std::vec::Vec<(&'static str, #p::Ipld)> = ::std::vec![#(#rules),*];
            #extra
            #p::__private::refine(#l, #name, __r)
        })
    }
}

/// A range bound: an integer or float literal, optionally negative.
fn bound(input: &syn::parse::ParseStream) -> syn::Result<Ts> {
    let p = krate();
    let neg = input.parse::<Option<syn::Token![-]>>()?.is_some();
    let lit: syn::Lit = input.parse()?;
    let sign = if neg { quote!(-) } else { quote!() };
    match lit {
        syn::Lit::Int(i) => {
            let v: i128 = i.base10_parse()?;
            Ok(quote!(#p::__private::Bound::I(#sign #v)))
        }
        syn::Lit::Float(f) => {
            let v: f64 = f.base10_parse()?;
            Ok(quote!(#p::__private::Bound::F(#sign #v)))
        }
        other => Err(Error::new(
            other.span(),
            "a range bound is an integer or float literal",
        )),
    }
}

#[derive(Default)]
struct Container {
    name: Option<String>,
    runtime: bool,
    transparent: bool,
    /// `logical = "Name"` (with `transparent`): the logical type's name.
    logical: Option<String>,
    /// Rules on a `transparent` type: they refine the field's layout.
    rules: Rules,
    /// `validate = path`: `fn(&Self) -> Result<(), String>`.
    validate: Option<Path>,
}

fn container_attrs(attrs: &[syn::Attribute]) -> syn::Result<Container> {
    let mut c = Container::default();
    for a in attrs.iter().filter(|a| a.path().is_ident("describe")) {
        a.parse_nested_meta(|m| {
            if m.path.is_ident("name") {
                c.name = Some(m.value()?.parse::<LitStr>()?.value());
            } else if m.path.is_ident("runtime") {
                c.runtime = true;
            } else if m.path.is_ident("transparent") {
                c.transparent = true;
            } else if m.path.is_ident("logical") {
                c.logical = Some(m.value()?.parse::<LitStr>()?.value());
            } else if m.path.is_ident("validate") {
                let v = m.value()?;
                c.validate = Some(if v.peek(LitStr) {
                    v.parse::<LitStr>()?.parse::<Path>()?
                } else {
                    v.parse::<Path>()?
                });
            } else if !c.rules.parse(&m)? {
                return Err(m.error(
                    "unknown describe attribute on a type (name, runtime, transparent, logical, range, unit, \
                     finite, validate)",
                ));
            }
            Ok(())
        })?;
    }
    Ok(c)
}

fn variant_name(v: &syn::Variant) -> syn::Result<String> {
    let mut name = v.ident.to_string();
    for a in v.attrs.iter().filter(|a| a.path().is_ident("describe")) {
        a.parse_nested_meta(|m| {
            if m.path.is_ident("name") {
                name = m.value()?.parse::<LitStr>()?.value();
                Ok(())
            } else {
                Err(m.error("unknown describe attribute on a variant (name)"))
            }
        })?;
    }
    Ok(name)
}

fn fields(fs: &Fields, in_variant: bool) -> syn::Result<Vec<Field>> {
    let mut out = Vec::new();
    for (i, f) in fs.iter().enumerate() {
        let (member, bind, mut name) = match &f.ident {
            Some(id) => (syn::Member::Named(id.clone()), id.clone(), id.to_string()),
            None => (
                syn::Member::Unnamed(syn::Index::from(i)),
                format_ident!("f{i}"),
                i.to_string(),
            ),
        };
        let mut merge = None;
        let mut kind = Kind::Plain;
        let mut rules = Rules::default();
        for a in f.attrs.iter().filter(|a| a.path().is_ident("describe")) {
            a.parse_nested_meta(|m| {
                if m.path.is_ident("merge") {
                    let s = m.value()?.parse::<LitStr>()?;
                    if in_variant {
                        return Err(Error::new(
                            s.span(),
                            "a variant's members have no rule: a union value merges whole",
                        ));
                    }
                    if !RULES.contains(&s.value().as_str()) {
                        return Err(Error::new(
                            s.span(),
                            format!("unknown rule {:?} (one of {})", s.value(), RULES.join(", ")),
                        ));
                    }
                    merge = Some(s.value());
                } else if m.path.is_ident("runtime") {
                    kind = Kind::Runtime;
                } else if m.path.is_ident("with") {
                    let v = m.value()?;
                    let p = if v.peek(LitStr) {
                        v.parse::<LitStr>()?.parse::<Path>()?
                    } else {
                        v.parse::<Path>()?
                    };
                    kind = Kind::With(p);
                } else if m.path.is_ident("name") {
                    name = m.value()?.parse::<LitStr>()?.value();
                } else if !rules.parse(&m)? {
                    return Err(m.error(
                        "unknown describe attribute on a field (merge, runtime, with, name, range, unit, finite)",
                    ));
                }
                Ok(())
            })?;
        }
        if matches!(kind, Kind::Runtime) && !rules.0.is_empty() {
            return Err(Error::new(
                f.ty.span(),
                "a runtime field is not saved: it has no range, unit or finite",
            ));
        }
        out.push(Field {
            member,
            bind,
            name,
            ty: f.ty.clone(),
            merge,
            kind,
            rules,
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for f in &out {
        if !seen.insert(f.name.clone()) {
            return Err(Error::new(
                f.ty.span(),
                format!("two members are named {:?}", f.name),
            ));
        }
    }
    Ok(out)
}

fn krate() -> Ts {
    quote!(::flecs_describe)
}

/// The field's layout, refined by its rules.
fn layout_of(f: &Field) -> Ts {
    let p = krate();
    let ty = &f.ty;
    let l = match &f.kind {
        Kind::Plain => {
            quote!((&&#p::__private::Probe::<#ty>(::core::marker::PhantomData)).p_layout())
        }
        Kind::Runtime => {
            quote!(#p::__private::runtime_layout(#p::__private::type_text(stringify!(#ty))))
        }
        Kind::With(w) => quote!(#w::layout()),
    };
    f.rules.wrap(l, None, None)
}

/// `r` is an expression of type `&FieldType`.
fn read_of(f: &Field, r: Ts) -> Ts {
    let p = krate();
    let ty = &f.ty;
    let name = &f.name;
    let e = match &f.kind {
        Kind::Plain => {
            quote!((&&#p::__private::Probe::<#ty>(::core::marker::PhantomData)).p_read(#r, refs))
        }
        Kind::Runtime => quote!({
            let _ = #r;
            ::core::result::Result::<#p::Ipld, ::std::string::String>::Ok(#p::Ipld::Null)
        }),
        Kind::With(w) => quote!(#w::read(#r, refs)),
    };
    quote!((#e).map_err(|e| #p::member_err(#name, e))?)
}

/// `v` is an expression of type `&Ipld`.
fn from_of(f: &Field, v: Ts) -> Ts {
    let p = krate();
    let ty = &f.ty;
    let name = &f.name;
    let e = match &f.kind {
        Kind::Plain => {
            let d = default_of(quote!(#ty));
            quote!((&&#p::__private::Probe::<#ty>(::core::marker::PhantomData)).p_from(#v, ids, #d))
        }
        Kind::Runtime => runtime_from(quote!(#ty), v),
        Kind::With(w) => quote!(#w::from_value(#v, ids)),
    };
    quote!((#e).map_err(|e| #p::member_err(#name, e))?)
}

/// A runtime part's value: null, restored as `ty`'s `Default` or an error when it has none. The probe is expanded
/// here, at the concrete type: inside a generic function it would always find no `Default`.
fn runtime_from(ty: Ts, v: Ts) -> Ts {
    let p = krate();
    let d = default_of(ty);
    quote!(#p::__private::expect_null(#v, "a runtime part").and_then(|()| (#d)()))
}

/// `ty`'s `Default` probe as a `fn() -> R<ty>`.
fn default_of(ty: Ts) -> Ts {
    let p = krate();
    quote!((|| (&&#p::__private::DefaultProbe::<#ty>(::core::marker::PhantomData)).p_default()) as fn() -> #p::R<#ty>)
}

fn flecs_of(f: &Field) -> Ts {
    let p = krate();
    let ty = &f.ty;
    match &f.kind {
        Kind::Plain => {
            quote!((&&#p::__private::Probe::<#ty>(::core::marker::PhantomData)).p_flecs())
        }
        _ => quote!(#p::Flecs::Opaque),
    }
}

/// `struct_layout(name, members)` for `fs`; `name` is an `Option<String>` expression.
fn struct_layout(fs: &[Field], name: Ts) -> Ts {
    let p = krate();
    let ms = fs.iter().map(|f| {
        let n = &f.name;
        let l = layout_of(f);
        let r = match &f.merge {
            Some(r) => quote!(::core::option::Option::Some(#r)),
            None => quote!(::core::option::Option::None),
        };
        quote!((::std::string::String::from(#n), #l, #r))
    });
    quote!(#p::__private::struct_layout(#name, ::std::vec![#(#ms),*]))
}

/// A struct value from the fields, each reached by `access(field)` (a `&FieldType` expression).
fn struct_read(fs: &[Field], access: impl Fn(&Field) -> Ts) -> Ts {
    let p = krate();
    let es = fs.iter().map(|f| {
        let n = &f.name;
        let r = read_of(f, access(f));
        quote!((#n, #r))
    });
    quote!(#p::__private::struct_value(::std::vec![#(#es),*]))
}

/// `let m = take_struct(v)?` then `Ctor { member: from_value(m[name]) }`.
fn struct_from(fs: &[Field], v: Ts, ctor: Ts) -> Ts {
    let p = krate();
    let names = fs.iter().map(|f| &f.name);
    let inits = fs.iter().map(|f| {
        let m = &f.member;
        let n = &f.name;
        let e = from_of(f, quote!(#p::__private::get(__m, #n)?));
        quote!(#m: #e)
    });
    quote!({
        let __m = #p::__private::take_struct(#v, &[#(#names),*])?;
        let _ = &__m;
        #ctor { #(#inits),* }
    })
}

/// The integer `#[repr]` of an enum: (layout name, Flecs primitive kind constant), or `C` as i32.
fn repr_of(attrs: &[syn::Attribute]) -> syn::Result<Option<(&'static str, &'static str)>> {
    let mut found = None;
    let mut c = false;
    for a in attrs.iter().filter(|a| a.path().is_ident("repr")) {
        a.parse_nested_meta(|m| {
            if m.input.peek(syn::token::Paren) {
                let content;
                syn::parenthesized!(content in m.input);
                let _: Ts = content.parse()?;
                return Ok(());
            }
            let Some(id) = m.path.get_ident() else {
                return Ok(());
            };
            let r = match id.to_string().as_str() {
                "u8" => Some(("u8", "EcsU8")),
                "u16" => Some(("u16", "EcsU16")),
                "u32" => Some(("u32", "EcsU32")),
                "u64" => Some(("u64", "EcsU64")),
                "i8" => Some(("i8", "EcsI8")),
                "i16" => Some(("i16", "EcsI16")),
                "i32" => Some(("i32", "EcsI32")),
                "i64" => Some(("i64", "EcsI64")),
                "usize" => Some(("uptr", "EcsUPtr")),
                "isize" => Some(("iptr", "EcsIPtr")),
                "C" => {
                    c = true;
                    None
                }
                _ => None,
            };
            if r.is_some() {
                found = r;
            }
            Ok(())
        })?;
    }
    Ok(found.or(if c { Some(("i32", "EcsI32")) } else { None }))
}

fn expand(input: &DeriveInput) -> syn::Result<Ts> {
    let p = krate();
    let ident = &input.ident;
    let c = container_attrs(&input.attrs)?;

    let mut generics = input.generics.clone();
    let mut args = Vec::new();
    for g in &mut generics.params {
        match g {
            GenericParam::Type(t) => {
                t.bounds.push(syn::parse_quote!(#p::Describe));
                let id = &t.ident;
                args.push(quote!(<#id as #p::Describe>::type_name()));
            }
            GenericParam::Const(k) => {
                let id = &k.ident;
                args.push(quote!(::std::string::ToString::to_string(&#id)));
            }
            GenericParam::Lifetime(l) => {
                return Err(Error::new(
                    l.span(),
                    "a described type is 'static: it cannot have a lifetime parameter",
                ));
            }
        }
    }
    let (ig, tg, wc) = generics.split_for_impl();

    let base = ident.to_string();
    let path = quote!(::core::concat!(::core::module_path!(), "::", #base));
    let type_name = match &c.name {
        Some(n) => quote!(::std::string::String::from(#n)),
        None if args.is_empty() => quote!(::std::string::String::from(#path)),
        None => quote!({
            let a: ::std::vec::Vec<::std::string::String> = ::std::vec![#(#args),*];
            ::std::format!("{}<{}>", #path, a.join(", "))
        }),
    };

    if (c.logical.is_some() || !c.rules.0.is_empty()) && !c.transparent {
        return Err(Error::new(
            input.ident.span(),
            "logical, range, unit and finite on a type need describe(transparent): a struct or enum is refined \
             member by member",
        ));
    }
    if c.runtime && c.validate.is_some() {
        return Err(Error::new(
            input.ident.span(),
            "a runtime type is not saved: it has nothing to validate",
        ));
    }

    let runtime_self = runtime_from(quote!(Self), quote!(v));
    let body = if c.runtime {
        quote! {
            fn layout() -> #p::Ipld {
                #p::__private::runtime_layout(<Self as #p::Describe>::type_name())
            }
            fn read(&self, _refs: &mut dyn #p::Refs) -> #p::R<#p::Ipld> {
                ::core::result::Result::Ok(#p::Ipld::Null)
            }
            fn from_value(v: &#p::Ipld, _ids: &mut dyn #p::Resolve) -> #p::R<Self> {
                #runtime_self
            }
        }
    } else if c.transparent {
        match &input.data {
            Data::Struct(s) => transparent_body(&s.fields, input)?,
            _ => {
                return Err(Error::new(
                    input.ident.span(),
                    "describe(transparent) is for a struct of one field",
                ));
            }
        }
    } else {
        match &input.data {
            Data::Struct(s) => struct_body(&s.fields)?,
            Data::Enum(e) => enum_body(input, e)?,
            Data::Union(u) => {
                return Err(Error::new(
                    u.union_token.span,
                    "Describe cannot be derived for a union",
                ));
            }
        }
    };

    // Type-level refinement: the body's `layout` and `from_value` move to an inherent impl, and the trait's wrap
    // them (the layout refined, the decoded value validated).
    let generic = !input.generics.params.is_empty();
    let mut inherent = quote!();
    let body = if c.logical.is_some() || !c.rules.0.is_empty() || c.validate.is_some() {
        let item: syn::ItemImpl = syn::parse2(quote!(impl __X { #body }))?;
        let (mut raw, mut kept) = (Vec::new(), Vec::new());
        for it in item.items {
            match it {
                syn::ImplItem::Fn(mut f)
                    if f.sig.ident == "layout" || f.sig.ident == "from_value" =>
                {
                    f.sig.ident = format_ident!("__flecs_describe_{}", f.sig.ident);
                    raw.push(f);
                }
                other => kept.push(other),
            }
        }
        inherent = quote! {
            impl #ig #ident #tg #wc {
                #(#raw)*
            }
        };
        let name = c
            .logical
            .as_ref()
            .map(|n| quote!(::core::option::Option::Some(::std::string::String::from(#n))));
        // A generic type's check has no one entity to run as, so only `from_value` validates it.
        let extra = match (&c.validate, generic) {
            (Some(_), false) => Some(quote!((
                "validated",
                #p::Ipld::List(::std::vec![#p::__private::text(#p::__private::check_path(
                    &<Self as #p::Describe>::type_name()
                ))])
            ))),
            _ => None,
        };
        let layout = c
            .rules
            .wrap(quote!(Self::__flecs_describe_layout()), name, extra);
        let validate = c.validate.as_ref().map(|v| {
            quote!(#v(&__x).map_err(|e| ::std::format!("{} refused: {e}", <Self as #p::Describe>::type_name()))?;)
        });
        quote! {
            fn layout() -> #p::Ipld {
                #layout
            }
            fn from_value(v: &#p::Ipld, ids: &mut dyn #p::Resolve) -> #p::R<Self> {
                let __x = Self::__flecs_describe_from_value(v, ids)?;
                #validate
                ::core::result::Result::Ok(__x)
            }
            #(#kept)*
        }
    } else {
        body
    };

    let check = match (&c.validate, generic) {
        (Some(_), false) => quote! {
            #[#p::__private::linkme::distributed_slice(#p::__private::CHECKS)]
            #[linkme(crate = #p::__private::linkme)]
            static __FLECS_DESCRIBE_CHECK: #p::__private::Check = #p::__private::Check {
                path: || #p::__private::check_path(&<#ident as #p::Describe>::type_name()),
                check: #p::__private::check_by_decode::<#ident>,
            };
        },
        _ => quote!(),
    };

    // Listed for `describe_all` when it has one Rust type (no type parameters).
    let entry = if input.generics.params.is_empty() {
        quote! {
            #[#p::__private::linkme::distributed_slice(#p::__private::DESCRIBED)]
            #[linkme(crate = #p::__private::linkme)]
            static __FLECS_DESCRIBE_ENTRY: #p::__private::Entry = #p::__private::Entry {
                symbol: ::core::any::type_name::<#ident>,
                register: #p::__private::register_entry::<#ident>,
            };
        }
    } else {
        quote!()
    };

    Ok(quote! {
        #[allow(
            unsafe_code,
            non_upper_case_globals,
            unused_qualifications,
            clippy::all,
            clippy::pedantic,
            clippy::nursery
        )]
        const _: () = {
            #[allow(unused_imports)]
            use #p::__private::{ViaDefault as _, ViaDescribe as _, ViaNoDefault as _, ViaRuntime as _};

            #[automatically_derived]
            impl #ig #p::Describe for #ident #tg #wc {
                fn type_name() -> ::std::string::String {
                    #type_name
                }
                #body
            }

            #inherent
            #entry
            #check
        };
    })
}

/// A struct of one plain field described as the field (`#[describe(transparent)]`). The Flecs meta is the
/// field's when the field is the whole value (offset 0, the same size), else opaque.
fn transparent_body(fs: &Fields, input: &DeriveInput) -> syn::Result<Ts> {
    let p = krate();
    let fs = fields(fs, false)?;
    let [f] = fs.as_slice() else {
        return Err(Error::new(
            input.ident.span(),
            "describe(transparent) is for a struct of exactly one field",
        ));
    };
    if !matches!(f.kind, Kind::Plain) {
        return Err(Error::new(
            f.ty.span(),
            "describe(transparent): the field cannot be runtime or `with`",
        ));
    }
    let ty = &f.ty;
    let m = &f.member;
    let probe = quote!((&&#p::__private::Probe::<#ty>(::core::marker::PhantomData)));
    let default = default_of(quote!(#ty));
    Ok(quote! {
        fn layout() -> #p::Ipld {
            #probe.p_layout()
        }
        fn read(&self, refs: &mut dyn #p::Refs) -> #p::R<#p::Ipld> {
            #probe.p_read(&self.#m, refs)
        }
        fn from_value(v: &#p::Ipld, ids: &mut dyn #p::Resolve) -> #p::R<Self> {
            ::core::result::Result::Ok(Self { #m: #probe.p_from(v, ids, #default)? })
        }
        fn flecs() -> #p::Flecs {
            if #p::__private::offset_of!(Self, #m) == 0
                && #p::__private::size_of::<Self>() == #p::__private::size_of::<#ty>()
                && #p::__private::align_of::<Self>() == #p::__private::align_of::<#ty>()
            {
                #probe.p_flecs()
            } else {
                #p::Flecs::Opaque
            }
        }
    })
}

fn struct_body(fs: &Fields) -> syn::Result<Ts> {
    let p = krate();
    let fs = fields(fs, false)?;
    let layout = struct_layout(
        &fs,
        quote!(::core::option::Option::Some(<Self as #p::Describe>::type_name())),
    );
    let read = struct_read(&fs, |f| {
        let m = &f.member;
        quote!(&self.#m)
    });
    let from = struct_from(&fs, quote!(v), quote!(Self));
    let members = fs.iter().map(|f| {
        let m = &f.member;
        let n = &f.name;
        let ty = &f.ty;
        let fl = flecs_of(f);
        quote!(#p::FlecsMember {
            name: ::std::string::String::from(#n),
            offset: #p::__private::offset_of!(Self, #m),
            size: #p::__private::size_of::<#ty>(),
            ty: #fl,
        })
    });
    Ok(quote! {
        fn layout() -> #p::Ipld {
            #layout
        }
        fn read(&self, refs: &mut dyn #p::Refs) -> #p::R<#p::Ipld> {
            let _ = &refs;
            ::core::result::Result::Ok(#read)
        }
        fn from_value(v: &#p::Ipld, ids: &mut dyn #p::Resolve) -> #p::R<Self> {
            let _ = &ids;
            ::core::result::Result::Ok(#from)
        }
        fn flecs() -> #p::Flecs {
            #p::Flecs::Struct(#p::FlecsStruct {
                name: <Self as #p::Describe>::type_name(),
                size: #p::__private::size_of::<Self>(),
                align: #p::__private::align_of::<Self>(),
                members: ::std::vec![#(#members),*],
            })
        }
    })
}

fn enum_body(input: &DeriveInput, e: &syn::DataEnum) -> syn::Result<Ts> {
    let p = krate();
    if e.variants.is_empty() {
        return Err(Error::new(
            input.ident.span(),
            "Describe cannot be derived for an enum with no variants",
        ));
    }
    let names: Vec<String> = e
        .variants
        .iter()
        .map(variant_name)
        .collect::<syn::Result<_>>()?;
    let idents: Vec<&Ident> = e.variants.iter().map(|v| &v.ident).collect();

    if e.variants.iter().all(|v| matches!(v.fields, Fields::Unit)) {
        // A unit enum: {"enum", "underlying", "constants"}; the value is the constant's name.
        let repr = repr_of(&input.attrs)?;
        let underlying = repr.map_or("i64", |r| r.0);
        let flecs = match repr {
            Some((_, kind)) => {
                let kind = format_ident!("{kind}");
                quote!(#p::Flecs::Enum {
                    name: <Self as #p::Describe>::type_name(),
                    underlying: #p::__private::sys::#kind,
                    size: #p::__private::size_of::<Self>(),
                    constants: ::std::vec![#((::std::string::String::from(#names), Self::#idents as i128)),*],
                })
            }
            None => quote!(#p::Flecs::Opaque),
        };
        return Ok(quote! {
            fn layout() -> #p::Ipld {
                #p::__private::enum_layout(
                    <Self as #p::Describe>::type_name(),
                    #underlying,
                    ::std::vec![#((#names, Self::#idents as i128)),*],
                )
            }
            fn read(&self, _refs: &mut dyn #p::Refs) -> #p::R<#p::Ipld> {
                ::core::result::Result::Ok(#p::__private::text(match self {
                    #(Self::#idents => #names),*
                }))
            }
            fn from_value(v: &#p::Ipld, _ids: &mut dyn #p::Resolve) -> #p::R<Self> {
                let n = #p::__private::text_of(v, "an enum value")?;
                match n {
                    #(#names => ::core::result::Result::Ok(Self::#idents),)*
                    _ => ::core::result::Result::Err(
                        #p::__private::unknown_constant(n, &<Self as #p::Describe>::type_name()),
                    ),
                }
            }
            fn flecs() -> #p::Flecs {
                #flecs
            }
        });
    }

    // A data enum: a union; each variant's payload a struct with no name, or null for a unit variant.
    let mut layouts = Vec::new();
    let mut reads = Vec::new();
    let mut froms = Vec::new();
    for ((v, name), id) in e.variants.iter().zip(&names).zip(&idents) {
        let fs = fields(&v.fields, true)?;
        let binds: Vec<&Ident> = fs.iter().map(|f| &f.bind).collect();
        match &v.fields {
            Fields::Unit => {
                layouts.push(
                    quote!((::std::string::String::from(#name), ::core::option::Option::None)),
                );
                reads.push(quote!(Self::#id => #p::__private::union_value(#name, ::core::option::Option::None)));
                froms.push(quote!(#name => {
                    #p::__private::expect_null(__p, ::std::concat!("variant ", #name))?;
                    ::core::result::Result::Ok(Self::#id)
                }));
            }
            Fields::Named(_) | Fields::Unnamed(_) => {
                let l = struct_layout(&fs, quote!(::core::option::Option::None));
                layouts.push(
                    quote!((::std::string::String::from(#name), ::core::option::Option::Some(#l))),
                );
                let r = struct_read(&fs, |f| {
                    let b = &f.bind;
                    quote!(#b)
                });
                let pat = match &v.fields {
                    Fields::Named(_) => quote!(Self::#id { #(#binds),* }),
                    _ => quote!(Self::#id ( #(#binds),* )),
                };
                reads.push(quote!(#pat => #p::__private::union_value(#name, ::core::option::Option::Some(#r))));
                let from = struct_from(&fs, quote!(__p), quote!(Self::#id));
                froms.push(quote!(#name => ::core::result::Result::Ok(#from)));
            }
        }
    }
    Ok(quote! {
        fn layout() -> #p::Ipld {
            #p::__private::union_layout(<Self as #p::Describe>::type_name(), ::std::vec![#(#layouts),*])
        }
        fn read(&self, refs: &mut dyn #p::Refs) -> #p::R<#p::Ipld> {
            let _ = &refs;
            ::core::result::Result::Ok(match self {
                #(#reads),*
            })
        }
        fn from_value(v: &#p::Ipld, ids: &mut dyn #p::Resolve) -> #p::R<Self> {
            let _ = &ids;
            let (__n, __p) = #p::__private::take_union(v)?;
            match __n {
                #(#froms,)*
                _ => ::core::result::Result::Err(
                    #p::__private::unknown_variant(__n, &<Self as #p::Describe>::type_name()),
                ),
            }
        }
    })
}
