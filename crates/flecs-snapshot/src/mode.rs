//! Sync modes as runtime data (SPEC.md, "Sync modes"): a target of the exclusive `Sync` relationship on a component or
//! tag type, read back as a plain enum, written into layout records by name and hashed with the layout.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// In the state hash, with change history. The default.
    Synced,
    /// In the state hash, without change history: only the latest value is kept.
    LatestOnly,
    /// Never hashed or stored: sent over a side channel (presence, cursors).
    Ephemeral,
}

impl Mode {
    pub const DEFAULT: Mode = Mode::Synced;

    pub const ALL: [Mode; 3] = [Mode::Synced, Mode::LatestOnly, Mode::Ephemeral];

    /// The name used in scripts (`(Sync, LatestOnly)`) and in layout records.
    pub fn name(self) -> &'static str {
        match self {
            Mode::Synced => "Synced",
            Mode::LatestOnly => "LatestOnly",
            Mode::Ephemeral => "Ephemeral",
        }
    }

    pub fn from_name(name: &str) -> Option<Mode> {
        Mode::ALL.into_iter().find(|m| m.name() == name)
    }
}
