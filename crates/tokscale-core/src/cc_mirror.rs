use serde::Deserialize;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::sessions::error::{SessionParseError, SessionParseResult};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VariantFile {
    pub(crate) name: Option<String>,
    pub(crate) provider: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) config_dir: Option<String>,
}

pub(crate) fn read_variant_file_checked(
    variant_path: &Path,
) -> SessionParseResult<Option<VariantFile>> {
    let contents = match std::fs::read_to_string(variant_path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(SessionParseError::at_path(
                variant_path,
                "read cc-mirror variant metadata",
                source,
            ));
        }
    };
    serde_json::from_str(&contents).map(Some).map_err(|source| {
        SessionParseError::at_path(variant_path, "decode cc-mirror variant metadata", source)
    })
}

pub(crate) fn variant_file_path(variant_dir: &Path) -> PathBuf {
    variant_dir.join("variant.json")
}

struct ResolvedVariant {
    variant_dir: PathBuf,
    projects_dir: PathBuf,
}

fn resolved_variants(home_dir: &Path) -> SessionParseResult<Vec<ResolvedVariant>> {
    let root = home_dir.join(".cc-mirror");
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(SessionParseError::at_path(
                &root,
                "read cc-mirror variant directory",
                source,
            ));
        }
    };

    let mut variants = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| {
            SessionParseError::at_path(&root, "read cc-mirror variant entry", source)
        })?;
        let variant_dir = entry.path();
        let file_type = entry.file_type().map_err(|source| {
            SessionParseError::at_path(&variant_dir, "read cc-mirror variant entry type", source)
        })?;
        if !file_type.is_dir() {
            continue;
        }

        let variant_path = variant_file_path(&variant_dir);
        let Some(metadata) = read_variant_file_checked(&variant_path)? else {
            continue;
        };
        let config_dir =
            config_dir_from_variant_metadata(&metadata, &variant_path, home_dir, &variant_dir)?;
        variants.push(ResolvedVariant {
            variant_dir,
            projects_dir: config_dir.join("projects"),
        });
    }
    Ok(variants)
}

pub(crate) fn discover_claude_project_roots(home_dir: &Path) -> SessionParseResult<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for variant in resolved_variants(home_dir)? {
        let projects_dir = variant.projects_dir;
        match std::fs::metadata(&projects_dir) {
            Ok(metadata) if metadata.is_dir() => roots.push(projects_dir),
            Ok(_) => {
                return Err(SessionParseError::at_path(
                    &projects_dir,
                    "validate cc-mirror projects directory",
                    std::io::Error::new(ErrorKind::InvalidData, "projects path is not a directory"),
                ));
            }
            Err(source) if source.kind() == ErrorKind::NotFound => {}
            Err(source) => {
                return Err(SessionParseError::at_path(
                    &projects_dir,
                    "read cc-mirror projects directory metadata",
                    source,
                ));
            }
        }
    }
    roots.sort_unstable();
    Ok(roots)
}

pub(crate) fn variant_file_for_session_path_checked(
    path: &Path,
    home_dir: Option<&Path>,
) -> SessionParseResult<Option<PathBuf>> {
    Ok(variant_dir_from_session_path_checked(path, home_dir)?
        .map(|variant_dir| variant_file_path(&variant_dir)))
}

pub(crate) fn variant_dir_from_session_path_checked(
    path: &Path,
    home_dir: Option<&Path>,
) -> SessionParseResult<Option<PathBuf>> {
    if let Some(variant_dir) = default_layout_variant_dir_from_session_path(path) {
        return Ok(Some(variant_dir));
    }
    let Some(home_dir) = home_dir else {
        return Ok(None);
    };

    let normal_claude_projects = home_dir.join(".claude").join("projects");
    let mut candidates = Vec::new();
    for variant in resolved_variants(home_dir)? {
        let projects_dir = variant.projects_dir;
        if projects_dir == normal_claude_projects || !path.starts_with(&projects_dir) {
            continue;
        }
        candidates.push((projects_dir.components().count(), variant.variant_dir));
    }

    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    Ok(candidates
        .into_iter()
        .map(|(_, variant_dir)| variant_dir)
        .next())
}

