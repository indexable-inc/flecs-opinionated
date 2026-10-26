//! flecs_ecs's `Entity` as `"entity"`: a ref, or null for 0 (SPEC.md, "Values"). Its memory is a Flecs entity, so
//! Flecs sees the primitive kind `entity`.
//!
//! flecs_ecs's `Id` (an entity or a pair) is not described: turning a pair's halves into refs needs the world
//! (`ecs_get_alive` on each half), which `Refs` does not give. Describe a pair as two `Entity` fields instead.

use flecs_c as sys;

use crate::__private::text;
use crate::{Describe, Flecs, Ipld, R, Refs, Resolve};

impl Describe for flecs_ecs::core::Entity {
    fn type_name() -> String {
        "Entity".into()
    }
    fn layout() -> Ipld {
        text("entity")
    }
    fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
        if self.0 == 0 {
            Ok(Ipld::Null)
        } else {
            refs.reference(self.0)
        }
    }
    fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
        match v {
            Ipld::Null => Ok(Self(0)),
            r => ids.entity(r).map(Self),
        }
    }
    fn flecs() -> Flecs {
        Flecs::Primitive(sys::EcsEntity)
    }
}
