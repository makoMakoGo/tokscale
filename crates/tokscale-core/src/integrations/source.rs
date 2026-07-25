use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceRoot {
    Home,
    LocalShare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceSpec {
    root: SourceRoot,
    relative_path: &'static str,
    pattern: &'static str,
}

impl SourceSpec {
    pub(crate) const fn home(relative_path: &'static str, pattern: &'static str) -> Self {
        Self {
            root: SourceRoot::Home,
            relative_path,
            pattern,
        }
    }

    pub(crate) const fn local_share(relative_path: &'static str, pattern: &'static str) -> Self {
        Self {
            root: SourceRoot::LocalShare,
            relative_path,
            pattern,
        }
    }

    pub(crate) fn resolve(self, home_dir: &Path) -> PathBuf {
        match self.root {
            SourceRoot::Home => home_dir.join(self.relative_path),
            SourceRoot::LocalShare => home_dir.join(".local/share").join(self.relative_path),
        }
    }

    pub(crate) const fn pattern(self) -> &'static str {
        self.pattern
    }
}
