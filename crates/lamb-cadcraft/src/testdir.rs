//! A test's own folder in the system temp directory, removed when the test ends, even when an
//! assertion fails. Shaped like the engine's `tests/support/temp_dir.rs`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// `lamb-<tag>-<pid>-<n>`. The test runner gives each test its own process, so the counter
/// restarts and a reused process id can name a folder an earlier run left behind: `new` removes
/// whatever is there before creating the folder, and dropping it removes the folder.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(tag: &str) -> std::io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self::at(std::env::temp_dir().join(format!("lamb-{tag}-{}-{n}", std::process::id())))
    }

    fn at(dir: PathBuf) -> std::io::Result<Self> {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_folder_is_emptied_on_new_and_removed_on_drop() -> std::io::Result<()> {
        let path = std::env::temp_dir().join(format!("lamb-testdir-stale-{}", std::process::id()));
        std::fs::create_dir_all(path.join("views"))?;
        std::fs::write(path.join("manifest.json"), "stale")?;
        let dir = TempDir::at(path.clone())?;
        assert!(dir.is_dir());
        assert!(!dir.join("manifest.json").exists() && !dir.join("views").exists());
        drop(dir);
        assert!(!path.exists());
        let named = TempDir::new("testdir")?;
        let name = named.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        assert!(name.starts_with(&format!("lamb-testdir-{}-", std::process::id())), "{name}");
        Ok(())
    }
}
