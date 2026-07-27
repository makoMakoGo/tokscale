#[path = "../build_support/decoder_contracts.rs"]
mod decoder_contracts;

use std::fs;
use std::path::Path;

use decoder_contracts::{
    contract_fingerprint, generate_decoder_contracts, integration_sources, DECODER_CONTRACTS,
};

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn fingerprint(root: &Path, integration: &str) -> [u8; 32] {
    let shared = vec![root.join("shared.rs")];
    let integration = vec![root.join(integration).join("decode.rs")];
    contract_fingerprint(root, &shared, &integration)
}

#[test]
fn contract_fingerprint_is_deterministic_and_content_addressed() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("shared.rs"), "shared-v1");
    write(&root.join("amp/decode.rs"), "amp-v1");

    let first = fingerprint(root, "amp");
    assert_eq!(first, fingerprint(root, "amp"));

    write(&root.join("amp/decode.rs"), "amp-v2");
    assert_ne!(first, fingerprint(root, "amp"));
}

#[test]
fn shared_sources_invalidate_all_contracts_and_integrations_stay_isolated() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("shared.rs"), "shared-v1");
    write(&root.join("amp/decode.rs"), "amp-v1");
    write(&root.join("claude/decode.rs"), "claude-v1");

    let amp = fingerprint(root, "amp");
    let claude = fingerprint(root, "claude");

    write(&root.join("amp/decode.rs"), "amp-v2");
    let amp_after_integration_change = fingerprint(root, "amp");
    assert_ne!(amp, amp_after_integration_change);
    assert_eq!(claude, fingerprint(root, "claude"));

    write(&root.join("shared.rs"), "shared-v2");
    assert_ne!(amp_after_integration_change, fingerprint(root, "amp"));
    assert_ne!(claude, fingerprint(root, "claude"));
}

#[test]
fn source_path_is_part_of_the_contract() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    write(&root.join("shared.rs"), "shared");
    write(&root.join("amp/decode.rs"), "same");
    write(&root.join("claude/decode.rs"), "same");

    let shared = vec![root.join("shared.rs")];
    let amp = vec![root.join("amp/decode.rs")];
    let claude = vec![root.join("claude/decode.rs")];
    assert_ne!(
        contract_fingerprint(root, &shared, &amp),
        contract_fingerprint(root, &shared, &claude)
    );
}

#[test]
fn integration_source_discovery_is_recursive_and_rust_only() {
    let temp = tempfile::TempDir::new().unwrap();
    let root = temp.path();
    write(
        &root.join("src/integrations/amp/decode.rs"),
        "fn decode() {}",
    );
    write(
        &root.join("src/integrations/amp/nested/filter.rs"),
        "fn filter() {}",
    );
    write(&root.join("src/integrations/amp/nested/fixture.json"), "{}");

    let sources = integration_sources(root, "amp");
    assert_eq!(sources.len(), 2);
    assert!(sources[0].ends_with("src/integrations/amp/decode.rs"));
    assert!(sources[1].ends_with("src/integrations/amp/nested/filter.rs"));
}

#[test]
fn declarative_specs_generate_every_decoder_identity_and_watch_every_source() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (generated, watched) = generate_decoder_contracts(crate_dir);

    assert_eq!(
        generated
            .matches(" => DecoderContractFingerprint([")
            .count(),
        DECODER_CONTRACTS.len()
    );
    for spec in DECODER_CONTRACTS {
        assert!(generated.contains(&format!("    {},", spec.variant)));
        assert!(generated.contains(&format!(
            "{:?} => Some(Self::{})",
            spec.stable_name, spec.variant
        )));
        assert!(watched.iter().any(|path| {
            path.components()
                .any(|component| component.as_os_str() == spec.integration)
        }));
    }
    assert!(watched
        .iter()
        .any(|path| path.ends_with("src/integrations/decoder.rs")));
    assert!(watched
        .iter()
        .any(|path| path.ends_with("src/records/utils.rs")));
}
