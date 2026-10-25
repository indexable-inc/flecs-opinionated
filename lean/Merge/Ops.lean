/-
Prepared operations commute: applying `o1` then `o2` to a well-formed state gives the same result
(the same state, or the same error) as `o2` then `o1`.

Every op but `Inc`/`Dec` is a one-entry state joined with `merge` (a delta), so its commutation is
associativity plus commutativity of the join. `Inc`/`Dec` add to one actor's count; they commute by
`Spec.bump_comm`, and overflow is order-independent (`counts_bump_comm`).
-/
import Merge.Laws

open Aeneas Aeneas.Std Result

namespace merge

/-- The ops each rule accepts, with the observed stamps sorted. -/
def Op.fits : Rule → Op → Prop
  | .LastWriter, .Write _ _ => True
  | .AddWins, .Add _ _ => True
  | .AddWins, .Remove s => Spec.Sorted s.abs
  | .Counter, .Inc _ _ => True
  | .Counter, .Dec _ _ => True
  | .KeepBoth, .Put _ _ s => Spec.Sorted s.abs
  | _, _ => False

/-- The one-entry state an op joins in, for every op but `Inc` and `Dec`. -/
def Op.delta : Op → Option Value
  | .Write s v => some (.Register ⟨s, v⟩)
  | .Add t e => some (.Set ⟨.Cons t e .Nil, .Nil⟩)
  | .Remove s => some (.Set ⟨.Nil, s⟩)
  | .Put t v s => some (.Multi ⟨.Cons t v .Nil, s⟩)
  | .Inc _ _ => none
  | .Dec _ _ => none

/-- Continue with `o` after a first step. -/
def andThen (r : Rule) (o : Op) :
    core.result.Result Value Error → Std.Result (core.result.Result Value Error)
  | .Ok s => apply r s o
  | .Err e => ok (.Err e)

theorem apply_delta (r : Rule) (s : Value) (o : Op) (d : Value) (h : o.delta = some d) :
    apply r s o = merge r s d := by
  cases o <;> simp only [Op.delta, Option.some.injEq, reduceCtorEq] at h <;> subst h <;>
    simp [apply, stamps_copy_ok]

theorem delta_wf (r : Rule) (o : Op) (d : Value) (hf : o.fits r) (h : o.delta = some d) :
    Value.wf r d := by
  cases r <;> cases o <;> simp only [Op.fits, Op.delta, Option.some.injEq, reduceCtorEq] at hf h <;>
    subst h <;>
    simp [Value.wf, Tombstoned.wf, Tagged.abs, Stamps.abs, Spec.Sorted] <;> exact hf

/-! ### Counter ops -/

def bumpThen (a n : U64) :
    core.result.Result Counts Error → Std.Result (core.result.Result Counts Error)
  | .Ok c => counts_bump c a n
  | .Err e => ok (.Err e)

/-- `counts_bump` on a sorted map: `Ok` with the model's result when the count stays in range,
`Err Overflow` otherwise. -/
theorem counts_bump_cases (c : Counts) (a n : U64) (hs : Spec.Sorted c.abs) :
    (∃ c', counts_bump c a n = ok (.Ok c') ∧ c'.abs = Spec.bump a.val n.val c.abs ∧
      (Spec.get a.val c.abs).getD 0 + n.val ≤ U64.max) ∨
    (counts_bump c a n = ok (.Err .Overflow) ∧
      U64.max < (Spec.get a.val c.abs).getD 0 + n.val) := by
  obtain ⟨hok, herr⟩ := counts_bump_ok c a n hs
  by_cases h : (Spec.get a.val c.abs).getD 0 + n.val ≤ U64.max
  · obtain ⟨c', h1, h2⟩ := hok h; exact Or.inl ⟨c', h1, h2, h⟩
  · exact Or.inr ⟨herr (by omega), by omega⟩

