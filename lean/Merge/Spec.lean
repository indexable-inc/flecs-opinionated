/-
The abstract model every list in `merge/` refines: a finite map kept as a list sorted strictly by key,
with values in `Nat` joined by `max`. Sets are maps whose values are all `0`.

`union` is the sorted merge, `bump` adds to one key. The proof pattern is extensionality: a sorted list
is determined by its lookup function (`ext`), and lookup through `union` is the pointwise join (`get_union`).
So the semilattice laws of `union` follow from those of `Option`-max, which `simp` and `omega` close.

This is the map form of the G-Set / G-Counter join (Shapiro et al. 2011); the Isabelle proofs of
Gomes, Kleppmann et al. (2017) argue op-based convergence the same way, through commutation of
operations on a common state rather than through any network model, which this file leaves out.
-/
import Mathlib.Order.Basic
import Mathlib.Data.List.Pairwise

namespace Spec

variable {K : Type} [LinearOrder K]

/-- Sorted union; a key in both keeps the larger value. -/
def union : List (K × Nat) → List (K × Nat) → List (K × Nat)
  | [], b => b
  | a@(_ :: _), [] => a
  | (x, u) :: ra, (y, v) :: rb =>
    if x < y then (x, u) :: union ra ((y, v) :: rb)
    else if y < x then (y, v) :: union ((x, u) :: ra) rb
    else (x, max u v) :: union ra rb
termination_by a b => a.length + b.length

@[simp] theorem union_nil_left (b : List (K × Nat)) : union [] b = b := by rw [union]

@[simp] theorem union_nil_right (a : List (K × Nat)) : union a [] = a := by
  cases a <;> rw [union]

theorem union_cons_cons (x y : K) (u v : Nat) (ra rb : List (K × Nat)) :
    union ((x, u) :: ra) ((y, v) :: rb) =
      if x < y then (x, u) :: union ra ((y, v) :: rb)
      else if y < x then (y, v) :: union ((x, u) :: ra) rb
      else (x, max u v) :: union ra rb := by
  rw [union]

/-- Add `n` to key `k`, inserting `(k, n)` if `k` is absent. -/
def bump (k : K) (n : Nat) : List (K × Nat) → List (K × Nat)
  | [] => [(k, n)]
  | (x, v) :: r =>
    if k < x then (k, n) :: (x, v) :: r
    else if x < k then (x, v) :: bump k n r
    else (x, v + n) :: r

/-- Strictly ascending keys. -/
def Sorted (l : List (K × Nat)) : Prop := l.Pairwise (fun p q => p.1 < q.1)

def get (k : K) : List (K × Nat) → Option Nat
  | [] => none
  | (x, v) :: r => if x = k then some v else get k r

/-- The join on lookups: `none` is bottom. -/
def oj : Option Nat → Option Nat → Option Nat
  | none, b => b
  | some a, none => some a
  | some a, some b => some (max a b)

@[simp] theorem oj_none_left (b : Option Nat) : oj none b = b := rfl

@[simp] theorem oj_none_right (a : Option Nat) : oj a none = a := by cases a <;> rfl

theorem oj_comm (a b : Option Nat) : oj a b = oj b a := by
  cases a <;> cases b <;> simp [oj, Nat.max_comm]

theorem oj_assoc (a b c : Option Nat) : oj (oj a b) c = oj a (oj b c) := by
  cases a <;> cases b <;> cases c <;> simp [oj, Nat.max_assoc]

theorem oj_idem (a : Option Nat) : oj a a = a := by
  cases a <;> simp [oj]

theorem sorted_nil : Sorted ([] : List (K × Nat)) := List.Pairwise.nil

theorem sorted_cons {p : K × Nat} {l : List (K × Nat)} :
    Sorted (p :: l) ↔ (∀ q ∈ l, p.1 < q.1) ∧ Sorted l := List.pairwise_cons

/-- A key below the head of a sorted list is absent. -/
theorem get_lt {k : K} : ∀ {l : List (K × Nat)}, (∀ q ∈ l, k < q.1) → get k l = none
  | [], _ => rfl
  | (x, v) :: r, h => by
    have hx : k < x := h (x, v) (by simp)
    have : x ≠ k := ne_of_gt hx
    simp only [get, this, ite_false]
    exact get_lt (fun q hq => h q (by simp [hq]))

theorem get_some_mem {k : K} {v : Nat} : ∀ {l : List (K × Nat)}, get k l = some v → (k, v) ∈ l
  | [], h => by simp [get] at h
  | (x, w) :: r, h => by
    by_cases hx : x = k
    · simp [get, hx] at h; subst hx; subst h; simp
    · simp only [get, hx, ite_false] at h; exact List.mem_cons_of_mem _ (get_some_mem h)

