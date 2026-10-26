//! `half::f16` and `half::bf16` as `"f16"` and `"bf16"` (SPEC.md, "Values"): widened to f64 exactly when read,
//! refused when restored from a float they cannot hold exactly. Flecs has no half floats, so they are opaque to it.

use crate::__private::text;
use crate::impls::{float, float_of};
use crate::{Describe, Ipld, R, Refs, Resolve};

macro_rules! half_impl {
    ($t:ty, $name:literal) => {
        impl Describe for $t {
            fn type_name() -> String {
                $name.into()
            }
            fn layout() -> Ipld {
                text($name)
            }
            fn read(&self, _: &mut dyn Refs) -> R<Ipld> {
                float(self.to_f64())
            }
            fn from_value(v: &Ipld, _: &mut dyn Resolve) -> R<Self> {
                let f = float_of(v, $name)?;
                let x = <$t>::from_f64(f);
                if x.to_f64() == f {
                    Ok(x)
                } else {
                    Err(format!(concat!("{} is not exactly an ", $name), f))
                }
            }
        }
    };
}

half_impl!(half::f16, "f16");
half_impl!(half::bf16, "bf16");