theorem getD_bump (k k' : Nat) (n : Nat) (l : List (Nat × Nat)) (hl : Spec.Sorted l) :
    (Spec.get k' (Spec.bump k n l)).getD 0 =
      if k = k' then (Spec.get k l).getD 0 + n else (Spec.get k' l).getD 0 := by
  rw [Spec.get_bump k k' n l hl]; split <;> simp

/-- Two bumps of one count map commute, overflow included. -/
theorem counts_bump_comm (c : Counts) (hs : Spec.Sorted c.abs) (a1 n1 a2 n2 : U64) :
    (counts_bump c a1 n1 >>= bumpThen a2 n2) = (counts_bump c a2 n2 >>= bumpThen a1 n1) := by
  have g12 := fun (c' : Counts) (hc' : c'.abs = Spec.bump a1.val n1.val c.abs) => by
    have : (Spec.get a2.val c'.abs).getD 0 = if a1.val = a2.val then
        (Spec.get a1.val c.abs).getD 0 + n1.val else (Spec.get a2.val c.abs).getD 0 := by
      rw [hc', getD_bump _ _ _ _ hs]
    exact this
  have g21 := fun (c' : Counts) (hc' : c'.abs = Spec.bump a2.val n2.val c.abs) => by
    have : (Spec.get a1.val c'.abs).getD 0 = if a2.val = a1.val then
        (Spec.get a2.val c.abs).getD 0 + n2.val else (Spec.get a1.val c.abs).getD 0 := by
      rw [hc', getD_bump _ _ _ _ hs]
    exact this
  have same : a1.val = a2.val → (Spec.get a1.val c.abs).getD 0 = (Spec.get a2.val c.abs).getD 0 :=
    fun h => by rw [h]
  rcases counts_bump_cases c a1 n1 hs with ⟨c1, h1, h1a, h1m⟩ | ⟨h1, h1m⟩ <;>
    rcases counts_bump_cases c a2 n2 hs with ⟨c2, h2, h2a, h2m⟩ | ⟨h2, h2m⟩ <;>
    rw [h1, h2] <;> simp only [bind_tc_ok, bumpThen]
  · have s1 : Spec.Sorted c1.abs := by rw [h1a]; exact Spec.sorted_bump _ _ _ hs
    have s2 : Spec.Sorted c2.abs := by rw [h2a]; exact Spec.sorted_bump _ _ _ hs
    have e1 := g12 c1 h1a
    have e2 := g21 c2 h2a
    rcases counts_bump_cases c1 a2 n2 s1 with ⟨d1, k1, k1a, k1m⟩ | ⟨k1, k1m⟩ <;>
      rcases counts_bump_cases c2 a1 n1 s2 with ⟨d2, k2, k2a, k2m⟩ | ⟨k2, k2m⟩ <;>
      rw [k1, k2]
    · rw [Counts.abs_inj (a := d1) (b := d2) (by
        rw [k1a, k2a, h1a, h2a, Spec.bump_comm _ _ _ _ _ hs])]
    · exfalso; rw [e2] at k2m; rw [e1] at k1m
      by_cases h : a1.val = a2.val
      · have := same h; simp only [h, if_true] at k1m k2m; omega
      · simp only [h, Ne.symm h, if_false] at k1m k2m; omega
    · exfalso; rw [e2] at k2m; rw [e1] at k1m
      by_cases h : a1.val = a2.val
      · have := same h; simp only [h, if_true] at k1m k2m; omega
      · simp only [h, Ne.symm h, if_false] at k1m k2m; omega
  · have s1 : Spec.Sorted c1.abs := by rw [h1a]; exact Spec.sorted_bump _ _ _ hs
    have e1 := g12 c1 h1a
    rcases counts_bump_cases c1 a2 n2 s1 with ⟨d1, k1, k1a, k1m⟩ | ⟨k1, k1m⟩ <;> rw [k1]
    exfalso; rw [e1] at k1m
    by_cases h : a1.val = a2.val
    · have := same h; simp only [h, if_true] at k1m; omega
    · simp only [h, if_false] at k1m; omega
  · have s2 : Spec.Sorted c2.abs := by rw [h2a]; exact Spec.sorted_bump _ _ _ hs
    have e2 := g21 c2 h2a
    rcases counts_bump_cases c2 a1 n1 s2 with ⟨d2, k2, k2a, k2m⟩ | ⟨k2, k2m⟩ <;> rw [k2]
    exfalso; rw [e2] at k2m
    by_cases h : a2.val = a1.val
    · have := same h.symm; simp only [h, if_true] at k2m; omega
    · simp only [h, if_false] at k2m; omega

/-- A counter state with a step on one side. -/
def liftInc (dec : Counts) : core.result.Result Counts Error → core.result.Result Value Error
  | .Ok c => .Ok (.Counter ⟨c, dec⟩)
  | .Err e => .Err e

def liftDec (inc : Counts) : core.result.Result Counts Error → core.result.Result Value Error
  | .Ok c => .Ok (.Counter ⟨inc, c⟩)
  | .Err e => .Err e

theorem apply_inc (pn : Pn) (a n : U64) (r : core.result.Result Counts Error)
    (h : counts_bump pn.inc a n = ok r) :
    apply .Counter (.Counter pn) (.Inc a n) = ok (liftInc pn.dec r) := by
  cases r <;> simp [apply, h, counts_copy_ok, liftInc]

theorem apply_dec (pn : Pn) (a n : U64) (r : core.result.Result Counts Error)
    (h : counts_bump pn.dec a n = ok r) :
    apply .Counter (.Counter pn) (.Dec a n) = ok (liftDec pn.inc r) := by
  cases r <;> simp [apply, h, counts_copy_ok, liftDec]

theorem counts_bump_some (c : Counts) (a n : U64) (hs : Spec.Sorted c.abs) :
    ∃ r, counts_bump c a n = ok r ∧ (r = .Err .Overflow ∨ ∃ c', r = .Ok c' ∧ Spec.Sorted c'.abs) := by
  rcases counts_bump_cases c a n hs with ⟨c', h, ha, -⟩ | ⟨h, -⟩
  · exact ⟨_, h, Or.inr ⟨c', rfl, by rw [ha]; exact Spec.sorted_bump _ _ _ hs⟩⟩
  · exact ⟨_, h, Or.inl rfl⟩

theorem bumpThen_some (r : core.result.Result Counts Error) (a n : U64)
    (hr : r = .Err .Overflow ∨ ∃ c', r = .Ok c' ∧ Spec.Sorted c'.abs) :
    ∃ R, bumpThen a n r = ok R := by
  rcases hr with rfl | ⟨c', rfl, hs⟩
  · exact ⟨_, rfl⟩
  · obtain ⟨R, h, -⟩ := counts_bump_some c' a n hs; exact ⟨R, h⟩

/-- Two `Inc`s commute. -/
theorem inc_inc (pn : Pn) (hw : pn.wf) (a1 n1 a2 n2 : U64) :
    (apply .Counter (.Counter pn) (.Inc a1 n1) >>= andThen .Counter (.Inc a2 n2)) =
    (apply .Counter (.Counter pn) (.Inc a2 n2) >>= andThen .Counter (.Inc a1 n1)) := by
  have key := counts_bump_comm pn.inc hw.1 a1 n1 a2 n2
  obtain ⟨r1, h1, hr1⟩ := counts_bump_some pn.inc a1 n1 hw.1
  obtain ⟨r2, h2, hr2⟩ := counts_bump_some pn.inc a2 n2 hw.1
  obtain ⟨R1, hR1⟩ := bumpThen_some r1 a2 n2 hr1
  obtain ⟨R2, hR2⟩ := bumpThen_some r2 a1 n1 hr2
  rw [h1, h2, bind_tc_ok, bind_tc_ok, hR1, hR2, Result.ok.injEq] at key
  subst key
  rw [apply_inc pn a1 n1 r1 h1, apply_inc pn a2 n2 r2 h2, bind_tc_ok, bind_tc_ok]
  have step : ∀ r a n R, bumpThen a n r = ok R →
      andThen .Counter (.Inc a n) (liftInc pn.dec r) = ok (liftInc pn.dec R) := by
    intro r a n R h
    cases r with
    | Ok c => exact apply_inc ⟨c, pn.dec⟩ a n R h
    | Err e => simp only [bumpThen, Result.ok.injEq] at h; subst h; rfl
  rw [step r1 a2 n2 R1 hR1, step r2 a1 n1 R1 hR2]

/-- Two `Dec`s commute. -/
theorem dec_dec (pn : Pn) (hw : pn.wf) (a1 n1 a2 n2 : U64) :
    (apply .Counter (.Counter pn) (.Dec a1 n1) >>= andThen .Counter (.Dec a2 n2)) =
    (apply .Counter (.Counter pn) (.Dec a2 n2) >>= andThen .Counter (.Dec a1 n1)) := by
  have key := counts_bump_comm pn.dec hw.2 a1 n1 a2 n2
  obtain ⟨r1, h1, hr1⟩ := counts_bump_some pn.dec a1 n1 hw.2
  obtain ⟨r2, h2, hr2⟩ := counts_bump_some pn.dec a2 n2 hw.2
  obtain ⟨R1, hR1⟩ := bumpThen_some r1 a2 n2 hr1
  obtain ⟨R2, hR2⟩ := bumpThen_some r2 a1 n1 hr2
  rw [h1, h2, bind_tc_ok, bind_tc_ok, hR1, hR2, Result.ok.injEq] at key
  subst key
  rw [apply_dec pn a1 n1 r1 h1, apply_dec pn a2 n2 r2 h2, bind_tc_ok, bind_tc_ok]
  have step : ∀ r a n R, bumpThen a n r = ok R →
      andThen .Counter (.Dec a n) (liftDec pn.inc r) = ok (liftDec pn.inc R) := by
    intro r a n R h
    cases r with
    | Ok c => exact apply_dec ⟨pn.inc, c⟩ a n R h
    | Err e => simp only [bumpThen, Result.ok.injEq] at h; subst h; rfl
  rw [step r1 a2 n2 R1 hR1, step r2 a1 n1 R1 hR2]

/-- An `Inc` and a `Dec` touch different maps, and both fail only with `Overflow`. -/
theorem inc_dec (pn : Pn) (hw : pn.wf) (a1 n1 a2 n2 : U64) :
    (apply .Counter (.Counter pn) (.Inc a1 n1) >>= andThen .Counter (.Dec a2 n2)) =
    (apply .Counter (.Counter pn) (.Dec a2 n2) >>= andThen .Counter (.Inc a1 n1)) := by
  rcases counts_bump_cases pn.inc a1 n1 hw.1 with ⟨i, hi, -, -⟩ | ⟨hi, -⟩ <;>
    rcases counts_bump_cases pn.dec a2 n2 hw.2 with ⟨d, hd, -, -⟩ | ⟨hd, -⟩ <;>
    rw [apply_inc pn a1 n1 _ hi, apply_dec pn a2 n2 _ hd, bind_tc_ok, bind_tc_ok] <;>
    simp only [liftInc, liftDec, andThen]
  · rw [apply_dec ⟨i, pn.dec⟩ a2 n2 _ hd, apply_inc ⟨pn.inc, d⟩ a1 n1 _ hi]; rfl
  · rw [apply_dec ⟨i, pn.dec⟩ a2 n2 _ hd]; rfl
  · rw [apply_inc ⟨pn.inc, d⟩ a1 n1 _ hi]; rfl

/-- Ops commute on every well-formed state, for ops the rule accepts. -/
theorem apply_comm (r : Rule) (s : Value) (o1 o2 : Op) (hs : Value.wf r s)
    (h1 : o1.fits r) (h2 : o2.fits r) :
    (apply r s o1 >>= andThen r o2) = (apply r s o2 >>= andThen r o1) := by
  by_cases hc : r = .Counter
  · subst hc
    cases s <;> simp only [Value.wf] at hs
    rename_i pn
    cases o1 <;> cases o2 <;> simp only [Op.fits] at h1 h2
    · exact inc_inc pn hs _ _ _ _
    · exact inc_dec pn hs _ _ _ _
    · exact (inc_dec pn hs _ _ _ _).symm
    · exact dec_dec pn hs _ _ _ _
  · have hd : ∀ o : Op, o.fits r → ∃ d, o.delta = some d := by
      intro o ho
      cases r <;> cases o <;> simp [Op.fits, Op.delta] at ho hc ⊢
    obtain ⟨d1, hd1⟩ := hd o1 h1
    obtain ⟨d2, hd2⟩ := hd o2 h2
    have w1 := delta_wf r o1 d1 h1 hd1
    have w2 := delta_wf r o2 d2 h2 hd2
    have J := merge_join r
    obtain ⟨s1, hs1, -⟩ := J.total s d1 hs w1
    obtain ⟨s2, hs2, -⟩ := J.total s d2 hs w2
    obtain ⟨e, he, -⟩ := J.total d1 d2 w1 w2
    have he' : merge r d2 d1 = ok (.Ok e) := by rw [J.comm d2 d1 w2 w1]; exact he
    rw [apply_delta r s o1 d1 hd1, hs1, apply_delta r s o2 d2 hd2, hs2, bind_tc_ok, bind_tc_ok]
    simp only [andThen]
    rw [apply_delta r s1 o2 d2 hd2, apply_delta r s2 o1 d1 hd1,
      J.assoc s d1 d2 s1 e hs w1 w2 hs1 he, J.assoc s d2 d1 s2 e hs w2 w1 hs2 he']

/-- Applying an accepted op to a well-formed state gives a well-formed state, or `Overflow`. -/
theorem apply_wf (r : Rule) (s : Value) (o : Op) (hs : Value.wf r s) (ho : o.fits r) :
    apply r s o = ok (.Err .Overflow) ∨ ∃ s', apply r s o = ok (.Ok s') ∧ Value.wf r s' := by
  by_cases hc : r = .Counter
  · subst hc
    cases s <;> simp only [Value.wf] at hs
    rename_i pn
    cases o <;> simp only [Op.fits] at ho
    · rename_i a n
      rcases counts_bump_cases pn.inc a n hs.1 with ⟨i, hi, hia, -⟩ | ⟨hi, -⟩
      · exact Or.inr ⟨_, apply_inc pn a n _ hi,
          ⟨by rw [hia]; exact Spec.sorted_bump _ _ _ hs.1, hs.2⟩⟩
      · exact Or.inl (apply_inc pn a n _ hi)
    · rename_i a n
      rcases counts_bump_cases pn.dec a n hs.2 with ⟨d, hd, hda, -⟩ | ⟨hd, -⟩
      · exact Or.inr ⟨_, apply_dec pn a n _ hd,
          ⟨hs.1, by rw [hda]; exact Spec.sorted_bump _ _ _ hs.2⟩⟩
      · exact Or.inl (apply_dec pn a n _ hd)
  · have hd : ∃ d, o.delta = some d := by
      cases r <;> cases o <;> simp [Op.fits, Op.delta] at ho hc ⊢
    obtain ⟨d, hdd⟩ := hd
    obtain ⟨s', h, hw⟩ := (merge_join r).total s d hs (delta_wf r o d ho hdd)
    exact Or.inr ⟨s', by rw [apply_delta r s o d hdd, h], hw⟩

end merge
