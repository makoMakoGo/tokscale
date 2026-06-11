use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogEntry {
    variant: String,
    id: String,
    display_name: String,
    short_name: String,
    hotkey: String,
    submit_default: bool,
    logo: String,
    color: String,
    text_color: Option<String>,
}

fn main() {
    println!("cargo:rerun-if-changed=client-catalog.json");

    let raw = fs::read_to_string("client-catalog.json")
        .expect("failed to read crates/tokscale-core/client-catalog.json");
    let entries: Vec<CatalogEntry> =
        serde_json::from_str(&raw).expect("failed to parse client-catalog.json");
    validate_catalog(&entries);

    let generated = generate_rust(&entries);
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR must be set by cargo");
    fs::write(Path::new(&out_dir).join("client_catalog.rs"), generated)
        .expect("failed to write generated client catalog");
}

fn validate_catalog(entries: &[CatalogEntry]) {
    assert!(!entries.is_empty(), "client catalog must not be empty");

    let mut variants = HashSet::new();
    let mut ids = HashSet::new();
    let mut hotkeys = HashSet::new();

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
        assert!(
            !entry.short_name.trim().is_empty(),
            "shortName must be set for {}",
            entry.id
        );
        assert!(
            !entry.logo.trim().is_empty(),
            "logo must be set for {}",
            entry.id
        );
        assert!(
            is_hex_color(&entry.color),
            "color must be #RRGGBB for {}",
            entry.id
        );
        if let Some(text_color) = &entry.text_color {
            assert!(
                is_hex_color(text_color),
                "textColor must be #RRGGBB for {}",
                entry.id
            );
        }
        let mut chars = entry.hotkey.chars();
        let Some(ch) = chars.next() else {
            panic!("hotkey must not be empty for {}", entry.id);
        };
        assert!(
            chars.next().is_none(),
            "hotkey must be one char for {}",
            entry.id
        );
        assert!(
            hotkeys.insert(ch),
            "duplicate hotkey {} in client catalog",
            ch
        );
    }
}

fn is_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
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
                "    ClientIdentity {{ id: {}, display_name: {}, short_name: {}, hotkey: {}, submit_default: {}, logo_url: {}, color: {}, text_color: {} }},\n",
                rust_string(&entry.id),
                rust_string(&entry.display_name),
                rust_string(&entry.short_name),
                rust_hotkey(&entry.hotkey),
                entry.submit_default,
                rust_string(&entry.logo),
                rust_string(&entry.color),
                rust_option_string(entry.text_color.as_deref()),
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
    pub short_name: &'static str,
    pub hotkey: Option<char>,
    pub submit_default: bool,
    pub logo_url: &'static str,
    pub color: &'static str,
    pub text_color: Option<&'static str>,
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

    pub fn short_name(self) -> &'static str {{
        self.identity().short_name
    }}

    pub fn hotkey(self) -> Option<char> {{
        self.identity().hotkey
    }}

    pub fn submit_default(self) -> bool {{
        self.identity().submit_default
    }}

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<ClientId> {{
        match s {{
{from_str}            _ => None,
        }}
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

fn rust_option_string(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("Some({})", rust_string(value)),
        None => "None".to_string(),
    }
}

fn rust_hotkey(value: &str) -> String {
    let ch = value
        .chars()
        .next()
        .expect("catalog validation rejects empty hotkeys");
    format!("Some({ch:?})")
}
