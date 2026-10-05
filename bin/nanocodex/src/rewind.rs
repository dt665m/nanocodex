//! Explicit user-only native file restoration; never registered as an agent tool.
use eyre::{Result, eyre};
pub(crate) fn run(session: &str, checkpoint: Option<&str>, restore: bool) -> Result<()> {
    let home = crate::config::default_codex_home()?;
    let result = crate::config::rewind_files(&home, session, checkpoint, restore)
        .map_err(|error| eyre!(error))?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
