/-
The hybrid logical clock: `hlc_lt` (as translated by Aeneas) is a strict total order, and `hlc_send`
and `hlc_recv` move the clock strictly forward.
-/
import Merge.Basic
import Merge.Spec
import Mathlib.Order.Prod.Lex.Basic

open Aeneas Aeneas.Std Result

namespace merge

/-- The order key of a stamp: (wall, counter, actor), lexicographic. -/
abbrev Key := Lex (Nat × Lex (Nat × Nat))

def Hlc.key (h : Hlc) : Key := toLex (h.wall.val, toLex (h.counter.val, h.actor.val))

theorem Hlc.key_lt (a b : Hlc) : a.key < b.key ↔
    a.wall.val < b.wall.val ∨ (a.wall.val = b.wall.val ∧
      (a.counter.val < b.counter.val ∨
        (a.counter.val = b.counter.val ∧ a.actor.val < b.actor.val))) := by
  simp [Hlc.key, Prod.Lex.toLex_lt_toLex]

theorem Hlc.key_inj : Function.Injective Hlc.key := by
  rintro ⟨w1, c1, a1⟩ ⟨w2, c2, a2⟩ h
  simp only [Hlc.key, toLex_inj, Prod.mk.injEq] at h
  obtain ⟨hw, hc, ha⟩ := h
  rw [u64_val_inj hw, u32_val_inj hc, u64_val_inj ha]

/-- `hlc_lt` decides the key order. -/
theorem hlc_lt_spec (a b : Hlc) : hlc_lt a b = ok (decide (a.key < b.key)) := by
  simp only [hlc_lt, UScalar.lt_equiv, Hlc.key_lt]
  split_ifs <;>
    simp only [Result.ok.injEq, eq_comm (a := true), eq_comm (a := false), decide_eq_true_eq,
      decide_eq_false_iff_not, decide_eq_decide] <;> omega

/-! ### `hlc_lt` is a strict total order -/

theorem hlc_lt_irrefl (a : Hlc) : hlc_lt a a = ok false := by
  simp [hlc_lt_spec]

theorem hlc_lt_trans {a b c : Hlc} (hab : hlc_lt a b = ok true) (hbc : hlc_lt b c = ok true) :
    hlc_lt a c = ok true := by
  simp only [hlc_lt_spec, Result.ok.injEq, decide_eq_true_eq] at *
  exact lt_trans hab hbc

theorem hlc_lt_asymm {a b : Hlc} (hab : hlc_lt a b = ok true) : hlc_lt b a = ok false := by
  simp only [hlc_lt_spec, Result.ok.injEq, decide_eq_true_eq, decide_eq_false_iff_not] at *
  exact not_lt_of_gt hab

theorem hlc_lt_total {a b : Hlc} (hne : a ≠ b) : hlc_lt a b = ok true ∨ hlc_lt b a = ok true := by
  simp only [hlc_lt_spec, Result.ok.injEq, decide_eq_true_eq]
  rcases lt_trichotomy a.key b.key with h | h | h
  · exact Or.inl h
  · exact absurd (Hlc.key_inj h) hne
  · exact Or.inr h

/-! ### The clock moves forward -/

theorem hlc_bump_spec (w : U64) (c : U32) (a : U64) :
    hlc_bump w c a = ok (.Err .Overflow) ∨
    ∃ h, hlc_bump w c a = ok (.Ok h) ∧ h.wall = w ∧ h.counter.val = c.val + 1 ∧ h.actor = a := by
  unfold hlc_bump
  by_cases h2 : c = core.num.U32.MAX
  · left; rw [ite_eq_left h2]
  · right
    rw [ite_eq_right h2]
    have hlt : c.val + (1#u32).val ≤ U32.max := by
      have := u32_le_max c
      have hne : c.val ≠ U32.max := fun h => h2 (u32_val_inj (by rw [h, u32_max_val]))
      simp; omega
    obtain ⟨z, hz, hzv⟩ := u32_add_ok c 1#u32 hlt
    exact ⟨{ wall := w, counter := z, actor := a }, by rw [hz, bind_ok], rfl, by simp [hzv], rfl⟩

/-- A send either overflows the counter or returns a stamp greater than `local`, at or after `now`,
with the same actor. -/
theorem hlc_send_spec (l : Hlc) (now : U64) :
    hlc_send l now = ok (.Err .Overflow) ∨
    ∃ h, hlc_send l now = ok (.Ok h) ∧ hlc_lt l h = ok true ∧ now.val ≤ h.wall.val ∧
      h.actor = l.actor := by
  unfold hlc_send
  by_cases h1 : l.wall < now
  · right
    refine ⟨{ l with wall := now, counter := 0#u32 }, by rw [ite_eq_left h1], ?_, by simp, rfl⟩
    rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt]; simp at h1; simp [h1])]
  · rw [ite_eq_right h1]
    simp at h1
    rcases hlc_bump_spec l.wall l.counter l.actor with h | ⟨h, hh, hw, hc, ha⟩
    · exact Or.inl h
    · refine Or.inr ⟨h, hh, ?_, by rw [hw]; exact h1, ha⟩
      rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt, hw, hc]; simp)]

