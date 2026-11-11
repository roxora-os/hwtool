use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The root filesystem hwtool works on: `/` on a board, a temporary
/// directory in tests.
#[derive(Debug, Clone)]
pub struct Root {
    base: PathBuf,
}

impl Root {
    pub fn new(base: impl Into<PathBuf>) -> Self {
        Root { base: base.into() }
    }

    pub fn system() -> Self {
        Root::new("/")
    }

    /// True when this is the live system rather than a fake root.
    pub fn is_system(&self) -> bool {
        self.base == Path::new("/")
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Map an absolute path on the target system to a path on this host.
    pub fn path(&self, abs: &str) -> PathBuf {
        self.base.join(abs.trim_start_matches('/'))
    }

    /// The inverse of [`Root::path`], for paths found by listing directories.
    pub fn unmap(&self, host: &Path) -> String {
        match host.strip_prefix(&self.base) {
            Ok(rel) => format!("/{}", rel.display()),
            Err(_) => host.display().to_string(),
        }
    }

    pub fn exists(&self, abs: &str) -> bool {
        self.path(abs).exists()
    }

    pub fn read(&self, abs: &str) -> Result<String> {
        fs::read_to_string(self.path(abs)).with_context(|| format!("cannot read {abs}"))
    }

    pub fn read_opt(&self, abs: &str) -> Option<String> {
        fs::read_to_string(self.path(abs)).ok()
    }

    /// Target of a symbolic link, as stored in the link.
    pub fn read_link(&self, abs: &str) -> Option<String> {
        fs::read_link(self.path(abs))
            .ok()
            .map(|p| p.display().to_string())
    }

    /// File names in a directory, sorted. Missing directory: empty list.
    pub fn list_dir(&self, abs: &str) -> Vec<String> {
        let mut names: Vec<String> = match fs::read_dir(self.path(abs)) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }

    /// True if an executable with this name is in one of the usual
    /// directories of the target system.
    pub fn has_program(&self, name: &str) -> bool {
        ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
            .iter()
            .any(|d| self.path(&format!("{d}/{name}")).exists())
    }
}
