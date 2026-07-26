use crate::input_record_cache::{DecoderId, DecoderRevision, DecoderVersion};

use super::CodeBuddyLogOrigin;

/// The single authoritative runtime identity of an input decoder.
///
/// Each variant carries every detail that affects execution routing or
/// inventory identity. Persisted cache identity remains the deliberately
/// narrower [`DecoderVersion`] protocol derived by [`DecoderKind::version`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecoderKind {
    Plain {
        decoder_id: DecoderId,
        revision: DecoderRevision,
    },
    OpenCodeSqlite {
        revision: DecoderRevision,
    },
    AntigravityCliSqlite {
        revision: DecoderRevision,
    },
    KiroFile {
        revision: DecoderRevision,
    },
    KiroSqlite {
        revision: DecoderRevision,
    },
    KiroGlobalStorage {
        revision: DecoderRevision,
    },
    CodeBuddyJsonl {
        revision: DecoderRevision,
    },
    CodeBuddyExtensionLog {
        revision: DecoderRevision,
        origin: CodeBuddyLogOrigin,
    },
    Codex {
        revision: DecoderRevision,
    },
}

impl DecoderKind {
    pub(crate) const fn plain(decoder_id: DecoderId, revision: DecoderRevision) -> Self {
        assert!(
            decoder_id.supports_plain_kind(),
            "specialized decoder requires its dedicated DecoderKind constructor"
        );
        Self::Plain {
            decoder_id,
            revision,
        }
    }

    pub(crate) const fn opencode_sqlite(revision: DecoderRevision) -> Self {
        Self::OpenCodeSqlite { revision }
    }

    pub(crate) const fn antigravity_cli_sqlite(revision: DecoderRevision) -> Self {
        Self::AntigravityCliSqlite { revision }
    }

    pub(crate) const fn kiro_file(revision: DecoderRevision) -> Self {
        Self::KiroFile { revision }
    }

    pub(crate) const fn kiro_sqlite(revision: DecoderRevision) -> Self {
        Self::KiroSqlite { revision }
    }

    pub(crate) const fn kiro_global_storage(revision: DecoderRevision) -> Self {
        Self::KiroGlobalStorage { revision }
    }

    pub(crate) const fn codebuddy_jsonl(revision: DecoderRevision) -> Self {
        Self::CodeBuddyJsonl { revision }
    }

    pub(crate) const fn codebuddy_extension_log(
        revision: DecoderRevision,
        origin: CodeBuddyLogOrigin,
    ) -> Self {
        Self::CodeBuddyExtensionLog { revision, origin }
    }

    pub(crate) const fn codex(revision: DecoderRevision) -> Self {
        Self::Codex { revision }
    }

    pub(crate) const fn version(self) -> DecoderVersion {
        match self {
            Self::Plain {
                decoder_id,
                revision,
            } => DecoderVersion::new(decoder_id, revision),
            Self::OpenCodeSqlite { revision } => {
                DecoderVersion::new(DecoderId::OpenCodeSqlite, revision)
            }
            Self::AntigravityCliSqlite { revision } => {
                DecoderVersion::new(DecoderId::AntigravityCliSqlite, revision)
            }
            Self::KiroFile { revision } => DecoderVersion::new(DecoderId::KiroFile, revision),
            Self::KiroSqlite { revision } => DecoderVersion::new(DecoderId::KiroSqlite, revision),
            Self::KiroGlobalStorage { revision } => {
                DecoderVersion::new(DecoderId::KiroGlobalStorage, revision)
            }
            Self::CodeBuddyJsonl { revision } | Self::CodeBuddyExtensionLog { revision, .. } => {
                DecoderVersion::new(DecoderId::CodeBuddy, revision)
            }
            Self::Codex { revision } => DecoderVersion::new(DecoderId::Codex, revision),
        }
    }

    pub(crate) const fn fingerprint_identity(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Plain { .. } => ("none", None),
            Self::OpenCodeSqlite { .. } => ("opencode-sqlite", None),
            Self::AntigravityCliSqlite { .. } => ("antigravity-cli-sqlite", None),
            Self::KiroFile { .. } => ("kiro-file", None),
            Self::KiroSqlite { .. } => ("kiro-sqlite", None),
            Self::KiroGlobalStorage { .. } => ("kiro-global-storage", None),
            Self::CodeBuddyJsonl { .. } => ("codebuddy-jsonl", None),
            Self::CodeBuddyExtensionLog { origin, .. } => (
                "codebuddy-extension-log",
                Some(match origin {
                    CodeBuddyLogOrigin::Extension => "extension",
                    CodeBuddyLogOrigin::Host => "host",
                }),
            ),
            Self::Codex { .. } => ("codex", None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "specialized decoder requires its dedicated DecoderKind constructor")]
    fn specialized_decoder_cannot_be_constructed_as_plain() {
        let _ = DecoderKind::plain(DecoderId::Codex, 1);
    }

    #[test]
    fn specialized_kind_derives_cache_and_fingerprint_identity() {
        let kind = DecoderKind::kiro_sqlite(7);

        assert!(matches!(kind, DecoderKind::KiroSqlite { revision: 7 }));
        assert_eq!(
            kind.version(),
            DecoderVersion::new(DecoderId::KiroSqlite, 7)
        );
        assert_eq!(kind.fingerprint_identity(), ("kiro-sqlite", None));
    }

    #[test]
    fn execution_detail_is_part_of_the_authoritative_kind() {
        let kind = DecoderKind::codebuddy_extension_log(3, CodeBuddyLogOrigin::Host);

        assert!(matches!(
            kind,
            DecoderKind::CodeBuddyExtensionLog {
                revision: 3,
                origin: CodeBuddyLogOrigin::Host
            }
        ));
        assert_eq!(kind.version(), DecoderVersion::new(DecoderId::CodeBuddy, 3));
        assert_eq!(
            kind.fingerprint_identity(),
            ("codebuddy-extension-log", Some("host"))
        );
    }
}