fn default_layout_variant_dir_from_session_path(path: &Path) -> Option<PathBuf> {
    for ancestor in path.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) != Some("config") {
            continue;
        }
        if !path.starts_with(ancestor.join("projects")) {
            continue;
        }
        let variant_dir = ancestor.parent()?;
        if variant_dir
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            == Some(".cc-mirror")
        {
            return Some(variant_dir.to_path_buf());
        }
    }

    None
}

fn config_dir_from_variant_metadata(
    metadata: &VariantFile,
    variant_path: &Path,
    home_dir: &Path,
    variant_dir: &Path,
) -> SessionParseResult<PathBuf> {
    match metadata.config_dir.as_deref() {
        Some(raw) => expand_config_dir(raw, home_dir, variant_dir).ok_or_else(|| {
            SessionParseError::at_path(
                variant_path,
                "validate cc-mirror variant metadata",
                std::io::Error::new(ErrorKind::InvalidData, "configDir is blank"),
            )
        }),
        None => Ok(variant_dir.join("config")),
    }
}

fn expand_config_dir(raw: &str, home_dir: &Path, variant_dir: &Path) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix("~/") {
        return Some(home_dir.join(rest));
    }

    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        Some(path)
    } else {
        Some(variant_dir.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_variant(home: &Path, name: &str, config_dir: &str) -> PathBuf {
        let variant_dir = home.join(".cc-mirror").join(name);
        std::fs::create_dir_all(&variant_dir).unwrap();
        std::fs::write(
            variant_file_path(&variant_dir),
            serde_json::json!({ "configDir": config_dir }).to_string(),
        )
        .unwrap();
        variant_dir
    }

    #[test]
    fn normal_claude_projects_are_not_mapped_to_a_mirror_variant() {
        let home = tempfile::TempDir::new().unwrap();
        write_variant(home.path(), "normal-alias", "~/.claude");
        let session = home.path().join(".claude/projects/workspace/session.jsonl");

        assert_eq!(
            variant_dir_from_session_path_checked(&session, Some(home.path())).unwrap(),
            None
        );
    }

    #[test]
    fn normal_claude_transcripts_are_not_mapped_to_a_mirror_variant() {
        let home = tempfile::TempDir::new().unwrap();
        write_variant(home.path(), "custom", "~/.claude/custom");
        let transcript = home.path().join(".claude/transcripts/session.jsonl");

        assert_eq!(
            variant_dir_from_session_path_checked(&transcript, Some(home.path())).unwrap(),
            None
        );
    }

    #[test]
    fn custom_config_dir_inside_dot_claude_maps_to_its_variant() {
        let home = tempfile::TempDir::new().unwrap();
        let variant = write_variant(home.path(), "custom", "~/.claude/custom");
        let session = home
            .path()
            .join(".claude/custom/projects/workspace/session.jsonl");

        assert_eq!(
            variant_dir_from_session_path_checked(&session, Some(home.path())).unwrap(),
            Some(variant)
        );
    }

    #[test]
    fn nested_custom_config_dirs_choose_the_longest_matching_variant() {
        let home = tempfile::TempDir::new().unwrap();
        let outer_config = home.path().join(".claude/custom");
        let inner_config = outer_config.join("projects/team");
        write_variant(
            home.path(),
            "outer",
            outer_config.to_str().expect("temp path must be UTF-8"),
        );
        let inner = write_variant(
            home.path(),
            "inner",
            inner_config.to_str().expect("temp path must be UTF-8"),
        );
        let session = inner_config.join("projects/workspace/session.jsonl");

        assert_eq!(
            variant_dir_from_session_path_checked(&session, Some(home.path())).unwrap(),
            Some(inner)
        );
    }
}
