use std::path::{Path, PathBuf};

type FileMatcher = fn(&Path) -> bool;
type DirectoryMatcher = fn(&Path, usize) -> bool;

fn match_all_directories(_path: &Path, _depth: usize) -> bool {
    true
}

/// Driver-owned policy passed into the generic filesystem scanner.
///
/// Function pointers keep source declarations const-friendly without adding
/// an driver trait hierarchy around two predicates.
#[derive(Clone, Copy)]
pub(crate) struct SourceMatcher {
    file: FileMatcher,
    directory: DirectoryMatcher,
}

impl SourceMatcher {
    pub(crate) const fn new(file: FileMatcher) -> Self {
        Self {
            file,
            directory: match_all_directories,
        }
    }

    pub(crate) const fn with_directory_filter(
        file: FileMatcher,
        directory: DirectoryMatcher,
    ) -> Self {
        Self { file, directory }
    }

    pub(crate) fn matches_file(self, path: &Path) -> bool {
        (self.file)(path)
    }

    pub(crate) fn should_descend(self, path: &Path, depth: usize) -> bool {
        (self.directory)(path, depth)
    }
}

pub(crate) mod matchers {
    use std::path::Path;

    fn file_name(path: &Path) -> std::borrow::Cow<'_, str> {
        path.file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default()
    }

    pub(crate) fn json(path: &Path) -> bool {
        file_name(path).ends_with(".json")
    }

    pub(crate) fn jsonl(path: &Path) -> bool {
        file_name(path).ends_with(".jsonl")
    }

    pub(crate) fn database(path: &Path) -> bool {
        file_name(path).ends_with(".db")
    }

    pub(crate) fn log(path: &Path) -> bool {
        file_name(path).ends_with(".log")
    }

    pub(crate) fn settings_json(path: &Path) -> bool {
        file_name(path).ends_with(".settings.json")
    }

    pub(crate) fn messages_json(path: &Path) -> bool {
        file_name(path).ends_with(".messages.json")
    }

    pub(crate) fn archived_jsonl(path: &Path) -> bool {
        let name = file_name(path);
        name.ends_with(".jsonl")
            || name.contains(".jsonl.deleted.")
            || name.contains(".jsonl.reset.")
    }

    pub(crate) fn amp_thread(path: &Path) -> bool {
        let name = file_name(path);
        name.starts_with("T-") && name.ends_with(".json")
    }

    pub(crate) fn kiro_global_storage(path: &Path) -> bool {
        let name = file_name(path);
        name.ends_with(".chat") || name.ends_with(".json") || path.extension().is_none()
    }

    macro_rules! exact_matchers {
        ($($name:ident => $file_name:literal),+ $(,)?) => {
            $(
                pub(crate) fn $name(path: &Path) -> bool {
                    file_name(path) == $file_name
                }
            )+
        };
    }

    exact_matchers! {
        chat_messages_json => "chat-messages.json",
        events_jsonl => "events.jsonl",
        kilo_db => "kilo.db",
        session_usage_json => "session-usage.json",
        sessions_db => "sessions.db",
        state_db => "state.db",
        threads_db => "threads.db",
        ui_messages_json => "ui_messages.json",
        updates_jsonl => "updates.jsonl",
        warp_sqlite => "warp.sqlite",
        wire_jsonl => "wire.jsonl",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceRoot {
    Home,
    LocalShare,
}

#[derive(Clone, Copy)]
pub(crate) struct SourceSpec {
    root: SourceRoot,
    relative_path: &'static str,
    matcher: SourceMatcher,
}

impl SourceSpec {
    pub(crate) const fn home(relative_path: &'static str, matcher: SourceMatcher) -> Self {
        Self {
            root: SourceRoot::Home,
            relative_path,
            matcher,
        }
    }

    pub(crate) const fn local_share(relative_path: &'static str, matcher: SourceMatcher) -> Self {
        Self {
            root: SourceRoot::LocalShare,
            relative_path,
            matcher,
        }
    }

    pub(crate) fn resolve(self, home_dir: &Path) -> PathBuf {
        match self.root {
            SourceRoot::Home => home_dir.join(self.relative_path),
            SourceRoot::LocalShare => home_dir.join(".local/share").join(self.relative_path),
        }
    }

    pub(crate) const fn matcher(self) -> SourceMatcher {
        self.matcher
    }
}
