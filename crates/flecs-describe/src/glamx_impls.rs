//! glamx's poses and 2D rotations (the types rapier and parry use) as structs named `glamx.<Type>`, in the layout,
//! the value and the Flecs meta: `glamx.Pose3 {rotation, translation}` (a `glam.Quat` and a `glam.Vec3`),
//! `glamx.Rot2 {re, im}`, `glamx.Pose2 {rotation, translation}`, and their `f64` (`D`) and `Pose3A` forms.
//! `Rot3` and `DRot3` are `glam::Quat` and `glam::DQuat`, described by the glam feature.
//!
//! `Pose3` carries a `padding: u32` after its translation (for bytemuck and SPIR-V; never read). It is not a member:
//! not saved, restored as zero, and its four bytes lie unnamed inside the Flecs struct, which is the full 32 bytes
//! with member offsets measured on a value (as glam's SIMD types).

use crate::__private::{get, offset_in, struct_layout, struct_value, take_struct};
use crate::glam_impls::glam_struct;
use crate::{Describe, Flecs, FlecsMember, FlecsStruct, Ipld, R, Refs, Resolve, member_err};

glam_struct! {
    glamx;
    Rot2 { re: f32, im: f32 }
    DRot2 { re: f64, im: f64 }
    Pose2 { rotation: glamx::Rot2, translation: glam::Vec2 }
    DPose2 { rotation: glamx::DRot2, translation: glam::DVec2 }
    Pose3 { rotation: glam::Quat, translation: glam::Vec3 }
    Pose3A { rotation: glam::Quat, translation: glam::Vec3A }
    DPose3 { rotation: glam::DQuat, translation: glam::DVec3 }
}
