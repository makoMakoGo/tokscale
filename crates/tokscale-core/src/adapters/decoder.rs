use crate::message_cache::{DecoderId, DecoderRevision, DecoderVersion};

use super::CodeBuddyLogOrigin;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum DecoderRoute {
    #[default]
    None,
    OpenCodeSqlite,
    AntigravityCliSqlite,
    KiroFile,
    KiroSqlite,
    KiroGlobalStorage,
    CodeBuddyJsonl,
    CodeBuddyExtensionLog {
        origin: CodeBuddyLogOrigin,
    },
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecoderSpec {
    version: DecoderVersion,
    route: DecoderRoute,
}

impl DecoderSpec {
    const fn new(decoder_id: DecoderId, revision: DecoderRevision, route: DecoderRoute) -> Self {
        Self {
            version: DecoderVersion::new(decoder_id, revision),
            route,
        }
    }

    pub(crate) const fn plain(decoder_id: DecoderId, revision: DecoderRevision) -> Self {
        assert!(
            decoder_id.supports_plain_route(),
            "routed decoder requires its specialized DecoderSpec constructor"
        );
        Self::new(decoder_id, revision, DecoderRoute::None)
    }

    pub(crate) const fn opencode_sqlite(revision: DecoderRevision) -> Self {
        Self::new(
            DecoderId::OpenCodeSqlite,
            revision,
            DecoderRoute::OpenCodeSqlite,
        )
    }

    pub(crate) const fn antigravity_cli_sqlite(revision: DecoderRevision) -> Self {
        Self::new(
            DecoderId::AntigravityCliSqlite,
            revision,
            DecoderRoute::AntigravityCliSqlite,
        )
    }

    pub(crate) const fn kiro_file(revision: DecoderRevision) -> Self {
        Self::new(DecoderId::KiroFile, revision, DecoderRoute::KiroFile)
    }

    pub(crate) const fn kiro_sqlite(revision: DecoderRevision) -> Self {
        Self::new(DecoderId::KiroSqlite, revision, DecoderRoute::KiroSqlite)
    }

    pub(crate) const fn kiro_global_storage(revision: DecoderRevision) -> Self {
        Self::new(
            DecoderId::KiroGlobalStorage,
            revision,
            DecoderRoute::KiroGlobalStorage,
        )
    }

    pub(crate) const fn codebuddy_jsonl(revision: DecoderRevision) -> Self {
        Self::new(DecoderId::CodeBuddy, revision, DecoderRoute::CodeBuddyJsonl)
    }

    pub(crate) const fn codebuddy_extension_log(
        revision: DecoderRevision,
        origin: CodeBuddyLogOrigin,
    ) -> Self {
        Self::new(
            DecoderId::CodeBuddy,
            revision,
            DecoderRoute::CodeBuddyExtensionLog { origin },
        )
    }

    pub(crate) const fn codex(revision: DecoderRevision) -> Self {
        Self::new(DecoderId::Codex, revision, DecoderRoute::Codex)
    }

    pub(crate) const fn version(self) -> DecoderVersion {
        self.version
    }

    pub(crate) const fn route(self) -> DecoderRoute {
        self.route
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "routed decoder requires its specialized DecoderSpec constructor")]
    fn routed_decoder_cannot_be_constructed_as_plain() {
        let _ = DecoderSpec::plain(DecoderId::Codex, 1);
    }

    #[test]
    fn specialized_constructor_binds_route_and_cache_identity() {
        let spec = DecoderSpec::kiro_sqlite(7);

        assert_eq!(spec.route(), DecoderRoute::KiroSqlite);
        assert_eq!(
            spec.version(),
            DecoderVersion::new(DecoderId::KiroSqlite, 7)
        );
    }
}