theorem keys_union : ∀ (a b : List (K × Nat)) (q : K × Nat), q ∈ union a b →
    (∃ w, (q.1, w) ∈ a) ∨ (∃ w, (q.1, w) ∈ b)
  | [], b, q, h => by right; exact ⟨q.2, by simpa [union] using h⟩
  | p :: ra, [], q, h => by left; exact ⟨q.2, by simpa [union] using h⟩
  | (x, u) :: ra, (y, v) :: rb, q, h => by
    rw [union] at h
    split_ifs at h with h1 h2
    · rcases List.mem_cons.mp h with rfl | h
      · left; exact ⟨u, by simp⟩
      · rcases keys_union ra _ q h with ⟨w, hw⟩ | hw
        · left; exact ⟨w, List.mem_cons_of_mem _ hw⟩
        · right; exact hw
    · rcases List.mem_cons.mp h with rfl | h
      · right; exact ⟨v, by simp⟩
      · rcases keys_union _ rb q h with hw | ⟨w, hw⟩
        · left; exact hw
        · right; exact ⟨w, List.mem_cons_of_mem _ hw⟩
    · rcases List.mem_cons.mp h with rfl | h
      · left; exact ⟨u, by simp⟩
      · rcases keys_union ra rb q h with ⟨w, hw⟩ | ⟨w, hw⟩
        · left; exact ⟨w, List.mem_cons_of_mem _ hw⟩
        · right; exact ⟨w, List.mem_cons_of_mem _ hw⟩
termination_by a b => a.length + b.length

/-- Every key of a sorted list's tail is above a bound that is below its head. -/
theorem bound_of_sorted {k : K} {p : K × Nat} {l : List (K × Nat)} (hs : Sorted (p :: l))
    (hk : k ≤ p.1) : ∀ q ∈ p :: l, k ≤ q.1 := by
  intro q hq
  rcases List.mem_cons.mp hq with rfl | hq
  · exact hk
  · exact le_of_lt (lt_of_le_of_lt hk ((sorted_cons.mp hs).1 q hq))

theorem sorted_union : ∀ (a b : List (K × Nat)), Sorted a → Sorted b → Sorted (union a b)
  | [], b, _, hb => by simpa [union] using hb
  | p :: ra, [], ha, _ => by simpa [union] using ha
  | (x, u) :: ra, (y, v) :: rb, ha, hb => by
    have ha' := sorted_cons.mp ha
    have hb' := sorted_cons.mp hb
    rw [union]
    split_ifs with h1 h2
    · refine sorted_cons.mpr ⟨fun q hq => ?_, sorted_union _ _ ha'.2 hb⟩
      rcases keys_union _ _ q hq with ⟨w, hw⟩ | ⟨w, hw⟩
      · exact ha'.1 (q.1, w) hw
      · exact lt_of_lt_of_le h1 (bound_of_sorted hb le_rfl (q.1, w) hw)
    · refine sorted_cons.mpr ⟨fun q hq => ?_, sorted_union _ _ ha hb'.2⟩
      rcases keys_union _ _ q hq with ⟨w, hw⟩ | ⟨w, hw⟩
      · exact lt_of_lt_of_le h2 (bound_of_sorted ha le_rfl (q.1, w) hw)
      · exact hb'.1 (q.1, w) hw
    · have hxy : x = y := le_antisymm (not_lt.mp h2) (not_lt.mp h1)
      subst hxy
      refine sorted_cons.mpr ⟨fun q hq => ?_, sorted_union _ _ ha'.2 hb'.2⟩
      rcases keys_union _ _ q hq with ⟨w, hw⟩ | ⟨w, hw⟩
      · exact ha'.1 (q.1, w) hw
      · exact hb'.1 (q.1, w) hw
termination_by a b => a.length + b.length

theorem get_head_none {k : K} {p : K × Nat} {l : List (K × Nat)} (hs : Sorted (p :: l))
    (hk : k ≤ p.1) : get k l = none :=
  get_lt (fun q hq => lt_of_le_of_lt hk ((sorted_cons.mp hs).1 q hq))

