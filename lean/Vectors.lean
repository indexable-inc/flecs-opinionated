/-
Test vectors from the Aeneas-generated (and proven) Lean. `lake exe vectors <file>` writes JSON;
`merge/tests/vectors.rs` replays it against the Rust, and a TypeScript merge can replay it too.

States are built by applying random accepted ops to the empty state (so they are well formed), with
few actors and small clocks so equal keys and ties come up often. Inputs are drawn from a fixed seed,
so the file is deterministic.

JSON shapes (externally tagged, like serde):
  hlc        [wall, counter, actor]
  Register   {"Register": {"stamp": hlc, "value": n}}
  Set, Multi {"Set": {"entries": [[hlc, n], ...], "removed": [hlc, ...]}}
  Counter    {"Counter": {"inc": [[actor, n], ...], "dec": [[actor, n], ...]}}
  op         {"Write": [hlc, n]} {"Add": [hlc, n]} {"Remove": [hlc, ...]} {"Inc": [actor, n]}
             {"Dec": [actor, n]} {"Put": [hlc, n, [hlc, ...]]}
  result     {"Ok": value} or {"Err": "Mismatch" | "Overflow"}
Integers are u64 and can pass 2^53: parse them as big integers outside Rust.
-/
import Merge

open Aeneas Aeneas.Std Result merge

deriving instance Inhabited, BEq for merge.Rule

/-! ### Running the generated code -/

def run {α} (r : Result α) : Option α :=
  match r.match with
  | .ok a => some a
  | _ => none

def u64 (n : Nat) : U64 := ⟨BitVec.ofNat 64 n⟩
def u32 (n : Nat) : U32 := ⟨BitVec.ofNat 32 n⟩

/-! ### JSON -/

def arr (xs : List String) : String := "[" ++ ",".intercalate xs ++ "]"
def obj (kvs : List (String × String)) : String :=
  "{" ++ ",".intercalate (kvs.map fun (k, v) => "\"" ++ k ++ "\":" ++ v) ++ "}"
def str (s : String) : String := "\"" ++ s ++ "\""

def jHlc (h : Hlc) : String := arr [toString h.wall.val, toString h.counter.val, toString h.actor.val]

def merge.Stamps.toList : Stamps → List Hlc
  | .Nil => []
  | .Cons h r => h :: r.toList
def merge.Tagged.toList : Tagged → List (Hlc × U64)
  | .Nil => []
  | .Cons h v r => (h, v) :: r.toList
def merge.Counts.toList : Counts → List (U64 × U64)
  | .Nil => []
  | .Cons a n r => (a, n) :: r.toList

def jStamps (s : Stamps) : String := arr (s.toList.map jHlc)
def jTomb (t : Tombstoned) : String :=
  obj [("entries", arr (t.entries.toList.map fun (h, v) => arr [jHlc h, toString v.val])),
    ("removed", jStamps t.removed)]
def jCounts (c : Counts) : String :=
  arr (c.toList.map fun (a, n) => arr [toString a.val, toString n.val])

def jValue : Value → String
  | .Register r => obj [("Register", obj [("stamp", jHlc r.stamp), ("value", toString r.value.val)])]
  | .Set t => obj [("Set", jTomb t)]
  | .Counter p => obj [("Counter", obj [("inc", jCounts p.inc), ("dec", jCounts p.dec)])]
  | .Multi t => obj [("Multi", jTomb t)]

def jOp : Op → String
  | .Write h v => obj [("Write", arr [jHlc h, toString v.val])]
  | .Add h v => obj [("Add", arr [jHlc h, toString v.val])]
  | .Remove s => obj [("Remove", jStamps s)]
  | .Inc a n => obj [("Inc", arr [toString a.val, toString n.val])]
  | .Dec a n => obj [("Dec", arr [toString a.val, toString n.val])]
  | .Put h v s => obj [("Put", arr [jHlc h, toString v.val, jStamps s])]

def jRule : Rule → String
  | .LastWriter => str "LastWriter"
  | .AddWins => str "AddWins"
  | .Counter => str "Counter"
  | .KeepBoth => str "KeepBoth"

def jError : merge.Error → String
  | .Mismatch => str "Mismatch"
  | .Overflow => str "Overflow"

def jResult (j : α → String) : core.result.Result α merge.Error → String
  | .Ok v => obj [("Ok", j v)]
  | .Err e => obj [("Err", jError e)]

/-! ### Random inputs -/

