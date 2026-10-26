//! `#[derive(Describe)]` for flecs-describe. See that crate for the layouts it writes.
//!
//! Attributes, all under `#[describe(...)]`:
//! - on the type: `name = "Path"` (the layout's and Flecs type's name; default the Rust name with its type
//!   arguments' names), `runtime` (the whole type holds process-local state: layout `{"runtime": name}`, value null,
//!   restored as `Default::default()`);
//! - on a field: `merge = "Rule"` (the member's rule slot: LastWriter, AddWins, Counter, KeepBoth or Text; not inside
//!   an enum's variants), `runtime` (the field's value is not saved; restored as its `Default`), `with = path` (or
//!   `with = "path"`: a module with `layout`, `read` and `from_value` for the field, see `flecs_describe::with`),
//!   `name = "member"` (the member's name; default the field's name, or `0`, `1`, ... in a tuple);
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
}

#[derive(Default)]
struct Container {
    name: Option<String>,
    runtime: bool,
}

fn container_attrs(attrs: &[syn::Attribute]) -> syn::Result<Container> {
    let mut c = Container::default();
    for a in attrs.iter().filter(|a| a.path().is_ident("describe")) {
        a.parse_nested_meta(|m| {
            if m.path.is_ident("name") {
                c.name = Some(m.value()?.parse::<LitStr>()?.value());
            } else if m.path.is_ident("runtime") {
                c.runtime = true;
            } else {
                return Err(m.error("unknown describe attribute on a type (name, runtime)"));
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
                } else {
                    return Err(m.error(
                        "unknown describe attribute on a field (merge, runtime, with, name)",
                    ));
                }
                Ok(())
            })?;
        }
        out.push(Field {
            member,
            bind,
            name,
            ty: f.ty.clone(),
            merge,
            kind,
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

fn layout_of(f: &Field) -> Ts {
    let p = krate();
    let ty = &f.ty;
    match &f.kind {
        Kind::Plain => quote!(<#ty as #p::Describe>::layout()),
        Kind::Runtime => {
            quote!(#p::__private::runtime_layout(#p::__private::type_text(stringify!(#ty))))
        }
        Kind::With(w) => quote!(#w::layout()),
    }
}

/// `r` is an expression of type `&FieldType`.
fn read_of(f: &Field, r: Ts) -> Ts {
    let p = krate();
    let ty = &f.ty;
    let name = &f.name;
    let e = match &f.kind {
        Kind::Plain => quote!(<#ty as #p::Describe>::read(#r, refs)),
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
        Kind::Plain => quote!(<#ty as #p::Describe>::from_value(#v, ids)),
        Kind::Runtime => quote!(#p::__private::runtime_from_value::<#ty>(#v)),
        Kind::With(w) => quote!(#w::from_value(#v, ids)),
    };
    quote!((#e).map_err(|e| #p::member_err(#name, e))?)
}

fn flecs_of(f: &Field) -> Ts {
    let p = krate();
    let ty = &f.ty;
    match &f.kind {
        Kind::Plain => quote!(<#ty as #p::Describe>::flecs()),
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
    let type_name = match &c.name {
        Some(n) => quote!(::std::string::String::from(#n)),
        None if args.is_empty() => quote!(::std::string::String::from(#base)),
        None => quote!({
            let a: ::std::vec::Vec<::std::string::String> = ::std::vec![#(#args),*];
            ::std::format!("{}<{}>", #base, a.join(", "))
        }),
    };

    let body = if c.runtime {
        quote! {
            fn layout() -> #p::Ipld {
                #p::__private::runtime_layout(<Self as #p::Describe>::type_name())
            }
            fn read(&self, _refs: &mut dyn #p::Refs) -> #p::R<#p::Ipld> {
                ::core::result::Result::Ok(#p::Ipld::Null)
            }
            fn from_value(v: &#p::Ipld, _ids: &mut dyn #p::Resolve) -> #p::R<Self> {
                #p::__private::runtime_from_value::<Self>(v)
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

    Ok(quote! {
        impl #ig #p::Describe for #ident #tg #wc {
            fn type_name() -> ::std::string::String {
                #type_name
            }
            #body
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
