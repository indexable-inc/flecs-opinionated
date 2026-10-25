/-
The merge laws, against the Aeneas translation of `merge/src/lib.rs`.

For every rule, `merge rule` is a join-semilattice on well-formed states (`Join`): it succeeds,
and it is commutative, associative and idempotent. `merge_fields` (a struct component merged
member by member) is one too, for any list of member rules: the field-wise product of semilattices
is a semilattice. Prepared operations commute (`apply_comm`).
-/
import Merge.Refine

open Aeneas Aeneas.Std Result

namespace merge

/-- A join-semilattice on the states satisfying `wf`, for a function in the shape Aeneas gives a
Rust `fn(&T, &T) -> Result<T, Error>`. -/
structure Join {α : Type} (wf : α → Prop) (f : α → α → Std.Result (core.result.Result α Error)) :
    Prop where
  total : ∀ a b, wf a → wf b → ∃ c, f a b = ok (.Ok c) ∧ wf c
  comm : ∀ a b, wf a → wf b → f a b = f b a
  assoc : ∀ a b c ab bc, wf a → wf b → wf c →
    f a b = ok (.Ok ab) → f b c = ok (.Ok bc) → f ab c = f a bc
  idem : ∀ a, wf a → f a a = ok (.Ok a)

/-! ### Well-formed states -/

def Tombstoned.wf (t : Tombstoned) : Prop := Spec.Sorted t.entries.abs ∧ Spec.Sorted t.removed.abs

def Pn.wf (p : Pn) : Prop := Spec.Sorted p.inc.abs ∧ Spec.Sorted p.dec.abs

/-- A well-formed state for a rule: the rule's shape, with every list sorted strictly by key. -/
def Value.wf : Rule → Value → Prop
  | .LastWriter, .Register _ => True
  | .AddWins, .Set t => t.wf
  | .Counter, .Counter p => p.wf
  | .KeepBoth, .Multi t => t.wf
  | _, _ => False

/-! ### LastWriter -/

def Register.key (r : Register) : Lex (Key × Nat) := toLex (r.stamp.key, r.value.val)

theorem Register.key_inj : Function.Injective Register.key := by
  rintro ⟨s1, v1⟩ ⟨s2, v2⟩ h
  simp only [Register.key, toLex_inj, Prod.mk.injEq] at h
  rw [Hlc.key_inj h.1, u64_val_inj h.2]

