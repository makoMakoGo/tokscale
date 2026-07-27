use crate::input_record_cache::{DecoderId, DecoderVariant, DecoderVersion};

use super::CodeBuddyLogOrigin;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopilotWorkspaceScope {
    BuiltInPlatform,
    ExplicitRoot,
}

/// The single authoritative runtime identity of an input decoder.
///
/// Each variant carries every detail that affects execution routing or
/// inventory identity. Persisted cache identity remains the deliberately
/// narrower [`DecoderVersion`] protocol derived by [`DecoderKind::version`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoderKind {
    Plain {
        decoder_id: DecoderId,
    },
    OpenCodeSqlite,
    AntigravityCliSqlite,
    KiroFile,
    KiroSqlite,
    KiroGlobalStorage,
    CodeBuddyJsonl,
    CodeBuddyExtensionLog {
        origin: CodeBuddyLogOrigin,
    },
    Copilot {
        workspace_scope: CopilotWorkspaceScope,
    },
    Codex,
}

impl DecoderKind {
    pub(crate) const fn plain(decoder_id: DecoderId) -> Self {
        assert!(
            decoder_id.supports_plain_kind(),
            "specialized decoder requires its dedicated DecoderKind constructor"
        );
        Self::Plain { decoder_id }
    }

    pub(crate) const fn opencode_sqlite() -> Self {
        Self::OpenCodeSqlite
    }

    pub(crate) const fn antigravity_cli_sqlite() -> Self {
        Self::AntigravityCliSqlite
    }

    pub(crate) const fn kiro_file() -> Self {
        Self::KiroFile
    }

    pub(crate) const fn kiro_sqlite() -> Self {
        Self::KiroSqlite
    }

    pub(crate) const fn kiro_global_storage() -> Self {
        Self::KiroGlobalStorage
    }

    pub(crate) const fn codebuddy_jsonl() -> Self {
        Self::CodeBuddyJsonl
    }

    pub(crate) const fn codebuddy_extension_log(origin: CodeBuddyLogOrigin) -> Self {
        Self::CodeBuddyExtensionLog { origin }
    }

    pub(crate) const fn copilot(workspace_scope: CopilotWorkspaceScope) -> Self {
        Self::Copilot { workspace_scope }
    }

    pub(crate) const fn codex() -> Self {
        Self::Codex
    }

    pub(crate) const fn version(self) -> DecoderVersion {
        match self {
            Self::Plain { decoder_id } => DecoderVersion::current(decoder_id),
            Self::OpenCodeSqlite => DecoderVersion::current(DecoderId::OpenCodeSqlite),
            Self::AntigravityCliSqlite => DecoderVersion::current(DecoderId::AntigravityCliSqlite),
            Self::KiroFile => DecoderVersion::current(DecoderId::KiroFile),
            Self::KiroSqlite => DecoderVersion::current(DecoderId::KiroSqlite),
            Self::KiroGlobalStorage => DecoderVersion::current(DecoderId::KiroGlobalStorage),
            Self::CodeBuddyJsonl => DecoderVersion::current(DecoderId::CodeBuddy)
                .with_variant(DecoderVariant::CodeBuddyJsonl),
            Self::CodeBuddyExtensionLog { origin } => DecoderVersion::current(DecoderId::CodeBuddy)
                .with_variant(match origin {
                    CodeBuddyLogOrigin::Extension => DecoderVariant::CodeBuddyExtension,
                    CodeBuddyLogOrigin::Host => DecoderVariant::CodeBuddyHost,
                }),
            Self::Copilot { workspace_scope } => DecoderVersion::current(DecoderId::Copilot)
                .with_variant(match workspace_scope {
                    CopilotWorkspaceScope::BuiltInPlatform => DecoderVariant::CopilotBuiltIn,
                    CopilotWorkspaceScope::ExplicitRoot => DecoderVariant::CopilotExplicitRoot,
                }),
            Self::Codex => DecoderVersion::current(DecoderId::Codex),
        }
    }

    pub(crate) const fn fingerprint_identity(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Plain { .. } => ("none", None),
            Self::OpenCodeSqlite => ("opencode-sqlite", None),
            Self::AntigravityCliSqlite => ("antigravity-cli-sqlite", None),
            Self::KiroFile => ("kiro-file", None),
            Self::KiroSqlite => ("kiro-sqlite", None),
            Self::KiroGlobalStorage => ("kiro-global-storage", None),
            Self::CodeBuddyJsonl => ("codebuddy-jsonl", None),
            Self::CodeBuddyExtensionLog { origin, .. } => (
                "codebuddy-extension-log",
                Some(match origin {
                    CodeBuddyLogOrigin::Extension => "extension",
                    CodeBuddyLogOrigin::Host => "host",
                }),
            ),
            Self::Copilot {
                workspace_scope, ..
            } => (
                "copilot",
                Some(match workspace_scope {
                    CopilotWorkspaceScope::BuiltInPlatform => "built-in-platform",
                    CopilotWorkspaceScope::ExplicitRoot => "explicit-root",
                }),
            ),
            Self::Codex => ("codex", None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "specialized decoder requires its dedicated DecoderKind constructor")]
    fn specialized_decoder_cannot_be_constructed_as_plain() {
        let _ = DecoderKind::plain(DecoderId::Codex);
    }

    #[test]
    fn specialized_kind_derives_cache_and_fingerprint_identity() {
        let kind = DecoderKind::kiro_sqlite();

        assert!(matches!(kind, DecoderKind::KiroSqlite));
        assert_eq!(
            kind.version(),
            DecoderVersion::current(DecoderId::KiroSqlite)
        );
        assert_eq!(kind.fingerprint_identity(), ("kiro-sqlite", None));
    }

    #[test]
    fn execution_detail_is_part_of_the_authoritative_kind() {
        let kind = DecoderKind::codebuddy_extension_log(CodeBuddyLogOrigin::Host);

        assert!(matches!(
            kind,
            DecoderKind::CodeBuddyExtensionLog {
                origin: CodeBuddyLogOrigin::Host
            }
        ));
        assert_eq!(
            kind.version(),
            DecoderVersion::current(DecoderId::CodeBuddy)
                .with_variant(DecoderVariant::CodeBuddyHost)
        );
        assert_eq!(
            kind.fingerprint_identity(),
            ("codebuddy-extension-log", Some("host"))
        );
    }
}
