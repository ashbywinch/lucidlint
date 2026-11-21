//! The repo's own coverage snapshot: the files it names and the lines it saw
//! executed. A file the snapshot does not name is UNKNOWN, never uncovered.

use std::collections::HashMap;

use serde::Deserialize;

/// The repo's own coverage snapshot.
#[derive(Deserialize, Default)]
pub struct CoverageSnapshot {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub covered: HashMap<String, Vec<usize>>,
}
