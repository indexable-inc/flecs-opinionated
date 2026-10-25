/-
The generated list functions refine the model in `Spec`: each list abstracts to a sorted
`List (key × Nat)` (stamp sets carry `0` as their value), and every generated function returns `ok`
with the abstraction of the model's result. The abstractions are injective, so equal models mean
equal Rust values.
-/
import Merge.Hlc

open Aeneas Aeneas.Std Result

namespace merge

def Stamps.abs : Stamps → List (Key × Nat)
  | .Nil => []
  | .Cons x r => (x.key, 0) :: r.abs

def Tagged.abs : Tagged → List (Key × Nat)
  | .Nil => []
  | .Cons k v r => (k.key, v.val) :: r.abs

def Counts.abs : Counts → List (Nat × Nat)
  | .Nil => []
  | .Cons k v r => (k.val, v.val) :: r.abs

theorem Stamps.abs_inj : ∀ {a b : Stamps}, a.abs = b.abs → a = b
  | .Nil, .Nil, _ => rfl
  | .Nil, .Cons _ _, h => by simp [Stamps.abs] at h
  | .Cons _ _, .Nil, h => by simp [Stamps.abs] at h
  | .Cons x r, .Cons y s, h => by
    simp only [Stamps.abs, List.cons.injEq, Prod.mk.injEq] at h
    rw [Hlc.key_inj h.1.1, Stamps.abs_inj h.2]

theorem Tagged.abs_inj : ∀ {a b : Tagged}, a.abs = b.abs → a = b
  | .Nil, .Nil, _ => rfl
  | .Nil, .Cons _ _ _, h => by simp [Tagged.abs] at h
  | .Cons _ _ _, .Nil, h => by simp [Tagged.abs] at h
  | .Cons x u r, .Cons y v s, h => by
    simp only [Tagged.abs, List.cons.injEq, Prod.mk.injEq] at h
    rw [Hlc.key_inj h.1.1, u64_val_inj h.1.2, Tagged.abs_inj h.2]

theorem Counts.abs_inj : ∀ {a b : Counts}, a.abs = b.abs → a = b
  | .Nil, .Nil, _ => rfl
  | .Nil, .Cons _ _ _, h => by simp [Counts.abs] at h
  | .Cons _ _ _, .Nil, h => by simp [Counts.abs] at h
  | .Cons x u r, .Cons y v s, h => by
    simp only [Counts.abs, List.cons.injEq, Prod.mk.injEq] at h
    rw [u64_val_inj h.1.1, u64_val_inj h.1.2, Counts.abs_inj h.2]

/-! ### Copies are the identity -/

theorem stamps_copy_ok (a : Stamps) : stamps_copy a = ok a := by
  induction a with
  | Nil => rw [stamps_copy]
  | Cons x r ih => rw [stamps_copy]; simp only [ih, bind_ok]

theorem tagged_copy_ok (a : Tagged) : tagged_copy a = ok a := by
  induction a with
  | Nil => rw [tagged_copy]
  | Cons k v r ih => rw [tagged_copy]; simp only [ih, bind_ok]

theorem counts_copy_ok (a : Counts) : counts_copy a = ok a := by
  induction a with
  | Nil => rw [counts_copy]
  | Cons k v r ih => rw [counts_copy]; simp only [ih, bind_ok]

/-! ### Unions -/

theorem max_val (u v : U64) : (if u < v then v else u).val = max u.val v.val := by
  by_cases h : u < v
  · simp only [h, if_true]; simp at h; omega
  · simp only [h, if_false]; simp at h; omega

