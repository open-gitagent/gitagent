//! Run records: the ATIF trajectory, feature counters and timestamps.
//!
//! Both files are rewritten atomically after every turn so that a run killed
//! by a timeout still leaves a complete record of everything up to that turn.

pub mod atif;
mod counters;
mod events;
mod time;

pub use counters::Counters;
pub use events::{Events, SharedBuffer};
pub use time::utc_now_rfc3339;

use std::path::Path;

use anyhow::{Context, Result};

/// Writes `bytes` to `path` via a temporary sibling file and a rename, so readers
/// never see a half-written file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", path.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    Ok(())
}
