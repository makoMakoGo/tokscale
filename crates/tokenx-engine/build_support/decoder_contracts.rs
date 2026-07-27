use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"tokenx-decoder-contract-v1";

pub(crate) struct DecoderContractSpec {
    pub variant: &'static str,
    pub stable_name: &'static str,
    pub plain_kind: bool,
    pub integration: &'static str,
}

pub(crate) const DECODER_CONTRACTS: &[DecoderContractSpec] = &[
    DecoderContractSpec {
        variant: "OpenCodeSqlite",
        stable_name: "opencode-sqlite",
        plain_kind: false,
        integration: "opencode",
    },
    DecoderContractSpec {
        variant: "Claude",
        stable_name: "claude",
        plain_kind: true,
        integration: "claude",
    },
    DecoderContractSpec {
        variant: "Codex",
        stable_name: "codex",
        plain_kind: false,
        integration: "codex",
    },
    DecoderContractSpec {
        variant: "Gemini",
        stable_name: "gemini",
        plain_kind: true,
        integration: "gemini",
    },
    DecoderContractSpec {
        variant: "Amp",
        stable_name: "amp",
        plain_kind: true,
        integration: "amp",
    },
    DecoderContractSpec {
        variant: "Droid",
        stable_name: "droid",
        plain_kind: true,
        integration: "droid",
    },
    DecoderContractSpec {
        variant: "OpenClaw",
        stable_name: "openclaw",
        plain_kind: true,
        integration: "openclaw",
    },
    DecoderContractSpec {
        variant: "Pi",
        stable_name: "pi",
        plain_kind: true,
        integration: "pi",
    },
    DecoderContractSpec {
        variant: "Omp",
        stable_name: "omp",
        plain_kind: true,
        integration: "omp",
    },
    DecoderContractSpec {
        variant: "Kimi",
        stable_name: "kimi",
        plain_kind: true,
        integration: "kimi",
    },
    DecoderContractSpec {
        variant: "Qwen",
        stable_name: "qwen",
        plain_kind: true,
        integration: "qwen",
    },
    DecoderContractSpec {
        variant: "RooCode",
        stable_name: "roo-code",
        plain_kind: true,
        integration: "roocode",
    },
    DecoderContractSpec {
        variant: "Mux",
        stable_name: "mux",
        plain_kind: true,
        integration: "mux",
    },
    DecoderContractSpec {
        variant: "Kilo",
        stable_name: "kilo",
        plain_kind: true,
        integration: "kilo",
    },
    DecoderContractSpec {
        variant: "Hermes",
        stable_name: "hermes",
        plain_kind: true,
        integration: "hermes",
    },
    DecoderContractSpec {
        variant: "Copilot",
        stable_name: "copilot",
        plain_kind: false,
        integration: "copilot",
    },
    DecoderContractSpec {
        variant: "Goose",
        stable_name: "goose",
        plain_kind: true,
        integration: "goose",
    },
    DecoderContractSpec {
        variant: "Codebuff",
        stable_name: "codebuff",
        plain_kind: true,
        integration: "codebuff",
    },
    DecoderContractSpec {
        variant: "AntigravityCliSqlite",
        stable_name: "antigravity-cli-sqlite",
        plain_kind: false,
        integration: "antigravity",
    },
    DecoderContractSpec {
        variant: "Zed",
        stable_name: "zed",
        plain_kind: true,
        integration: "zed",
    },
    DecoderContractSpec {
        variant: "Kiro",
        stable_name: "kiro",
        plain_kind: true,
        integration: "kiro",
    },
    DecoderContractSpec {
        variant: "KiroFile",
        stable_name: "kiro-file",
        plain_kind: false,
        integration: "kiro",
    },
    DecoderContractSpec {
        variant: "KiroSqlite",
        stable_name: "kiro-sqlite",
        plain_kind: false,
        integration: "kiro",
    },
    DecoderContractSpec {
        variant: "KiroGlobalStorage",
        stable_name: "kiro-global-storage",
        plain_kind: false,
        integration: "kiro",
    },
    DecoderContractSpec {
        variant: "Junie",
        stable_name: "junie",
        plain_kind: true,
        integration: "junie",
    },
    DecoderContractSpec {
        variant: "Cline",
        stable_name: "cline",
        plain_kind: true,
        integration: "cline",
    },
    DecoderContractSpec {
        variant: "CommandCode",
        stable_name: "command-code",
        plain_kind: true,
        integration: "commandcode",
    },
    DecoderContractSpec {
        variant: "Grok",
        stable_name: "grok",
        plain_kind: true,
        integration: "grok",
    },
    DecoderContractSpec {
        variant: "Zcode",
        stable_name: "zcode",
        plain_kind: true,
        integration: "zcode",
    },
    DecoderContractSpec {
        variant: "Warp",
        stable_name: "warp",
        plain_kind: true,
        integration: "warp",
    },
    DecoderContractSpec {
        variant: "CodeBuddy",
        stable_name: "codebuddy",
        plain_kind: false,
        integration: "codebuddy",
    },
    DecoderContractSpec {
        variant: "OmpParentHealth",
        stable_name: "omp-parent-health",
        plain_kind: true,
        integration: "omp",
    },
];

