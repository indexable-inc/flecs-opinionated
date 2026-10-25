//! Merge rules as runtime data (SPEC.md, "Merging"). A rule is a target of the `Merge` relationship in the world,
//! read back here as a plain enum, written into layout records by name and hashed with the layout. Merging itself is
//! not done here: callers hand values to a [`Merger`], which the merge crate implements with its non-generic
//! `merge(rule, a, b)`.

use ipld_core::ipld::Ipld;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    /// The write with the greatest (HLC, actor) wins.
    LastWriter,
    /// A set: an element is present if any add is not covered by a remove that saw it.
    AddWins,
    /// Concurrent increments add up.
    Counter,
    /// Concurrent writes are all kept, for the reader (or a later write) to resolve.
    KeepBoth,
    /// A text CRDT over the string.
    Text,
}

impl Rule {
    /// Never drop a concurrent write silently (SPEC.md, "Default rule").
    pub const DEFAULT: Rule = Rule::KeepBoth;

    pub const ALL: [Rule; 5] = [
        Rule::LastWriter,
        Rule::AddWins,
        Rule::Counter,
        Rule::KeepBoth,
        Rule::Text,
    ];

    /// The name used in scripts (`(Merge, LastWriter)`) and in layout records.
    pub fn name(self) -> &'static str {
        match self {
            Rule::LastWriter => "LastWriter",
            Rule::AddWins => "AddWins",
            Rule::Counter => "Counter",
            Rule::KeepBoth => "KeepBoth",
            Rule::Text => "Text",
        }
    }

    pub fn from_name(name: &str) -> Option<Rule> {
        Rule::ALL.into_iter().find(|r| r.name() == name)
    }

    /// Whether the rule can apply to a value of this layout (SPEC.md, "Layouts").
    pub fn check(self, layout: &Ipld) -> Result<(), String> {
        let prim = match layout {
            Ipld::String(s) => Some(s.as_str()),
            _ => None,
        };
        let ok = match self {
            Rule::LastWriter | Rule::KeepBoth => true,
            Rule::Text => prim == Some("string"),
            Rule::Counter => matches!(
                prim,
                Some(
                    "u8" | "u16"
                        | "u32"
                        | "u64"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "f32"
                        | "f64"
                        | "uptr"
                        | "iptr"
                        | "byte"
                )
            ),
            Rule::AddWins => matches!(layout, Ipld::Map(m) if m.contains_key("vector")),
        };
        if ok {
            Ok(())
        } else {
            let what = match self {
                Rule::Text => "a string",
                Rule::Counter => "a number",
                _ => "a vector",
            };
            Err(format!("(Merge, {}) needs {what}", self.name()))
        }
    }
}

/// How this crate calls merging: one non-generic function over runtime values, so a host, TS or a proof sees the
/// same thing. `a` and `b` are snapshot values (SPEC.md, "Values"); the result is a value of the same layout.
pub trait Merger {
    fn merge(&self, rule: Rule, a: &Ipld, b: &Ipld) -> Result<Ipld, String>;
}
