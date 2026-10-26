//! Values saved by name and looked up in a catalog (SPEC.md, "Logical types", `refers_to`).

use crate::{Describe, Ipld, R, Refs, Resolve};

/// A fixed set of `'static` entries a value refers to by name: a member `&'static T` is saved as the entry's name
/// (layout `{"logical": null, "of": "string", "rules": {"refers_to": [CATALOG, [names]]}}`) and restored by looking
/// the name up. Registering a type that holds one makes the catalog entity at `CATALOG` (`sync.Catalog`, a child per
/// name), so the world, REST and the inspector list the names.
///
/// ```ignore
/// impl Catalog for Material {
///     const CATALOG: &'static str = "sim.materials";
///     fn entries() -> &'static [&'static Material] { &MATERIALS }
///     fn name(&self) -> &str { self.name }
/// }
/// ```
pub trait Catalog: 'static {
    /// The catalog entity's path (`.`-separated, as SPEC.md writes paths).
    const CATALOG: &'static str;
    /// Every entry, each with a different name.
    fn entries() -> &'static [&'static Self];
    /// The entry's name, as saved.
    fn name(&self) -> &str;
}

fn names<T: Catalog>() -> Vec<Ipld> {
    let mut n: Vec<&str> = T::entries().iter().map(|e| e.name()).collect();
    n.sort_unstable();
    n.dedup();
    n.into_iter().map(|s| Ipld::String(s.into())).collect()
}

impl<T: Catalog> Describe for &'static T {
    fn type_name() -> String {
        format!("&{}", T::CATALOG)
    }

    fn layout() -> Ipld {
        crate::__private::refine(
            Ipld::String("string".into()),
            None,
            vec![(
                "refers_to",
                Ipld::List(vec![
                    Ipld::String(T::CATALOG.into()),
                    Ipld::List(names::<T>()),
                ]),
            )],
        )
    }

    /// The entry's name; an entry not in `entries()` (a copy, say) is refused, since restoring its name would give
    /// another value.
    fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
        let me: &T = self;
        let found = T::entries()
            .iter()
            .find(|e| e.name() == me.name())
            .is_some_and(|e| core::ptr::eq(*e, me));
        if found {
            Ok(Ipld::String(me.name().into()))
        } else {
            Err(format!(
                "{:?} is not the catalog {}'s entry of that name (only its entries can be saved)",
                me.name(),
                T::CATALOG
            ))
        }
    }

    fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
        let Ipld::String(n) = v else {
            return Err(format!("not a name in the catalog {}", T::CATALOG));
        };
        T::entries()
            .iter()
            .copied()
            .find(|e| e.name() == n)
            .ok_or_else(|| format!("{n:?} is not a name in the catalog {}", T::CATALOG))
    }
}
