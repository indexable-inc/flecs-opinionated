/- Small facts about the Aeneas primitives the generated code uses. -/
import Merge.Funs

open Aeneas Aeneas.Std Result

namespace merge

theorem u64_add_ok (x y : U64) (h : x.val + y.val ≤ U64.max) :
    ∃ z, x + y = ok z ∧ z.val = x.val + y.val := by
  have h' := @UScalar.add_equiv .U64 x y
  split at h'
  next z heq =>
    obtain ⟨_, hval, _⟩ := h'
    simp at heq
    exact ⟨z, heq, hval⟩
  next heq => exfalso; scalar_tac
  next => exact h'.elim

theorem u32_add_ok (x y : U32) (h : x.val + y.val ≤ U32.max) :
    ∃ z, x + y = ok z ∧ z.val = x.val + y.val := by
  have h' := @UScalar.add_equiv .U32 x y
  split at h'
  next z heq =>
    obtain ⟨_, hval, _⟩ := h'
    simp at heq
    exact ⟨z, heq, hval⟩
  next heq => exfalso; scalar_tac
  next => exact h'.elim

theorem u64_sub_ok (x y : U64) (h : y.val ≤ x.val) :
    ∃ z, x - y = ok z ∧ z.val = x.val - y.val := by
  have h' := @UScalar.sub_equiv .U64 x y
  split at h'
  next z heq =>
    obtain ⟨_, hval, _⟩ := h'
    simp at heq
    exact ⟨z, heq, by omega⟩
  next heq => exfalso; scalar_tac
  next => exact h'.elim

theorem u64_max_val : core.num.U64.MAX.val = U64.max := by
  simp [core.num.U64.MAX, U64.rMax, U64.max_eq]

theorem u32_max_val : core.num.U32.MAX.val = U32.max := by
  simp [core.num.U32.MAX, U32.rMax, U32.max_eq]

theorem u64_val_inj {x y : U64} (h : x.val = y.val) : x = y := UScalar.eq_imp x y h

theorem u32_val_inj {x y : U32} (h : x.val = y.val) : x = y := UScalar.eq_imp x y h

theorem u64_le_max (x : U64) : x.val ≤ U64.max := by scalar_tac

theorem u32_le_max (x : U32) : x.val ≤ U32.max := by scalar_tac

end merge