theorem stamps_union_ok (a b : Stamps) :
    ∃ c, stamps_union a b = ok c ∧ c.abs = Spec.union a.abs b.abs := by
  induction a generalizing b with
  | Nil => exact ⟨b, by rw [stamps_union]; simp only [stamps_copy_ok], by simp [Stamps.abs]⟩
  | Cons x ra iha =>
    induction b with
    | Nil => exact ⟨.Cons x ra, by rw [stamps_union]; simp only [stamps_copy_ok], by simp [Stamps.abs]⟩
    | Cons y rb ihb =>
      rw [stamps_union]
      simp only [hlc_lt_spec, bind_ok, Stamps.abs, Spec.union_cons_cons]
      by_cases h1 : x.key < y.key
      · obtain ⟨c, hc, hca⟩ := iha (.Cons y rb)
        exact ⟨.Cons x c, by simp [h1, hc], by simp [h1, Stamps.abs, hca]⟩
      · by_cases h2 : y.key < x.key
        · obtain ⟨c, hc, hca⟩ := ihb
          refine ⟨.Cons y c, by simp [h1, h2, hc], ?_⟩
          simp only [Stamps.abs] at hca
          simp [h1, h2, Stamps.abs, hca]
        · obtain ⟨c, hc, hca⟩ := iha rb
          exact ⟨.Cons x c, by simp [h1, h2, hc], by simp [h1, h2, Stamps.abs, hca]⟩

theorem tagged_union_ok (a b : Tagged) :
    ∃ c, tagged_union a b = ok c ∧ c.abs = Spec.union a.abs b.abs := by
  induction a generalizing b with
  | Nil => exact ⟨b, by rw [tagged_union]; simp only [tagged_copy_ok], by simp [Tagged.abs]⟩
  | Cons x u ra iha =>
    induction b with
    | Nil =>
      exact ⟨.Cons x u ra, by rw [tagged_union]; simp only [tagged_copy_ok], by simp [Tagged.abs]⟩
    | Cons y v rb ihb =>
      rw [tagged_union]
      simp only [hlc_lt_spec, bind_ok, Tagged.abs, Spec.union_cons_cons]
      by_cases h1 : x.key < y.key
      · obtain ⟨c, hc, hca⟩ := iha (.Cons y v rb)
        exact ⟨.Cons x u c, by simp [h1, hc], by simp [h1, Tagged.abs, hca]⟩
      · by_cases h2 : y.key < x.key
        · obtain ⟨c, hc, hca⟩ := ihb
          refine ⟨.Cons y v c, by simp [h1, h2, hc], ?_⟩
          simp only [Tagged.abs] at hca
          simp [h1, h2, Tagged.abs, hca]
        · obtain ⟨c, hc, hca⟩ := iha rb
          refine ⟨.Cons x (if u < v then v else u) c, ?_, ?_⟩
          · by_cases hu : u < v <;> simp [h1, h2, hc, hu]
          · simp only [h1, h2, decide_false, Bool.false_eq_true, if_false, Tagged.abs, hca, max_val]