theorem max_u64_ok (a b : U64) :
    ∃ m, max_u64 a b = ok m ∧ m.val = max a.val b.val := by
  unfold max_u64
  by_cases h : a < b
  · refine ⟨b, by rw [ite_eq_left h], ?_⟩; simp at h; omega
  · refine ⟨a, by rw [ite_eq_right h], ?_⟩; simp at h; omega

theorem max_u32_ok (a b : U32) :
    ∃ m, max_u32 a b = ok m ∧ m.val = max a.val b.val := by
  unfold max_u32
  by_cases h : a < b
  · refine ⟨b, by rw [ite_eq_left h], ?_⟩; simp at h; omega
  · refine ⟨a, by rw [ite_eq_right h], ?_⟩; simp at h; omega

/-- A receive either overflows the counter or returns a stamp greater than `local` and greater than
`remote` (whatever the actors), at or after `now`, with `local`'s actor. -/
theorem hlc_recv_spec (l r : Hlc) (now : U64) :
    hlc_recv l r now = ok (.Err .Overflow) ∨
    ∃ h, hlc_recv l r now = ok (.Ok h) ∧ hlc_lt l h = ok true ∧ hlc_lt r h = ok true ∧
      now.val ≤ h.wall.val ∧ h.actor = l.actor := by
  unfold hlc_recv
  obtain ⟨m1, hm1, hm1v⟩ := max_u64_ok l.wall r.wall
  obtain ⟨w, hw, hwv⟩ := max_u64_ok m1 now
  rw [hm1, bind_ok, hw, bind_ok]
  by_cases hl : w = l.wall
  · rw [ite_eq_left hl]
    by_cases hr : w = r.wall
    · rw [ite_eq_left hr]
      obtain ⟨c, hc, hcv⟩ := max_u32_ok l.counter r.counter
      rw [hc, bind_ok]
      rcases hlc_bump_spec w c l.actor with h | ⟨h, hh, hhw, hhc, hha⟩
      · exact Or.inl h
      · refine Or.inr ⟨h, hh, ?_, ?_, by rw [hhw]; omega, hha⟩
        · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt, hhw, hhc, ← hl]; omega)]
        · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt, hhw, hhc, ← hr]; omega)]
    · rw [ite_eq_right hr]
      have hr' : w.val ≠ r.wall.val := fun h => hr (u64_val_inj h)
      rcases hlc_bump_spec w l.counter l.actor with h | ⟨h, hh, hhw, hhc, hha⟩
      · exact Or.inl h
      · refine Or.inr ⟨h, hh, ?_, ?_, by rw [hhw]; omega, hha⟩
        · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt, hhw, hhc, ← hl]; omega)]
        · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt, hhw]; omega)]
  · rw [ite_eq_right hl]
    have hl' : w.val ≠ l.wall.val := fun h => hl (u64_val_inj h)
    by_cases hr : w = r.wall
    · rw [ite_eq_left hr]
      rcases hlc_bump_spec w r.counter l.actor with h | ⟨h, hh, hhw, hhc, hha⟩
      · exact Or.inl h
      · refine Or.inr ⟨h, hh, ?_, ?_, by rw [hhw]; omega, hha⟩
        · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt, hhw]; omega)]
        · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt, hhw, hhc, ← hr]; omega)]
    · rw [ite_eq_right hr]
      have hr' : w.val ≠ r.wall.val := fun h => hr (u64_val_inj h)
      refine Or.inr ⟨{ l with wall := w, counter := 0#u32 }, rfl, ?_, ?_, by simp; omega, rfl⟩
      · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt]; simp; omega)]
      · rw [hlc_lt_spec, decide_eq_true (by rw [Hlc.key_lt]; simp; omega)]

end merge
