use std::path::{Path, PathBuf};

use rayon::prelude::*;
use serde::Deserialize;

use crate::adapters::{
    AdapterScanContext, FoldContext, LocalSourceAdapter, MessageSink, ParseContext, ParsedUnit,
    SourceUnit, SourceUnitMeta, UnitMessageSource,
};
use crate::clients::ClientId;
use crate::sessions;

pub(crate) struct CrushAdapter;

#[derive(Deserialize)]
struct CrushProjectList {
    projects: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct CrushProject {
    path: String,
    data_dir: String,
}

impl LocalSourceAdapter for CrushAdapter {
    fn client(&self) -> ClientId {
        ClientId::Crush
    }

    fn discover(&self, ctx: &AdapterScanContext<'_>) -> Vec<SourceUnit> {
        let def = ClientId::Crush
            .local_def()
            .expect("Crush adapter must have local scan policy");
        let registry_path =
            PathBuf::from(def.resolve_path_with_env_strategy(ctx.home_dir, ctx.use_env_roots));
        discover_crush_units(&registry_path)
    }

    fn parse(&self, units: Vec<SourceUnit>, ctx: &ParseContext<'_>) -> Vec<ParsedUnit> {
        units
            .into_par_iter()
            .map(|unit| {
                let mut messages = sessions::crush::parse_crush_sqlite(&unit.path);
                let (workspace_key, workspace_label) = match &unit.meta {
                    SourceUnitMeta::Crush {
                        workspace_key,
                        workspace_label,
                    } => (workspace_key.clone(), workspace_label.clone()),
                    _ => unreachable!("unexpected Crush source unit meta"),
                };
                for message in &mut messages {
                    message.set_workspace(workspace_key.clone(), workspace_label.clone());
                }
                crate::finalize_token_priced_messages(&mut messages, ctx.pricing);
                ParsedUnit {
                    unit,
                    messages: UnitMessageSource::Fresh(messages),
                    cache_entry: None,
                    invalidate_cache: false,
                }
            })
            .collect()
    }

    fn fold(
        &self,
        parsed: Vec<ParsedUnit>,
        _ctx: &mut FoldContext<'_>,
        sink: &mut dyn MessageSink,
    ) {
        for unit in parsed {
            if let UnitMessageSource::Fresh(messages) = unit.messages {
                sink.extend_messages(messages);
            }
        }
    }
}

fn discover_crush_units(registry_path: &Path) -> Vec<SourceUnit> {
    let registry = match std::fs::read_to_string(registry_path) {
        Ok(contents) => contents,
        Err(_) => return Vec::new(),
    };
    let list: CrushProjectList = match serde_json::from_str(&registry) {
        Ok(list) => list,
        Err(_) => return Vec::new(),
    };

    let mut units: Vec<SourceUnit> = list
        .projects
        .into_iter()
        .filter_map(|project| serde_json::from_value::<CrushProject>(project).ok())
        .filter_map(|project| {
            let db_path = crush_db_path(&resolve_crush_data_dir(&project))?;
            let workspace_key = sessions::normalize_workspace_key(&project.path);
            let workspace_label = workspace_key
                .as_deref()
                .and_then(sessions::workspace_label_from_key);
            Some(
                SourceUnit::sqlite_with_wal(ClientId::Crush, db_path).with_meta(
                    SourceUnitMeta::Crush {
                        workspace_key,
                        workspace_label,
                    },
                ),
            )
        })
        .collect();

    units.sort_by(|left, right| left.path.cmp(&right.path));
    units.dedup_by(|left, right| left.path == right.path);
    units
}

fn resolve_crush_data_dir(project: &CrushProject) -> PathBuf {
    let data_dir = PathBuf::from(&project.data_dir);
    if data_dir.is_absolute() {
        data_dir
    } else {
        PathBuf::from(&project.path).join(data_dir)
    }
}

fn crush_db_path(data_dir: &Path) -> Option<PathBuf> {
    let candidate = data_dir.join("crush.db");
    candidate.is_file().then_some(candidate)
}

pub(crate) static CRUSH_ADAPTER: CrushAdapter = CrushAdapter;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crush_adapter_discovers_registry_dbs_with_workspace_meta() {
        let home = tempfile::TempDir::new().unwrap();
        let project_dir = home.path().join("work/project-a");
        let data_dir = project_dir.join(".crush-data");
        let db_path = data_dir.join("crush.db");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(&db_path, "").unwrap();

        let registry_path = home.path().join(".local/share/crush/projects.json");
        std::fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
        std::fs::write(
            &registry_path,
            format!(
                r#"{{"projects":[{{"path":"{}","data_dir":".crush-data"}}]}}"#,
                project_dir.to_string_lossy()
            ),
        )
        .unwrap();
        let settings = crate::scanner::ScannerSettings::default();
        let ctx = AdapterScanContext {
            home_dir: home.path().to_str().unwrap(),
            use_env_roots: false,
            scanner_settings: &settings,
        };

        let units = CRUSH_ADAPTER.discover(&ctx);

        assert_eq!(units.len(), 1);
        assert_eq!(units[0].path, db_path);
        match &units[0].meta {
            SourceUnitMeta::Crush {
                workspace_key,
                workspace_label,
            } => {
                assert_eq!(
                    workspace_key.as_deref(),
                    Some(project_dir.to_string_lossy().as_ref())
                );
                assert_eq!(workspace_label.as_deref(), Some("project-a"));
            }
            other => panic!("unexpected Crush source meta: {other:?}"),
        }
    }
}
