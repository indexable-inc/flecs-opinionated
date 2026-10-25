//! Replays `vectors.json`, written by `lake exe vectors` from the proven Lean (see `lean/Vectors.lean`
//! for the JSON shapes). Each case is decoded, run through the Rust, encoded back and compared as JSON.

use merge::{
    Counts, Error, Fields, Hlc, Op, Pn, Register, Rule, Rules, Stamps, Tagged, Tombstoned, Value,
};
use serde_json::{Value as J, json};

fn n(j: &J) -> u64 {
    j.as_u64().expect("u64")
}

fn hlc(j: &J) -> Hlc {
    let a = j.as_array().expect("hlc");
    Hlc {
        wall: n(&a[0]),
        counter: u32::try_from(n(&a[1])).expect("u32"),
        actor: n(&a[2]),
    }
}

fn stamps(j: &J) -> Stamps {
    j.as_array()
        .expect("stamps")
        .iter()
        .rev()
        .fold(Stamps::Nil, |r, h| Stamps::Cons(hlc(h), Box::new(r)))
}

fn tagged(j: &J) -> Tagged {
    j.as_array()
        .expect("entries")
        .iter()
        .rev()
        .fold(Tagged::Nil, |r, e| {
            Tagged::Cons(hlc(&e[0]), n(&e[1]), Box::new(r))
        })
}

fn counts(j: &J) -> Counts {
    j.as_array()
        .expect("counts")
        .iter()
        .rev()
        .fold(Counts::Nil, |r, e| {
            Counts::Cons(n(&e[0]), n(&e[1]), Box::new(r))
        })
}

fn tomb(j: &J) -> Tombstoned {
    Tombstoned {
        entries: tagged(&j["entries"]),
        removed: stamps(&j["removed"]),
    }
}

fn tag(j: &J) -> (&str, &J) {
    let (k, v) = j
        .as_object()
        .expect("tagged")
        .iter()
        .next()
        .expect("one key");
    (k.as_str(), v)
}

fn value(j: &J) -> Value {
    match tag(j) {
        ("Register", v) => Value::Register(Register {
            stamp: hlc(&v["stamp"]),
            value: n(&v["value"]),
        }),
        ("Set", v) => Value::Set(tomb(v)),
        ("Counter", v) => Value::Counter(Pn {
            inc: counts(&v["inc"]),
            dec: counts(&v["dec"]),
        }),
        ("Multi", v) => Value::Multi(tomb(v)),
        (k, _) => panic!("value {k}"),
    }
}

fn op(j: &J) -> Op {
    match tag(j) {
        ("Write", v) => Op::Write(hlc(&v[0]), n(&v[1])),
        ("Add", v) => Op::Add(hlc(&v[0]), n(&v[1])),
        ("Remove", v) => Op::Remove(stamps(v)),
        ("Inc", v) => Op::Inc(n(&v[0]), n(&v[1])),
        ("Dec", v) => Op::Dec(n(&v[0]), n(&v[1])),
        ("Put", v) => Op::Put(hlc(&v[0]), n(&v[1]), stamps(&v[2])),
        (k, _) => panic!("op {k}"),
    }
}

fn rule(j: &J) -> Rule {
    match j.as_str().expect("rule") {
        "LastWriter" => Rule::LastWriter,
        "AddWins" => Rule::AddWins,
        "Counter" => Rule::Counter,
        "KeepBoth" => Rule::KeepBoth,
        k => panic!("rule {k}"),
    }
}

fn j_hlc(h: &Hlc) -> J {
    json!([h.wall, h.counter, h.actor])
}

fn j_stamps(s: &Stamps) -> J {
    let mut out = Vec::new();
    let mut s = s;
    while let Stamps::Cons(h, r) = s {
        out.push(j_hlc(h));
        s = r;
    }
    J::Array(out)
}

fn j_tomb(t: &Tombstoned) -> J {
    let mut entries = Vec::new();
    let mut e = &t.entries;
    while let Tagged::Cons(h, v, r) = e {
        entries.push(json!([j_hlc(h), v]));
        e = r;
    }
    json!({"entries": entries, "removed": j_stamps(&t.removed)})
}

fn j_counts(c: &Counts) -> J {
    let mut out = Vec::new();
    let mut c = c;
    while let Counts::Cons(a, v, r) = c {
        out.push(json!([a, v]));
        c = r;
    }
    J::Array(out)
}

fn j_value(v: &Value) -> J {
    match v {
        Value::Register(r) => json!({"Register": {"stamp": j_hlc(&r.stamp), "value": r.value}}),
        Value::Set(t) => json!({"Set": j_tomb(t)}),
        Value::Counter(p) => json!({"Counter": {"inc": j_counts(&p.inc), "dec": j_counts(&p.dec)}}),
        Value::Multi(t) => json!({"Multi": j_tomb(t)}),
    }
}

fn j_error(e: Error) -> J {
    match e {
        Error::Mismatch => json!("Mismatch"),
        Error::Overflow => json!("Overflow"),
    }
}

fn j_result<T>(r: &Result<T, Error>, f: impl Fn(&T) -> J) -> J {
    match r {
        Ok(v) => json!({"Ok": f(v)}),
        Err(e) => json!({"Err": j_error(*e)}),
    }
}

fn fields(j: &J) -> Fields {
    j.as_array()
        .expect("fields")
        .iter()
        .rev()
        .fold(Fields::Nil, |r, v| Fields::Cons(value(v), Box::new(r)))
}

fn rules(j: &J) -> Rules {
    j.as_array()
        .expect("rules")
        .iter()
        .rev()
        .fold(Rules::Nil, |r, v| Rules::Cons(rule(v), Box::new(r)))
}

fn j_fields(f: &Fields) -> J {
    let mut out = Vec::new();
    let mut f = f;
    while let Fields::Cons(v, r) = f {
        out.push(j_value(v));
        f = r;
    }
    J::Array(out)
}

fn vectors() -> J {
    serde_json::from_str(include_str!("vectors.json")).expect("vectors.json parses")
}

fn cases<'a>(all: &'a J, key: &str) -> &'a [J] {
    let cs = all[key].as_array().expect("case list");
    assert!(!cs.is_empty(), "no {key} vectors");
    cs
}

#[test]
fn merge_vectors() {
    let all = vectors();
    for c in cases(&all, "merge") {
        let got = merge::merge(rule(&c["rule"]), &value(&c["a"]), &value(&c["b"]));
        assert_eq!(j_result(&got, j_value), c["out"], "case {c}");
    }
}

#[test]
fn apply_vectors() {
    let all = vectors();
    for c in cases(&all, "apply") {
        let got = merge::apply(rule(&c["rule"]), &value(&c["state"]), &op(&c["op"]));
        assert_eq!(j_result(&got, j_value), c["out"], "case {c}");
    }
}

#[test]
fn field_vectors() {
    let all = vectors();
    for c in cases(&all, "fields") {
        let got = merge::merge_fields(&rules(&c["rules"]), &fields(&c["a"]), &fields(&c["b"]));
        assert_eq!(j_result(&got, j_fields), c["out"], "case {c}");
    }
}
