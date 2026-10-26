//! glam's vectors, quaternions, matrices and affine transforms as structs named `glam.<Type>` with their fields as
//! members (`glam.Vec3 {x, y, z}`, `glam.Mat3 {x_axis, y_axis, z_axis}`, `glam.Affine3A {matrix3, translation}`).
//!
//! The SIMD types (`Vec3A`, `Vec4`, `Quat`, `Mat2`, `Mat3A`, `Mat4`, `Affine3A` on targets with SIMD) reach their
//! fields through `Deref`, so member offsets are measured on a value rather than taken with `offset_of!`; a SIMD
//! type's Flecs struct is its full size (`Vec3A` is 16 bytes with three members). `BVec3A` and `BVec4A` are masks
//! with no stable memory layout and are not described.

use crate::__private::{get, offset_in, struct_layout, struct_value, take_struct};
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
    Vec2 { x: f32, y: f32 }
    Vec3 { x: f32, y: f32, z: f32 }
    Vec3A { x: f32, y: f32, z: f32 }
    Vec4 { x: f32, y: f32, z: f32, w: f32 }
    DVec2 { x: f64, y: f64 }
    DVec3 { x: f64, y: f64, z: f64 }
    DVec4 { x: f64, y: f64, z: f64, w: f64 }
    IVec2 { x: i32, y: i32 }
    IVec3 { x: i32, y: i32, z: i32 }
    IVec4 { x: i32, y: i32, z: i32, w: i32 }
    UVec2 { x: u32, y: u32 }
    UVec3 { x: u32, y: u32, z: u32 }
    UVec4 { x: u32, y: u32, z: u32, w: u32 }
    I16Vec2 { x: i16, y: i16 }
    I16Vec3 { x: i16, y: i16, z: i16 }
    I16Vec4 { x: i16, y: i16, z: i16, w: i16 }
    U16Vec2 { x: u16, y: u16 }
    U16Vec3 { x: u16, y: u16, z: u16 }
    U16Vec4 { x: u16, y: u16, z: u16, w: u16 }
    I64Vec2 { x: i64, y: i64 }
    I64Vec3 { x: i64, y: i64, z: i64 }
    I64Vec4 { x: i64, y: i64, z: i64, w: i64 }
    U64Vec2 { x: u64, y: u64 }
    U64Vec3 { x: u64, y: u64, z: u64 }
    U64Vec4 { x: u64, y: u64, z: u64, w: u64 }
    BVec2 { x: bool, y: bool }
    BVec3 { x: bool, y: bool, z: bool }
    BVec4 { x: bool, y: bool, z: bool, w: bool }
    Quat { x: f32, y: f32, z: f32, w: f32 }
    DQuat { x: f64, y: f64, z: f64, w: f64 }
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
