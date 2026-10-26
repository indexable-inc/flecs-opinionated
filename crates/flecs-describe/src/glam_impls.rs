//! glam's vectors and quaternions as arrays of their components (`glam.Vec3` is `[f32; 3]`, `glam.Quat` is
//! `[f32; 4]` x, y, z, w; what Flecs Script and the REST API write, `{"color": [0.6, 0.6, 0.65]}`), in the layout, the
//! value and the Flecs meta (an inline array member). Matrices and affine transforms are structs named
//! `glam.<Type>` with their fields as members (`glam.Mat3 {x_axis, y_axis, z_axis}`, each an array). `Vec3A` (three
//! floats in 16 bytes) and the `BVec` masks stay structs (`{x, y, z}`): an array of three would not be their size.
//!
//! The SIMD types (`Vec3A`, `Vec4`, `Quat`, `Mat2`, `Mat3A`, `Mat4`, `Affine3A` on targets with SIMD) reach their
//! fields through `Deref`, so member offsets are measured on a value rather than taken with `offset_of!`; a SIMD
//! type's Flecs struct is its full size (`Vec3A` is 16 bytes with three members). `BVec3A` and `BVec4A` are masks
//! with no stable memory layout and are not described.

use crate::__private::{get, offset_in, struct_layout, struct_value, take_struct};

/// a glam vector or quaternion as `[$e; $n]` (its `to_array` and `from_array`).
macro_rules! glam_array {
    ($($t:ident: $e:ty, $n:literal;)*) => {$(
        impl Describe for glam::$t {
            fn type_name() -> String {
                concat!("glam.", stringify!($t)).into()
            }
            fn layout() -> Ipld {
                <[$e; $n] as Describe>::layout()
            }
            fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
                <[$e; $n] as Describe>::read(&self.to_array(), refs)
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                Ok(glam::$t::from_array(<[$e; $n] as Describe>::from_value(v, ids)?))
            }
            fn flecs() -> Flecs {
                Flecs::Array {
                    name: Self::type_name(),
                    elem: Box::new(<$e as Describe>::flecs()),
                    count: $n,
                    size: core::mem::size_of::<Self>(),
                    align: core::mem::align_of::<Self>(),
                }
            }
        }
    )*};
}

glam_array! {
    Vec2: f32, 2;
    Vec3: f32, 3;
    Vec4: f32, 4;
    DVec2: f64, 2;
    DVec3: f64, 3;
    DVec4: f64, 4;
    IVec2: i32, 2;
    IVec3: i32, 3;
    IVec4: i32, 4;
    UVec2: u32, 2;
    UVec3: u32, 3;
    UVec4: u32, 4;
    I16Vec2: i16, 2;
    I16Vec3: i16, 3;
    I16Vec4: i16, 4;
    U16Vec2: u16, 2;
    U16Vec3: u16, 3;
    U16Vec4: u16, 4;
    I64Vec2: i64, 2;
    I64Vec3: i64, 3;
    I64Vec4: i64, 4;
    U64Vec2: u64, 2;
    U64Vec3: u64, 3;
    U64Vec4: u64, 4;
    Quat: f32, 4;
    DQuat: f64, 4;
}
use crate::{Describe, Flecs, FlecsMember, FlecsStruct, Ipld, R, Refs, Resolve, member_err};

macro_rules! glam_struct {
    ($($t:ident { $($f:ident: $ft:ty),+ $(,)? })*) => {$(
        impl Describe for glam::$t {
            fn type_name() -> String {
                concat!("glam.", stringify!($t)).into()
            }
            fn layout() -> Ipld {
                struct_layout(
                    Some(Self::type_name()),
                    vec![$((stringify!($f).into(), <$ft as Describe>::layout(), None)),+],
                )
            }
            fn read(&self, refs: &mut dyn Refs) -> R<Ipld> {
                Ok(struct_value(vec![$((
                    stringify!($f),
                    <$ft as Describe>::read(&self.$f, refs).map_err(|e| member_err(stringify!($f), e))?,
                )),+]))
            }
            fn from_value(v: &Ipld, ids: &mut dyn Resolve) -> R<Self> {
                let m = take_struct(v, &[$(stringify!($f)),+])?;
                let mut out = <glam::$t as Default>::default();
                $(
                    out.$f = <$ft as Describe>::from_value(get(m, stringify!($f))?, ids)
                        .map_err(|e| member_err(stringify!($f), e))?;
                )+
                Ok(out)
            }
            fn flecs() -> Flecs {
                let v = <glam::$t as Default>::default();
                Flecs::Struct(FlecsStruct {
                    name: Self::type_name(),
                    size: core::mem::size_of::<Self>(),
                    align: core::mem::align_of::<Self>(),
                    members: vec![$(FlecsMember {
                        name: stringify!($f).into(),
                        offset: offset_in(&v, &v.$f),
                        size: core::mem::size_of::<$ft>(),
                        ty: <$ft as Describe>::flecs(),
                    }),+],
                })
            }
        }
    )*};
}

glam_struct! {
    Vec3A { x: f32, y: f32, z: f32 }
    BVec2 { x: bool, y: bool }
    BVec3 { x: bool, y: bool, z: bool }
    BVec4 { x: bool, y: bool, z: bool, w: bool }
    Mat2 { x_axis: glam::Vec2, y_axis: glam::Vec2 }
    Mat3 { x_axis: glam::Vec3, y_axis: glam::Vec3, z_axis: glam::Vec3 }
    Mat3A { x_axis: glam::Vec3A, y_axis: glam::Vec3A, z_axis: glam::Vec3A }
    Mat4 { x_axis: glam::Vec4, y_axis: glam::Vec4, z_axis: glam::Vec4, w_axis: glam::Vec4 }
    DMat2 { x_axis: glam::DVec2, y_axis: glam::DVec2 }
    DMat3 { x_axis: glam::DVec3, y_axis: glam::DVec3, z_axis: glam::DVec3 }
    DMat4 { x_axis: glam::DVec4, y_axis: glam::DVec4, z_axis: glam::DVec4, w_axis: glam::DVec4 }
    Affine2 { matrix2: glam::Mat2, translation: glam::Vec2 }
    Affine3A { matrix3: glam::Mat3A, translation: glam::Vec3A }
    DAffine2 { matrix2: glam::DMat2, translation: glam::DVec2 }
    DAffine3 { matrix3: glam::DMat3, translation: glam::DVec3 }
}