/-- `register_merge` keeps the greater of the two registers under (stamp, value). -/
theorem register_merge_spec (a b : Register) :
    register_merge a b = ok (Spec.maxBy Register.key a b) := by
  simp only [register_merge, hlc_lt_spec, bind_ok, Spec.maxBy, Register.key,
    Prod.Lex.toLex_lt_toLex]
  by_cases h1 : a.stamp.key < b.stamp.key
  · simp [h1]
  · by_cases h2 : b.stamp.key < a.stamp.key
    · simp [h1, h2, ne_of_gt h2]
    · have he : a.stamp.key = b.stamp.key := le_antisymm (not_lt.mp h2) (not_lt.mp h1)
      by_cases hv : a.value < b.value
      · have hv' : a.value.val < b.value.val := by simpa using hv
        simp [h1, h2, he, hv, hv']
      · have hv' : ¬ a.value.val < b.value.val := by simpa using hv
        simp [h1, h2, he, hv, hv']

/-! ### AddWins and KeepBoth -/

theorem Tombstoned.ext_abs {a b : Tombstoned} (h1 : a.entries.abs = b.entries.abs)
    (h2 : a.removed.abs = b.removed.abs) : a = b := by
  cases a; cases b
  simp only [Tombstoned.mk.injEq]
  exact ⟨Tagged.abs_inj h1, Stamps.abs_inj h2⟩

theorem tombstoned_merge_ok (a b : Tombstoned) :
    ∃ c, tombstoned_merge a b = ok c ∧
      c.entries.abs = Spec.union a.entries.abs b.entries.abs ∧
      c.removed.abs = Spec.union a.removed.abs b.removed.abs := by
  obtain ⟨e, he, hea⟩ := tagged_union_ok a.entries b.entries
  obtain ⟨r, hr, hra⟩ := stamps_union_ok a.removed b.removed
  exact ⟨⟨e, r⟩, by simp [tombstoned_merge, he, hr], hea, hra⟩

theorem tombstoned_total {a b : Tombstoned} (ha : a.wf) (hb : b.wf) :
    ∃ c, tombstoned_merge a b = ok c ∧ c.wf := by
  obtain ⟨c, hc, h1, h2⟩ := tombstoned_merge_ok a b
  exact ⟨c, hc, by rw [Tombstoned.wf, h1, h2]; exact
    ⟨Spec.sorted_union _ _ ha.1 hb.1, Spec.sorted_union _ _ ha.2 hb.2⟩⟩

theorem tombstoned_comm {a b : Tombstoned} (ha : a.wf) (hb : b.wf) :
    tombstoned_merge a b = tombstoned_merge b a := by
  obtain ⟨c, hc, h1, h2⟩ := tombstoned_merge_ok a b
  obtain ⟨d, hd, h3, h4⟩ := tombstoned_merge_ok b a
  rw [hc, hd, Tombstoned.ext_abs (a := c) (b := d)
    (by rw [h1, h3, Spec.union_comm _ _ ha.1 hb.1])
    (by rw [h2, h4, Spec.union_comm _ _ ha.2 hb.2])]

theorem tombstoned_assoc {a b c ab bc : Tombstoned} (ha : a.wf) (hb : b.wf) (hc : c.wf)
    (hab : tombstoned_merge a b = ok ab) (hbc : tombstoned_merge b c = ok bc) :
    tombstoned_merge ab c = tombstoned_merge a bc := by
  obtain ⟨x, hx, h1, h2⟩ := tombstoned_merge_ok a b
  obtain ⟨y, hy, h3, h4⟩ := tombstoned_merge_ok b c
  rw [hab, Result.ok.injEq] at hx; subst hx
  rw [hbc, Result.ok.injEq] at hy; subst hy
  obtain ⟨l, hl, h5, h6⟩ := tombstoned_merge_ok ab c
  obtain ⟨r, hr, h7, h8⟩ := tombstoned_merge_ok a bc
  rw [hl, hr, Tombstoned.ext_abs (a := l) (b := r)
    (by rw [h5, h7, h1, h3, Spec.union_assoc _ _ _ ha.1 hb.1 hc.1])
    (by rw [h6, h8, h2, h4, Spec.union_assoc _ _ _ ha.2 hb.2 hc.2])]

theorem tombstoned_idem {a : Tombstoned} (ha : a.wf) : tombstoned_merge a a = ok a := by
  obtain ⟨c, hc, h1, h2⟩ := tombstoned_merge_ok a a
  rw [hc, Tombstoned.ext_abs (a := c) (b := a)
    (by rw [h1, Spec.union_idem _ ha.1]) (by rw [h2, Spec.union_idem _ ha.2])]

/-! ### Counter -/

theorem Pn.ext_abs {a b : Pn} (h1 : a.inc.abs = b.inc.abs) (h2 : a.dec.abs = b.dec.abs) :
    a = b := by
  cases a; cases b
  simp only [Pn.mk.injEq]
  exact ⟨Counts.abs_inj h1, Counts.abs_inj h2⟩

theorem pn_merge_ok (a b : Pn) :
    ∃ c, pn_merge a b = ok c ∧
      c.inc.abs = Spec.union a.inc.abs b.inc.abs ∧ c.dec.abs = Spec.union a.dec.abs b.dec.abs := by
  obtain ⟨i, hi, hia⟩ := counts_union_ok a.inc b.inc
  obtain ⟨d, hd, hda⟩ := counts_union_ok a.dec b.dec
  exact ⟨⟨i, d⟩, by simp [pn_merge, hi, hd], hia, hda⟩

theorem pn_total {a b : Pn} (ha : a.wf) (hb : b.wf) : ∃ c, pn_merge a b = ok c ∧ c.wf := by
  obtain ⟨c, hc, h1, h2⟩ := pn_merge_ok a b
  exact ⟨c, hc, by rw [Pn.wf, h1, h2]; exact
    ⟨Spec.sorted_union _ _ ha.1 hb.1, Spec.sorted_union _ _ ha.2 hb.2⟩⟩

theorem pn_comm {a b : Pn} (ha : a.wf) (hb : b.wf) : pn_merge a b = pn_merge b a := by
  obtain ⟨c, hc, h1, h2⟩ := pn_merge_ok a b
  obtain ⟨d, hd, h3, h4⟩ := pn_merge_ok b a
  rw [hc, hd, Pn.ext_abs (a := c) (b := d)
    (by rw [h1, h3, Spec.union_comm _ _ ha.1 hb.1])
    (by rw [h2, h4, Spec.union_comm _ _ ha.2 hb.2])]

theorem pn_assoc {a b c ab bc : Pn} (ha : a.wf) (hb : b.wf) (hc : c.wf)
    (hab : pn_merge a b = ok ab) (hbc : pn_merge b c = ok bc) :
    pn_merge ab c = pn_merge a bc := by
  obtain ⟨x, hx, h1, h2⟩ := pn_merge_ok a b
  obtain ⟨y, hy, h3, h4⟩ := pn_merge_ok b c
  rw [hab, Result.ok.injEq] at hx; subst hx
  rw [hbc, Result.ok.injEq] at hy; subst hy
  obtain ⟨l, hl, h5, h6⟩ := pn_merge_ok ab c
  obtain ⟨r, hr, h7, h8⟩ := pn_merge_ok a bc
  rw [hl, hr, Pn.ext_abs (a := l) (b := r)
    (by rw [h5, h7, h1, h3, Spec.union_assoc _ _ _ ha.1 hb.1 hc.1])
    (by rw [h6, h8, h2, h4, Spec.union_assoc _ _ _ ha.2 hb.2 hc.2])]

theorem pn_idem {a : Pn} (ha : a.wf) : pn_merge a a = ok a := by
  obtain ⟨c, hc, h1, h2⟩ := pn_merge_ok a a
  rw [hc, Pn.ext_abs (a := c) (b := a)
    (by rw [h1, Spec.union_idem _ ha.1]) (by rw [h2, Spec.union_idem _ ha.2])]

/-! ### `merge`: one theorem per rule -/

theorem merge_lastwriter_join : Join (Value.wf .LastWriter) (merge .LastWriter) where
  total a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    rename_i x y
    exact ⟨.Register (Spec.maxBy Register.key x y), by simp [merge, register_merge_spec],
      by simp [Value.wf]⟩
  comm a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    simp [merge, register_merge_spec, Spec.maxBy_comm Register.key_inj]
  assoc a b c ab bc ha hb hc hab hbc := by
    cases a <;> cases b <;> cases c <;> simp [Value.wf] at ha hb hc
    simp only [merge, register_merge_spec, bind_ok, Result.ok.injEq] at hab hbc
    cases hab; cases hbc
    simp [merge, register_merge_spec, Spec.maxBy_assoc]
  idem a ha := by
    cases a <;> simp [Value.wf] at ha
    simp [merge, register_merge_spec, Spec.maxBy_idem]

theorem merge_addwins_join : Join (Value.wf .AddWins) (merge .AddWins) where
  total a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    obtain ⟨c, hc, hw⟩ := tombstoned_total ha hb
    exact ⟨.Set c, by simp [merge, hc], hw⟩
  comm a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    simp only [merge, tombstoned_comm ha hb]
  assoc a b c ab bc ha hb hc hab hbc := by
    cases a <;> cases b <;> cases c <;> simp [Value.wf] at ha hb hc
    rename_i x y z
    obtain ⟨u, hu, -⟩ := tombstoned_total ha hb
    obtain ⟨v, hv, -⟩ := tombstoned_total hb hc
    simp only [merge, hu, hv, bind_ok, Result.ok.injEq, core.result.Result.Ok.injEq] at hab hbc
    cases hab; cases hbc
    simp only [merge, tombstoned_assoc ha hb hc hu hv]
  idem a ha := by
    cases a <;> simp [Value.wf] at ha
    simp [merge, tombstoned_idem ha]

theorem merge_keepboth_join : Join (Value.wf .KeepBoth) (merge .KeepBoth) where
  total a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    obtain ⟨c, hc, hw⟩ := tombstoned_total ha hb
    exact ⟨.Multi c, by simp [merge, hc], hw⟩
  comm a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    simp only [merge, tombstoned_comm ha hb]
  assoc a b c ab bc ha hb hc hab hbc := by
    cases a <;> cases b <;> cases c <;> simp [Value.wf] at ha hb hc
    obtain ⟨u, hu, -⟩ := tombstoned_total ha hb
    obtain ⟨v, hv, -⟩ := tombstoned_total hb hc
    simp only [merge, hu, hv, bind_ok, Result.ok.injEq, core.result.Result.Ok.injEq] at hab hbc
    cases hab; cases hbc
    simp only [merge, tombstoned_assoc ha hb hc hu hv]
  idem a ha := by
    cases a <;> simp [Value.wf] at ha
    simp [merge, tombstoned_idem ha]

theorem merge_counter_join : Join (Value.wf .Counter) (merge .Counter) where
  total a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    obtain ⟨c, hc, hw⟩ := pn_total ha hb
    exact ⟨.Counter c, by simp [merge, hc], hw⟩
  comm a b ha hb := by
    cases a <;> cases b <;> simp [Value.wf] at ha hb
    simp only [merge, pn_comm ha hb]
  assoc a b c ab bc ha hb hc hab hbc := by
    cases a <;> cases b <;> cases c <;> simp [Value.wf] at ha hb hc
    obtain ⟨u, hu, -⟩ := pn_total ha hb
    obtain ⟨v, hv, -⟩ := pn_total hb hc
    simp only [merge, hu, hv, bind_ok, Result.ok.injEq, core.result.Result.Ok.injEq] at hab hbc
    cases hab; cases hbc
    simp only [merge, pn_assoc ha hb hc hu hv]
  idem a ha := by
    cases a <;> simp [Value.wf] at ha
    simp [merge, pn_idem ha]

/-- Every rule's `merge` is a join-semilattice on its well-formed states. -/
theorem merge_join : ∀ r : Rule, Join (Value.wf r) (merge r)
  | .LastWriter => merge_lastwriter_join
  | .AddWins => merge_addwins_join
  | .Counter => merge_counter_join
  | .KeepBoth => merge_keepboth_join

/-! ### Struct components: the field-wise product -/

/-- Well-formed members: one state per rule, each well-formed for its rule. -/
def Fields.wf : Rules → Fields → Prop
  | .Nil, .Nil => True
  | .Cons r rs, .Cons v vs => Value.wf r v ∧ Fields.wf rs vs
  | _, _ => False

theorem merge_fields_cons {r : Rule} {rs : Rules} {x y v : Value} {xs ys w : Fields}
    (h1 : merge r x y = ok (.Ok v)) (h2 : merge_fields rs xs ys = ok (.Ok w)) :
    merge_fields (.Cons r rs) (.Cons x xs) (.Cons y ys) = ok (.Ok (.Cons v w)) := by
  rw [merge_fields]; simp [h1, h2]

/-- The field-wise product of semilattices is a semilattice: given that each member's merge is a
`Join` (here, `merge_join`), `merge_fields rules` is one for every list of member rules. -/
theorem merge_fields_join : ∀ rs : Rules, Join (Fields.wf rs) (merge_fields rs)
  | .Nil => {
      total := fun a b ha hb => by
        cases a <;> cases b <;> simp [Fields.wf] at ha hb
        exact ⟨.Nil, by rw [merge_fields], trivial⟩
      comm := fun a b ha hb => by cases a <;> cases b <;> simp [Fields.wf] at ha hb; rfl
      assoc := fun a b c ab bc ha hb hc hab hbc => by
        cases a <;> cases b <;> cases c <;> simp [Fields.wf] at ha hb hc
        rw [merge_fields] at hab hbc
        simp only [Result.ok.injEq, core.result.Result.Ok.injEq] at hab hbc
        cases hab; cases hbc; rfl
      idem := fun a ha => by cases a <;> simp [Fields.wf] at ha; rw [merge_fields] }
  | .Cons r rs => by
    have J := merge_join r
    have IH := merge_fields_join rs
    exact {
      total := fun a b ha hb => by
        cases a <;> cases b <;> simp [Fields.wf] at ha hb
        rename_i x xs y ys
        obtain ⟨v, hv, hvw⟩ := J.total x y ha.1 hb.1
        obtain ⟨w, hw, hww⟩ := IH.total xs ys ha.2 hb.2
        exact ⟨.Cons v w, merge_fields_cons hv hw, hvw, hww⟩
      comm := fun a b ha hb => by
        cases a <;> cases b <;> simp [Fields.wf] at ha hb
        rename_i x xs y ys
        rw [merge_fields, merge_fields, J.comm x y ha.1 hb.1, IH.comm xs ys ha.2 hb.2]
      assoc := fun a b c ab bc ha hb hc hab hbc => by
        cases a <;> cases b <;> cases c <;> simp [Fields.wf] at ha hb hc
        rename_i x xs y ys z zs
        obtain ⟨u, hu, huw⟩ := J.total x y ha.1 hb.1
        obtain ⟨u', hu', hu'w⟩ := IH.total xs ys ha.2 hb.2
        obtain ⟨v, hv, hvw⟩ := J.total y z hb.1 hc.1
        obtain ⟨v', hv', hv'w⟩ := IH.total ys zs hb.2 hc.2
        rw [merge_fields_cons hu hu', Result.ok.injEq, core.result.Result.Ok.injEq] at hab
        rw [merge_fields_cons hv hv', Result.ok.injEq, core.result.Result.Ok.injEq] at hbc
        cases hab; cases hbc
        rw [merge_fields, merge_fields, J.assoc x y z u v ha.1 hb.1 hc.1 hu hv,
          IH.assoc xs ys zs u' v' ha.2 hb.2 hc.2 hu' hv']
      idem := fun a ha => by
        cases a <;> simp [Fields.wf] at ha
        rename_i x xs
        exact merge_fields_cons (J.idem x ha.1) (IH.idem xs ha.2) }

/-! ### Validation is sound -/

theorem stamps_sorted_sound : ∀ a : Stamps, stamps_sorted a = ok true → Spec.Sorted a.abs
  | .Nil, _ => Spec.sorted_nil
  | .Cons x .Nil, _ => by simp [Stamps.abs, Spec.Sorted]
  | .Cons x (.Cons y rest), h => by
    rw [stamps_sorted] at h
    simp only [hlc_lt_spec, bind_ok] at h
    by_cases hxy : x.key < y.key
    · simp only [hxy, decide_true, if_true] at h
      have hs := stamps_sorted_sound (.Cons y rest) h
      refine Spec.sorted_cons.mpr ⟨fun q hq => ?_, hs⟩
      exact lt_of_lt_of_le hxy (Spec.bound_of_sorted hs le_rfl q hq)
    · simp [hxy] at h

theorem tagged_sorted_sound : ∀ a : Tagged, tagged_sorted a = ok true → Spec.Sorted a.abs
  | .Nil, _ => Spec.sorted_nil
  | .Cons x _ .Nil, _ => by simp [Tagged.abs, Spec.Sorted]
  | .Cons x u (.Cons y v rest), h => by
    rw [tagged_sorted] at h
    simp only [hlc_lt_spec, bind_ok] at h
    by_cases hxy : x.key < y.key
    · simp only [hxy, decide_true, if_true] at h
      have hs := tagged_sorted_sound (.Cons y v rest) h
      refine Spec.sorted_cons.mpr ⟨fun q hq => ?_, hs⟩
      exact lt_of_lt_of_le hxy (Spec.bound_of_sorted hs le_rfl q hq)
    · simp [hxy] at h

theorem counts_sorted_sound : ∀ a : Counts, counts_sorted a = ok true → Spec.Sorted a.abs
  | .Nil, _ => Spec.sorted_nil
  | .Cons x _ .Nil, _ => by simp [Counts.abs, Spec.Sorted]
  | .Cons x u (.Cons y v rest), h => by
    rw [counts_sorted] at h
    by_cases hxy : x < y
    · simp only [hxy, if_true] at h
      have hs := counts_sorted_sound (.Cons y v rest) h
      have hxy' : x.val < y.val := by simpa using hxy
      refine Spec.sorted_cons.mpr ⟨fun q hq => ?_, hs⟩
      exact lt_of_lt_of_le hxy' (Spec.bound_of_sorted hs le_rfl q hq)
    · simp [hxy] at h

theorem stamps_sorted_total : ∀ a : Stamps, ∃ b, stamps_sorted a = ok b
  | .Nil => ⟨true, by rw [stamps_sorted]⟩
  | .Cons x .Nil => ⟨true, by rw [stamps_sorted]⟩
  | .Cons x (.Cons y rest) => by
    rw [stamps_sorted]; simp only [hlc_lt_spec, bind_ok]
    by_cases hxy : x.key < y.key
    · simp only [hxy, decide_true, if_true]; exact stamps_sorted_total _
    · exact ⟨false, by simp [hxy]⟩

theorem tagged_sorted_total : ∀ a : Tagged, ∃ b, tagged_sorted a = ok b
  | .Nil => ⟨true, by rw [tagged_sorted]⟩
  | .Cons x _ .Nil => ⟨true, by rw [tagged_sorted]⟩
  | .Cons x u (.Cons y v rest) => by
    rw [tagged_sorted]; simp only [hlc_lt_spec, bind_ok]
    by_cases hxy : x.key < y.key
    · simp only [hxy, decide_true, if_true]; exact tagged_sorted_total _
    · exact ⟨false, by simp [hxy]⟩

theorem counts_sorted_total : ∀ a : Counts, ∃ b, counts_sorted a = ok b
  | .Nil => ⟨true, by rw [counts_sorted]⟩
  | .Cons x _ .Nil => ⟨true, by rw [counts_sorted]⟩
  | .Cons x u (.Cons y v rest) => by
    rw [counts_sorted]
    by_cases hxy : x < y
    · simp only [hxy, if_true]; exact counts_sorted_total _
    · exact ⟨false, by simp [hxy]⟩

theorem tombstoned_valid (t : Tombstoned)
    (h : (do let b ← tagged_sorted t.entries; if b then stamps_sorted t.removed else ok false) =
      ok true) : t.wf := by
  obtain ⟨b, hb⟩ := tagged_sorted_total t.entries
  rw [hb, bind_ok] at h
  cases b
  · simp at h
  · exact ⟨tagged_sorted_sound _ hb, stamps_sorted_sound _ (by simpa using h)⟩

theorem pn_valid (p : Pn)
    (h : (do let b ← counts_sorted p.inc; if b then counts_sorted p.dec else ok false) =
      ok true) : p.wf := by
  obtain ⟨b, hb⟩ := counts_sorted_total p.inc
  rw [hb, bind_ok] at h
  cases b
  · simp at h
  · exact ⟨counts_sorted_sound _ hb, counts_sorted_sound _ (by simpa using h)⟩

/-- A state that passes `validate` is well-formed, so the laws apply to it. -/
theorem validate_sound (r : Rule) (v : Value) (h : validate r v = ok true) : Value.wf r v := by
  cases r <;> cases v <;> simp only [validate, Result.ok.injEq, Bool.false_eq_true] at h <;>
    simp only [Value.wf]
  all_goals first
    | trivial
    | exact tombstoned_valid _ h
    | exact pn_valid _ h

end merge