theorem get_union (k : K) : ∀ (a b : List (K × Nat)), Sorted a → Sorted b →
    get k (union a b) = oj (get k a) (get k b)
  | [], b, _, _ => by rw [union]; rfl
  | p :: ra, [], _, _ => by rw [union]; simp [get]
  | (x, u) :: ra, (y, v) :: rb, ha, hb => by
    have ha' := sorted_cons.mp ha
    have hb' := sorted_cons.mp hb
    rw [union]
    split_ifs with h1 h2
    · rw [get, get_union k ra _ ha'.2 hb]
      by_cases hx : x = k
      · subst hx
        have : get x ((y, v) :: rb) = none := get_lt (fun q hq => lt_of_lt_of_le h1
          (bound_of_sorted hb le_rfl q hq))
        rw [this]; simp [get]
      · simp [get, hx]
    · rw [get, get_union k _ rb ha hb'.2]
      by_cases hy : y = k
      · subst hy
        have : get y ((x, u) :: ra) = none := get_lt (fun q hq => lt_of_lt_of_le h2
          (bound_of_sorted ha le_rfl q hq))
        rw [this]; simp [get, oj]
      · simp [get, hy]
    · have hxy : x = y := le_antisymm (not_lt.mp h2) (not_lt.mp h1)
      subst hxy
      rw [get, get_union k ra rb ha'.2 hb'.2]
      by_cases hx : x = k
      · subst hx
        rw [get_head_none ha le_rfl, get_head_none hb le_rfl]
        simp [get, oj]
      · simp [get, hx]
termination_by a b => a.length + b.length

/-- A sorted list is determined by its lookups. -/
theorem ext : ∀ (a b : List (K × Nat)), Sorted a → Sorted b → (∀ k, get k a = get k b) → a = b
  | [], [], _, _, _ => rfl
  | [], (y, v) :: rb, _, _, h => by have := h y; simp [get] at this
  | (x, u) :: ra, [], _, _, h => by have := h x; simp [get] at this
  | (x, u) :: ra, (y, v) :: rb, ha, hb, h => by
    have ha' := sorted_cons.mp ha
    have hb' := sorted_cons.mp hb
    -- x is a key of b, so x ≥ y; symmetrically y ≥ x.
    have hxb : (x, u) ∈ (y, v) :: rb := get_some_mem (by rw [← h x]; simp [get])
    have hya : (y, v) ∈ (x, u) :: ra := get_some_mem (by rw [h y]; simp [get])
    have hyx : y ≤ x := by
      rcases List.mem_cons.mp hxb with he | hm
      · exact le_of_eq (congrArg Prod.fst he).symm
      · exact le_of_lt (hb'.1 _ hm)
    have hxy : x ≤ y := by
      rcases List.mem_cons.mp hya with he | hm
      · exact le_of_eq (congrArg Prod.fst he).symm
      · exact le_of_lt (ha'.1 _ hm)
    have hxy' : x = y := le_antisymm hxy hyx
    subst hxy'
    have huv : u = v := by have := h x; simpa [get] using this
    subst huv
    congr 1
    refine ext ra rb ha'.2 hb'.2 (fun k => ?_)
    by_cases hk : x = k
    · subst hk; rw [get_head_none ha le_rfl, get_head_none hb le_rfl]
    · have := h k; simpa [get, hk] using this

theorem union_comm (a b : List (K × Nat)) (ha : Sorted a) (hb : Sorted b) :
    union a b = union b a :=
  ext _ _ (sorted_union _ _ ha hb) (sorted_union _ _ hb ha) (fun k => by
    rw [get_union k _ _ ha hb, get_union k _ _ hb ha, oj_comm])

theorem union_assoc (a b c : List (K × Nat)) (ha : Sorted a) (hb : Sorted b) (hc : Sorted c) :
    union (union a b) c = union a (union b c) :=
  ext _ _ (sorted_union _ _ (sorted_union _ _ ha hb) hc)
    (sorted_union _ _ ha (sorted_union _ _ hb hc)) (fun k => by
    rw [get_union k _ _ (sorted_union _ _ ha hb) hc, get_union k _ _ ha hb,
      get_union k _ _ ha (sorted_union _ _ hb hc), get_union k _ _ hb hc, oj_assoc])

theorem union_idem (a : List (K × Nat)) (ha : Sorted a) : union a a = a :=
  ext _ _ (sorted_union _ _ ha ha) ha (fun k => by rw [get_union k _ _ ha ha, oj_idem])

/-! ### `bump` -/

theorem keys_bump (k : K) (n : Nat) : ∀ (l : List (K × Nat)) (q : K × Nat), q ∈ bump k n l →
    q.1 = k ∨ ∃ w, (q.1, w) ∈ l
  | [], q, h => by left; simp [bump] at h; simp [h]
  | (x, v) :: r, q, h => by
    rw [bump] at h
    split_ifs at h with h1 h2
    · rcases List.mem_cons.mp h with rfl | h
      · left; rfl
      · right; exact ⟨q.2, h⟩
    · rcases List.mem_cons.mp h with rfl | h
      · right; exact ⟨v, by simp⟩
      · rcases keys_bump k n r q h with h | ⟨w, hw⟩
        · left; exact h
        · right; exact ⟨w, List.mem_cons_of_mem _ hw⟩
    · rcases List.mem_cons.mp h with rfl | h
      · right; exact ⟨v, by simp⟩
      · right; exact ⟨q.2, List.mem_cons_of_mem _ h⟩

theorem sorted_bump (k : K) (n : Nat) : ∀ (l : List (K × Nat)), Sorted l → Sorted (bump k n l)
  | [], _ => by simp [bump, Sorted]
  | (x, v) :: r, hl => by
    have hl' := sorted_cons.mp hl
    rw [bump]
    split_ifs with h1 h2
    · refine sorted_cons.mpr ⟨fun q hq => ?_, hl⟩
      exact lt_of_lt_of_le h1 (bound_of_sorted hl le_rfl q hq)
    · refine sorted_cons.mpr ⟨fun q hq => ?_, sorted_bump k n r hl'.2⟩
      rcases keys_bump k n r q hq with h | ⟨w, hw⟩
      · rw [h]; exact h2
      · exact hl'.1 (q.1, w) hw
    · exact sorted_cons.mpr ⟨fun q hq => hl'.1 q hq, hl'.2⟩

theorem get_bump (k k' : K) (n : Nat) : ∀ (l : List (K × Nat)), Sorted l →
    get k' (bump k n l) = if k = k' then some ((get k l).getD 0 + n) else get k' l
  | [], _ => by by_cases h : k = k' <;> simp [bump, get, h]
  | (x, v) :: r, hl => by
    have hl' := sorted_cons.mp hl
    by_cases h1 : k < x
    · have hn : get k ((x, v) :: r) = none := get_lt (fun q hq =>
        lt_of_lt_of_le h1 (bound_of_sorted hl le_rfl q hq))
      rw [bump, ite_eq_left h1]
      by_cases hk : k = k'
      · subst hk; rw [ite_eq_left rfl, hn]; simp [get]
      · rw [ite_eq_right hk]; simp [get, hk]
    · by_cases h2 : x < k
      · have hxk : x ≠ k := ne_of_lt h2
        rw [bump, ite_eq_right h1, ite_eq_left h2, get, get_bump k k' n r hl'.2]
        by_cases hk : k = k'
        · subst hk; simp [get, hxk]
        · simp [get, hk]
      · have hxk : x = k := le_antisymm (not_lt.mp h1) (not_lt.mp h2)
        subst hxk
        rw [bump, ite_eq_right h1, ite_eq_right h2]
        by_cases hk : x = k'
        · subst hk; simp [get]
        · simp [get, hk]

theorem bump_comm (k1 k2 : K) (n1 n2 : Nat) (l : List (K × Nat)) (hl : Sorted l) :
    bump k1 n1 (bump k2 n2 l) = bump k2 n2 (bump k1 n1 l) := by
  refine ext _ _ (sorted_bump _ _ _ (sorted_bump _ _ _ hl))
    (sorted_bump _ _ _ (sorted_bump _ _ _ hl)) (fun k => ?_)
  rw [get_bump _ _ _ _ (sorted_bump _ _ _ hl), get_bump _ _ _ _ hl,
    get_bump _ _ _ _ (sorted_bump _ _ _ hl), get_bump _ _ _ _ hl,
    get_bump _ _ _ _ hl, get_bump _ _ _ _ hl]
  by_cases h1 : k1 = k <;> by_cases h2 : k2 = k
  · subst h1; subst h2; simp; omega
  · subst h1; simp [h2, Ne.symm h2]
  · subst h2; simp [h1, Ne.symm h1]
  · simp [h1, h2]

/-! ### Max by an injective key: the LastWriter join -/

section MaxBy

variable {α β : Type} [LinearOrder β] (f : α → β)

/-- The greater of two elements under the key `f`. -/
def maxBy (a b : α) : α := if f a < f b then b else a

variable {f}

theorem maxBy_comm (hf : Function.Injective f) (a b : α) : maxBy f a b = maxBy f b a := by
  unfold maxBy
  rcases lt_trichotomy (f a) (f b) with h | h | h
  · simp [h, not_lt.mpr (le_of_lt h)]
  · have := hf h; subst this; simp
  · simp [h, not_lt.mpr (le_of_lt h)]

theorem maxBy_assoc (a b c : α) : maxBy f (maxBy f a b) c = maxBy f a (maxBy f b c) := by
  unfold maxBy
  by_cases hab : f a < f b <;> by_cases hbc : f b < f c <;> by_cases hac : f a < f c <;>
    simp only [hab, hbc, hac, if_true, if_false] <;>
    first
    | rfl
    | (exfalso; exact absurd (lt_trans hab hbc) hac)
    | (exfalso; exact absurd (lt_of_le_of_lt (not_lt.mp hab) hac) hbc)
    | (exfalso; exact absurd (lt_of_lt_of_le hac (not_lt.mp hbc)) hab)

theorem maxBy_idem (a : α) : maxBy f a a = a := by simp [maxBy]

end MaxBy

end Spec
