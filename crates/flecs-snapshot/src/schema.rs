//! The `sync` module every world starts with (SPEC.md, "The sync module"): the stable id component, the `Merge`
//! relationship and its rules, and the reserved permission and validation vocabulary. It is defined by a Flecs script,
//! so it is ordinary data in the world, and put on the lookup path so scripts write `(Merge, Text)` unqualified.

use std::ffi::CString;

use flecs_c as sys;
use sha2::{Digest, Sha256};
use sys::{ecs_entity_t, ecs_world_t};

use crate::merge::Rule;
use crate::mode::Mode;

pub const SCRIPT: &str = r#"
sync {
  // The stable id (SPEC.md, "Stable ids"). Not copied to prefab instances: an instance gets its own.
  struct Id(hi: u64, lo: u64)
  Id { (OnInstantiate, DontInherit) }

  // (Merge, Rule) on a component, or on one of its members (which overrides the component's rule).
  Merge { Exclusive }
  LastWriter {}
  AddWins {}
  Counter {}
  KeepBoth {}
  Text {}

  // (Sync, Mode) on a component or tag type: how its values are kept (SPEC.md, "Sync modes"). Synced is the default.
  Sync { Exclusive }
  Synced {}
  LatestOnly {}
  Ephemeral {}

  // Binary data (SPEC.md, "Values"). Bytes is written inline as a CBOR byte string; Blob is cut into raw blocks by
  // content-defined chunking and written as a CID link.
  vector Bytes(type: u8)
  vector Blob(type: u8)

  // Reserved, not enforced (SPEC.md, "Permissions").
  // An actor: an entity with its public key. `key` is its raw ed25519 public key in lowercase hex (64 digits): the actor id.
  struct Actor(key: string)
  // Access to a subtree, on a role prefab or an actor (Keyhive's levels: Relay < Read < Edit < Admin). Each level
  // implies the ones below it; a grant covers the entity and everything under it.
  // (CanRelay, subtree): may store and forward its ciphertext blocks, not read them.
  CanRelay {}
  // (CanRead, subtree): may read it (is a member of its group key).
  CanRead {}
  // (CanWrite, subtree): may write it.
  CanWrite {}
  // (CanAdmin, subtree): may change who has access to it.
  CanAdmin {}
  // (WritableBy, role) on a component type: only actors with that role may write the component.
  WritableBy {}
  // (ValidatedBy, validator) on a component type or a subtree root: every change must pass the validator.
  ValidatedBy {}
  // A validator: a pure predicate named by `name`, at `version` (SPEC.md, "Validation").
  struct Validator(name: string, version: u32)
  // (Authority, actor) on a subtree root: changes there need that actor's countersignature.
  Authority { Exclusive }
}
"#;

pub struct Schema {
    pub module: ecs_entity_t,
    pub id_component: ecs_entity_t,
    pub merge: ecs_entity_t,
    pub validated_by: ecs_entity_t,
    pub writable_by: ecs_entity_t,
    pub sync: ecs_entity_t,
    pub bytes: ecs_entity_t,
    pub blob: ecs_entity_t,
    rules: Vec<(ecs_entity_t, Rule)>,
    modes: Vec<(ecs_entity_t, Mode)>,
    lookup: Box<[ecs_entity_t; 3]>,
}

fn lookup(w: *const ecs_world_t, path: &str) -> Result<ecs_entity_t, String> {
    let c = CString::new(path).expect("no NUL");
    // SAFETY: plain lookup.
    match unsafe { sys::ecs_lookup(w, c.as_ptr()) } {
        0 => Err(format!("sync module: {path} missing")),
        e => Ok(e),
    }
}

impl Schema {
    /// Looks the module up after SCRIPT ran, and puts it on the lookup path.
    pub fn new(w: *mut ecs_world_t) -> Result<Schema, String> {
        let module = lookup(w, "sync")?;
        let mut rules = Vec::new();
        for r in Rule::ALL {
            rules.push((lookup(w, &format!("sync.{}", r.name()))?, r));
        }
        let mut modes = Vec::new();
        for m in Mode::ALL {
            modes.push((lookup(w, &format!("sync.{}", m.name()))?, m));
        }
        // SAFETY: a read of a C global set by ecs_init.
        let core = unsafe { sys::EcsFlecsCore };
        let s = Schema {
            module,
            id_component: lookup(w, "sync.Id")?,
            merge: lookup(w, "sync.Merge")?,
            validated_by: lookup(w, "sync.ValidatedBy")?,
            writable_by: lookup(w, "sync.WritableBy")?,
            sync: lookup(w, "sync.Sync")?,
            bytes: lookup(w, "sync.Bytes")?,
            blob: lookup(w, "sync.Blob")?,
            rules,
            modes,
            lookup: Box::new([core, module, 0]),
        };
        // SAFETY: the array is zero-terminated and lives as long as the Schema, which lives as long as the world.
        unsafe { sys::ecs_set_lookup_path(w, s.lookup.as_ptr()) };
        Ok(s)
    }

    pub fn rule(&self, e: ecs_entity_t) -> Option<Rule> {
        self.rules.iter().find(|(x, _)| *x == e).map(|(_, r)| *r)
    }

    pub fn mode(&self, e: ecs_entity_t) -> Option<Mode> {
        self.modes.iter().find(|(x, _)| *x == e).map(|(_, m)| *m)
    }

    pub fn stable_id(&self, w: *const ecs_world_t, e: ecs_entity_t) -> Option<[u8; 16]> {
        // SAFETY: plain queries; Id is two u64s.
        unsafe {
            if !sys::ecs_owns_id(w, e, self.id_component) {
                return None;
            }
            let p = sys::ecs_get_id(w, e, self.id_component).cast::<[u64; 2]>();
            let [hi, lo] = p.as_ref()?;
            let mut out = [0; 16];
            out[..8].copy_from_slice(&hi.to_be_bytes());
            out[8..].copy_from_slice(&lo.to_be_bytes());
            Some(out)
        }
    }

    pub fn set_stable_id(&self, w: *mut ecs_world_t, e: ecs_entity_t, id: [u8; 16]) {
        let v = [
            u64::from_be_bytes(id[..8].try_into().unwrap()),
            u64::from_be_bytes(id[8..].try_into().unwrap()),
        ];
        // SAFETY: Id is two u64s.
        unsafe { sys::ecs_set_id(w, e, self.id_component, 16, v.as_ptr().cast()) };
    }
}

/// SPEC.md, "Stable ids": the first 16 bytes of SHA-256 over a domain tag, the script's name and the entity's path
/// when it was first given an id.
pub fn derive_id(script: &str, path: &str) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(b"flecs-entity-id/1\0");
    h.update(script.as_bytes());
    h.update(b"\0");
    h.update(path.as_bytes());
    h.finalize()[..16].try_into().unwrap()
}