theorem counts_union_ok (a b : Counts) :
    ∃ c, counts_union a b = ok c ∧ c.abs = Spec.union a.abs b.abs := by
  induction a generalizing b with
  | Nil => exact ⟨b, by rw [counts_union]; simp only [counts_copy_ok], by simp [Counts.abs]⟩
  | Cons x u ra iha =>
    induction b with
    | Nil =>
      exact ⟨.Cons x u ra, by rw [counts_union]; simp only [counts_copy_ok], by simp [Counts.abs]⟩
    | Cons y v rb ihb =>
      rw [counts_union]
      simp only [bind_ok, Counts.abs, Spec.union_cons_cons]
      by_cases h1 : x < y
      · have h1' : x.val < y.val := by simpa using h1
        obtain ⟨c, hc, hca⟩ := iha (.Cons y v rb)
        exact ⟨.Cons x u c, by simp [h1, hc], by simp [h1', Counts.abs, hca]⟩
      · have h1' : ¬ x.val < y.val := by simpa using h1
        by_cases h2 : y < x
        · have h2' : y.val < x.val := by simpa using h2
          obtain ⟨c, hc, hca⟩ := ihb
          refine ⟨.Cons y v c, by simp [h1, h2, hc], ?_⟩
          simp only [Counts.abs] at hca
          simp [h1', h2', Counts.abs, hca]
        · have h2' : ¬ y.val < x.val := by simpa using h2
          obtain ⟨c, hc, hca⟩ := iha rb
          refine ⟨.Cons x (if u < v then v else u) c, ?_, ?_⟩
          · by_cases hu : u < v <;> simp [h1, h2, hc, hu]
          · simp only [h1', h2', if_false, Counts.abs, hca, max_val]

/-! ### Counter bump -/

/-- `counts_bump` adds `n` to `actor` (model: `Spec.bump`), and reports `Overflow` exactly when the
new count would pass `U64.max`. -/
theorem counts_bump_ok (c : Counts) (actor n : U64) (hs : Spec.Sorted c.abs) :
    ((Spec.get actor.val c.abs).getD 0 + n.val ≤ U64.max →
      ∃ c', counts_bump c actor n = ok (.Ok c') ∧ c'.abs = Spec.bump actor.val n.val c.abs) ∧
    (U64.max < (Spec.get actor.val c.abs).getD 0 + n.val →
      counts_bump c actor n = ok (.Err .Overflow)) := by
  induction c with
  | Nil =>
    refine ⟨fun _ => ⟨.Cons actor n .Nil, by rw [counts_bump], by simp [Counts.abs, Spec.bump]⟩,
      fun h => ?_⟩
    have := u64_le_max n
    simp [Counts.abs, Spec.get] at h; omega
  | Cons k v rest ih =>
    have hs' := Spec.sorted_cons.mp hs
    rw [counts_bump]
    by_cases h1 : actor < k
    · have h1' : actor.val < k.val := by simpa using h1
      have hn : Spec.get actor.val (Counts.abs (.Cons k v rest)) = none :=
        Spec.get_lt (fun q hq => lt_of_lt_of_le h1' (Spec.bound_of_sorted hs le_rfl q hq))
      rw [hn]
      refine ⟨fun _ => ⟨.Cons actor n (.Cons k v rest), by simp [h1, counts_copy_ok], ?_⟩,
        fun h => ?_⟩
      · simp [Counts.abs, Spec.bump, h1']
      · have := u64_le_max n; simp at h; omega
    · have h1' : ¬ actor.val < k.val := by simpa using h1
      by_cases h2 : k < actor
      · have h2' : k.val < actor.val := by simpa using h2
        have hne : k.val ≠ actor.val := ne_of_lt h2'
        have hg : Spec.get actor.val (Counts.abs (.Cons k v rest)) = Spec.get actor.val rest.abs := by
          simp [Counts.abs, Spec.get, hne]
        rw [hg]
        obtain ⟨ihok, iherr⟩ := ih hs'.2
        refine ⟨fun h => ?_, fun h => ?_⟩
        · obtain ⟨c', hc', hca⟩ := ihok h
          exact ⟨.Cons k v c', by simp [h1, h2, hc'], by simp [Counts.abs, Spec.bump, h1', h2', hca]⟩
        · simp [h1, h2, iherr h]
      · have hk : k = actor := u64_val_inj (by simp at h1 h2; omega)
        subst hk
        have hg : Spec.get k.val (Counts.abs (.Cons k v rest)) = some v.val := by
          simp [Counts.abs, Spec.get]
        rw [hg]
        have hle := u64_le_max n
        obtain ⟨i, hi, hiv⟩ := u64_sub_ok core.num.U64.MAX n (by rw [u64_max_val]; exact hle)
        rw [u64_max_val] at hiv
        simp only [h1, h2, if_false, hi, bind_ok, Option.getD_some]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · have hnot : ¬ i < v := by simp; omega
          obtain ⟨z, hz, hzv⟩ := u64_add_ok v n h
          refine ⟨.Cons k z rest, by simp [hnot, hz, counts_copy_ok], ?_⟩
          simp [Counts.abs, Spec.bump, hzv]
        · have hlt : i < v := by simp; omega
          simp [hlt]

end merge