/-- A 64-bit LCG (Knuth's MMIX constants). -/
abbrev Gen := StateM Nat

def next (bound : Nat) : Gen Nat := do
  let s := ((← get) * 6364136223846793005 + 1442695040888963407) % 2^64
  set s
  return (s / 2^33) % bound

def genHlc : Gen Hlc := do
  return { wall := u64 (← next 4), counter := u32 (← next 3), actor := u64 (← next 3) }

def genStamps : Gen Stamps := do
  let n ← next 3
  let mut s := Stamps.Nil
  for _ in [0:n] do
    let h ← genHlc
    s := (run (stamps_union s (.Cons h .Nil))).getD s
  return s

/-- A count, sometimes near `u64::MAX` so overflow comes up. -/
def genCount : Gen Nat := do
  if (← next 3) = 0 then return 2^64 - 1 - (← next 3) else return (← next 5)

/-- An op the rule accepts. -/
def genOp : Rule → Gen Op
  | .LastWriter => do return .Write (← genHlc) (u64 (← next 4))
  | .AddWins => do
    if (← next 3) = 0 then return .Remove (← genStamps) else return .Add (← genHlc) (u64 (← next 4))
  | .Counter => do
    let a := u64 (← next 3)
    let n := u64 (← genCount)
    if (← next 2) = 0 then return .Inc a n else return .Dec a n
  | .KeepBoth => do return .Put (← genHlc) (u64 (← next 4)) (← genStamps)

def empty : Rule → Value
  | .LastWriter => .Register ⟨⟨u64 0, u32 0, u64 0⟩, u64 0⟩
  | .AddWins => .Set ⟨.Nil, .Nil⟩
  | .Counter => .Counter ⟨.Nil, .Nil⟩
  | .KeepBoth => .Multi ⟨.Nil, .Nil⟩

/-- A well-formed state: up to four accepted ops on the empty state (failed ops are skipped). -/
def genState (r : Rule) : Gen Value := do
  let mut s := empty r
  for _ in [0:(← next 5)] do
    let o ← genOp r
    match run (apply r s o) with
    | some (.Ok s') => s := s'
    | _ => pure ()
  return s

def rules : List Rule := [.LastWriter, .AddWins, .Counter, .KeepBoth]

def genRule : Gen Rule := do return rules[← next 4]!

def genRules : Gen (List Rule) := do
  let mut rs := []
  for _ in [0:(← next 4)] do rs := rs ++ [← genRule]
  return rs

def toRules : List Rule → Rules
  | [] => .Nil
  | r :: rs => .Cons r (toRules rs)
def toFields : List Value → Fields
  | [] => .Nil
  | v :: vs => .Cons v (toFields vs)

def merge.Fields.toList : Fields → List Value
  | .Nil => []
  | .Cons v r => v :: r.toList

def jFields (f : Fields) : String := arr (f.toList.map jValue)

/-! ### Vectors -/

def mergeVec (r : Rule) (a b : Value) : Option String := do
  let out ← run (merge r a b)
  return obj [("rule", jRule r), ("a", jValue a), ("b", jValue b), ("out", jResult jValue out)]

def genMerge : Gen (List String) := do
  let mut out := []
  for r in rules do
    for _ in [0:40] do
      let a ← genState r
      let b ← genState r
      if let some v := mergeVec r a b then out := out ++ [v]
  -- shape mismatches
  for _ in [0:8] do
    let r ← genRule
    let other ← genRule
    if r != other then
      if let some v := mergeVec r (← genState r) (← genState other) then out := out ++ [v]
  return out

def genApply : Gen (List String) := do
  let mut out := []
  for r in rules do
    for _ in [0:40] do
      let s ← genState r
      let o ← genOp r
      if let some res := run (apply r s o) then
        out := out ++ [obj [("rule", jRule r), ("state", jValue s), ("op", jOp o),
          ("out", jResult jValue res)]]
  -- an op of another rule
  for _ in [0:8] do
    let r ← genRule
    let other ← genRule
    let s ← genState r
    let o ← genOp other
    if let some res := run (apply r s o) then
      out := out ++ [obj [("rule", jRule r), ("state", jValue s), ("op", jOp o),
        ("out", jResult jValue res)]]
  return out

def genFields : Gen (List String) := do
  let mut out := []
  for i in [0:30] do
    let rs ← genRules
    let mut a := []
    let mut b := []
    for r in rs do
      a := a ++ [← genState r]
      b := b ++ [← genState r]
    -- every fifth: a field count that does not match the rules
    if i % 5 = 4 then b := b ++ [empty .Counter]
    if let some res := run (merge_fields (toRules rs) (toFields a) (toFields b)) then
      out := out ++ [obj [("rules", arr (rs.map jRule)), ("a", jFields (toFields a)),
        ("b", jFields (toFields b)), ("out", jResult jFields res)]]
  return out

def lines (xs : List String) : String := "[\n" ++ ",\n".intercalate xs ++ "\n]"

def main (args : List String) : IO UInt32 := do
  let gen : Gen String := do
    let m ← genMerge
    let a ← genApply
    let f ← genFields
    return obj [("merge", lines m), ("apply", lines a), ("fields", lines f)] ++ "\n"
  let (json, _) := gen.run 20261024
  match args with
  | [path] => IO.FS.writeFile path json
  | _ => IO.print json
  return 0