pub(crate) fn production_sources(crate_dir: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        crate_dir.join("src/lib.rs"),
        crate_dir.join("src/model_aliases.rs"),
        crate_dir.join("src/provider_identity.rs"),
        crate_dir.join("src/token_imputation.rs"),
    ];
    paths.extend(rust_sources_recursive(&crate_dir.join("src/records")));
    paths.extend(
        fs::read_dir(crate_dir.join("src/integrations"))
            .expect("failed to enumerate shared integration sources")
            .map(|entry| {
                entry
                    .expect("failed to inspect shared integration source")
                    .path()
            })
            .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "rs")),
    );
    paths.sort();
    paths.dedup();
    paths
}

pub(crate) fn integration_sources(crate_dir: &Path, integration: &str) -> Vec<PathBuf> {
    let root = crate_dir.join("src/integrations").join(integration);
    let mut paths = rust_sources_recursive(&root);
    paths.sort();
    paths
}

fn rust_sources_recursive(root: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(root).unwrap_or_else(|error| {
        panic!(
            "failed to enumerate decoder contract source {}: {error}",
            root.display()
        )
    });
    let mut paths = Vec::new();
    for entry in entries {
        let path = entry
            .unwrap_or_else(|error| {
                panic!(
                    "failed to inspect decoder contract source {}: {error}",
                    root.display()
                )
            })
            .path();
        if path.is_dir() {
            paths.extend(rust_sources_recursive(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            paths.push(path);
        }
    }
    paths
}

pub(crate) fn contract_fingerprint(
    crate_dir: &Path,
    shared_sources: &[PathBuf],
    integration_sources: &[PathBuf],
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(DOMAIN);
    hash_sources(&mut hasher, crate_dir, shared_sources);
    hash_sources(&mut hasher, crate_dir, integration_sources);
    hasher.finalize().into()
}

fn hash_sources(hasher: &mut Sha256, crate_dir: &Path, sources: &[PathBuf]) {
    for path in sources {
        let relative = path.strip_prefix(crate_dir).unwrap_or(path);
        let relative = relative.to_string_lossy();
        let bytes = fs::read(path).unwrap_or_else(|error| {
            panic!(
                "failed to read decoder contract source {}: {error}",
                path.display()
            )
        });
        hash_part(hasher, relative.as_bytes());
        hash_part(hasher, &bytes);
    }
}

fn hash_part(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

pub(crate) fn generate_decoder_contracts(crate_dir: &Path) -> (String, Vec<PathBuf>) {
    let shared = production_sources(crate_dir);
    let mut watched = shared.clone();
    let mut stable_name_arms = String::new();
    let mut from_stable_name_arms = String::new();
    let mut plain_kind_arms = String::new();
    let mut contract_arms = String::new();

    for spec in DECODER_CONTRACTS {
        let integration = integration_sources(crate_dir, spec.integration);
        watched.extend(integration.iter().cloned());
        let fingerprint = contract_fingerprint(crate_dir, &shared, &integration);
        let bytes = fingerprint
            .iter()
            .map(|byte| format!("0x{byte:02x}"))
            .collect::<Vec<_>>()
            .join(", ");
        stable_name_arms.push_str(&format!(
            "            Self::{} => {:?},\n",
            spec.variant, spec.stable_name
        ));
        from_stable_name_arms.push_str(&format!(
            "            {:?} => Some(Self::{}),\n",
            spec.stable_name, spec.variant
        ));
        plain_kind_arms.push_str(&format!(
            "            Self::{} => {},\n",
            spec.variant, spec.plain_kind
        ));
        contract_arms.push_str(&format!(
            "            Self::{} => DecoderContractFingerprint([{bytes}]),\n",
            spec.variant
        ));
    }

    watched.sort();
    watched.dedup();
    (
        format!(
            "#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]\npub(crate) enum DecoderId {{\n{}\n}}\n\nimpl DecoderId {{\n    pub(crate) const fn stable_name(self) -> &'static str {{\n        match self {{\n{stable_name_arms}        }}\n    }}\n\n    pub(crate) fn from_stable_name(stable_name: &str) -> Option<Self> {{\n        match stable_name {{\n{from_stable_name_arms}            _ => None,\n        }}\n    }}\n\n    pub(crate) const fn supports_plain_kind(self) -> bool {{\n        match self {{\n{plain_kind_arms}        }}\n    }}\n\n    pub(crate) const fn contract_fingerprint(self) -> DecoderContractFingerprint {{\n        match self {{\n{contract_arms}        }}\n    }}\n}}\n\nimpl serde::Serialize for DecoderId {{\n    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: serde::Serializer,\n    {{\n        serializer.serialize_str(self.stable_name())\n    }}\n}}\n\nimpl<'de> serde::Deserialize<'de> for DecoderId {{\n    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>\n    where\n        D: serde::Deserializer<'de>,\n    {{\n        let stable_name = <String as serde::Deserialize>::deserialize(deserializer)?;\n        Self::from_stable_name(&stable_name).ok_or_else(|| {{\n            serde::de::Error::custom(format!(\"unknown decoder `{{stable_name}}`\"))\n        }})\n    }}\n}}\n",
            DECODER_CONTRACTS
                .iter()
                .map(|spec| format!("    {},", spec.variant))
                .collect::<Vec<_>>()
                .join("\n")
        ),
        watched,
    )
}
