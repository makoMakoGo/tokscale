use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;

#[path = "build_support/decoder_contracts.rs"]
mod decoder_contracts;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogEntry {
    variant: String,
    id: String,
    display_name: String,
}

fn main() {
    println!("cargo:rerun-if-changed=client-catalog.json");
    println!("cargo:rerun-if-changed=build_support/decoder_contracts.rs");

    let raw = fs::read_to_string("client-catalog.json")
        .expect("failed to read crates/tokenx-engine/client-catalog.json");
    let entries: Vec<CatalogEntry> =
        serde_json::from_str(&raw).expect("failed to parse client-catalog.json");
    validate_catalog(&entries);

    let generated = generate_rust(&entries);
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR must be set by cargo");
    fs::write(Path::new(&out_dir).join("client_catalog.rs"), generated)
        .expect("failed to write generated client catalog");

    let crate_dir = Path::new(".");
    let (generated, watched) = decoder_contracts::generate_decoder_contracts(crate_dir);
    for source in watched {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    fs::write(Path::new(&out_dir).join("decoder_contracts.rs"), generated)
        .expect("failed to write generated decoder contracts");
}

fn validate_catalog(entries: &[CatalogEntry]) {
    assert!(!entries.is_empty(), "client catalog must not be empty");

    let mut variants = HashSet::new();
    let mut ids = HashSet::new();

    for entry in entries {
        assert!(
            variants.insert(entry.variant.as_str()),
            "duplicate client variant {}",
            entry.variant
        );
        assert!(
            ids.insert(entry.id.as_str()),
            "duplicate client id {}",
            entry.id
        );
        assert!(
            !entry.id.trim().is_empty() && entry.id == entry.id.to_ascii_lowercase(),
            "client id must be non-empty lowercase: {}",
            entry.id
        );
        assert!(
            !entry.display_name.trim().is_empty(),
            "displayName must be set for {}",
            entry.id
        );
    }
}

fn generate_rust(entries: &[CatalogEntry]) -> String {
    let count = entries.len();
    let variants = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| format!("    {} = {index},\n", entry.variant))
        .collect::<String>();
    let all = entries
        .iter()
        .map(|entry| format!("ClientId::{}", entry.variant))
        .collect::<Vec<_>>()
        .join(", ");
    let from_str = entries
        .iter()
        .map(|entry| {
            format!(
                "            {} => Some(ClientId::{}),\n",
                rust_string(&entry.id),
                entry.variant
            )
        })
        .collect::<String>();
    let identities = entries
        .iter()
        .map(|entry| {
            format!(
                "    ClientIdentity {{ id: {}, display_name: {} }},\n",
                rust_string(&entry.id),
                rust_string(&entry.display_name),
            )
        })
        .collect::<String>();

    format!(
        r#"#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum ClientId {{
{variants}}}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIdentity {{
    pub id: &'static str,
    pub display_name: &'static str,
}}

impl ClientId {{
    pub const COUNT: usize = {count};
    pub const ALL: [ClientId; Self::COUNT] = [{all}];

    pub fn iter() -> impl Iterator<Item = ClientId> {{
        Self::ALL.iter().copied()
    }}

    pub fn identity(self) -> &'static ClientIdentity {{
        &CLIENT_IDENTITIES[self as usize]
    }}

    pub fn as_str(self) -> &'static str {{
        self.identity().id
    }}

    pub fn display_name(self) -> &'static str {{
        self.identity().display_name
    }}

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<ClientId> {{
        match s {{
{from_str}            _ => None,
        }}
    }}
}}

impl std::fmt::Display for ClientId {{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{
        formatter.write_str(self.as_str())
    }}
}}

impl PartialOrd for ClientId {{
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {{
        Some(self.cmp(other))
    }}
}}

impl Ord for ClientId {{
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {{
        self.as_str().cmp(other.as_str())
    }}
}}

impl AsRef<str> for ClientId {{
    fn as_ref(&self) -> &str {{
        self.as_str()
    }}
}}

impl serde::Serialize for ClientId {{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {{
        serializer.serialize_str(self.as_str())
    }}
}}

impl<'de> serde::Deserialize<'de> for ClientId {{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {{
        let id = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_str(&id)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown local client `{{id}}`")))
    }}
}}

pub const CLIENT_IDENTITIES: [ClientIdentity; ClientId::COUNT] = [
{identities}];
"#
    )
}

fn rust_string(value: &str) -> String {
    format!("{value:?}")
}
