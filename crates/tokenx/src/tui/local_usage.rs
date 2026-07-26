#[cfg(test)]
use std::ops::{Deref, DerefMut};

use anyhow::Result;
use tokenx_engine::{Generation, UsageQuery};

use super::data::{build_period_usage, OverviewSummary, PeriodKind, PeriodUsage, UsageProjection};
use super::session_data::SessionSnapshot;

struct PeriodUsageCache {
    monthly: Vec<PeriodUsage>,
    weekly: Vec<PeriodUsage>,
}

impl PeriodUsageCache {
    fn new(view: &UsageProjection) -> Result<Self> {
        Ok(Self {
            monthly: build_period_usage(view, PeriodKind::Monthly)?,
            weekly: build_period_usage(view, PeriodKind::Weekly)?,
        })
    }

    fn get(&self, kind: PeriodKind) -> &[PeriodUsage] {
        match kind {
            PeriodKind::Monthly => &self.monthly,
            PeriodKind::Weekly => &self.weekly,
        }
    }
}

/// One coherent installed local generation and its current projection.
///
/// The query, materialized usage projection, period projections, overview summary,
/// and Sessions snapshot are replaced together. Renderers only borrow these values
/// through [`LocalUsageState`] accessors.
pub(crate) struct InstalledGeneration {
    query: UsageQuery,
    view: UsageProjection,
    periods: PeriodUsageCache,
    overview: OverviewSummary,
    sessions: SessionSnapshot,
    // Keep the authority last so renderer projections release their interned
    // identities before Generation's lifetime guard prunes the weak pool.
    generation: Generation,
}

pub(crate) struct PreparedProjection {
    query: UsageQuery,
    view: UsageProjection,
    periods: PeriodUsageCache,
    overview: OverviewSummary,
}

impl PreparedProjection {
    pub(crate) fn view(&self) -> &UsageProjection {
        &self.view
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalUsageStatus<'a> {
    Empty,
    Ready,
    Degraded { diagnostic: &'a str },
    Failed { diagnostic: &'a str },
}

/// Local generation lifecycle.
///
/// A warm failure becomes `Degraded` and retains that installed generation;
/// a cold failure becomes `Failed` and cannot masquerade as empty success.
pub(crate) enum LocalUsageState {
    Empty {
        query: UsageQuery,
    },
    Ready(Box<InstalledGeneration>),
    Degraded {
        installed: Box<InstalledGeneration>,
        diagnostic: String,
    },
    Failed {
        query: UsageQuery,
        diagnostic: String,
    },
}

impl InstalledGeneration {
    fn new(generation: Generation, query: UsageQuery) -> Result<Self> {
        let view = generation.project_usage(&query)?;
        let periods = PeriodUsageCache::new(&view)?;
        let sessions = SessionSnapshot::new(generation.sessions(), generation.input_footprint());
        let overview = derive_overview(&view, &sessions, &query);
        Ok(Self {
            query,
            view,
            periods,
            overview,
            sessions,
            generation,
        })
    }

    fn prepare_projection(&self, query: UsageQuery) -> Result<PreparedProjection> {
        let view = self.generation.project_usage(&query)?;
        let periods = PeriodUsageCache::new(&view)?;
        let overview = derive_overview(&view, &self.sessions, &query);
        Ok(PreparedProjection {
            query,
            view,
            periods,
            overview,
        })
    }

    fn install_projection(&mut self, projection: PreparedProjection) {
        self.query = projection.query;
        self.view = projection.view;
        self.periods = projection.periods;
        self.overview = projection.overview;
    }

    pub(crate) fn generation(&self) -> &Generation {
        &self.generation
    }

    pub(crate) fn view(&self) -> &UsageProjection {
        &self.view
    }

    pub(crate) fn periods(&self, kind: PeriodKind) -> &[PeriodUsage] {
        self.periods.get(kind)
    }

    pub(crate) fn overview(&self) -> &OverviewSummary {
        &self.overview
    }

    pub(crate) fn sessions(&self) -> &SessionSnapshot {
        &self.sessions
    }
}

impl LocalUsageState {
    pub(crate) fn new(query: UsageQuery) -> Self {
        Self::Empty { query }
    }

    pub(crate) fn status(&self) -> LocalUsageStatus<'_> {
        match self {
            Self::Empty { .. } => LocalUsageStatus::Empty,
            Self::Ready(_) => LocalUsageStatus::Ready,
            Self::Degraded { diagnostic, .. } => LocalUsageStatus::Degraded { diagnostic },
            Self::Failed { diagnostic, .. } => LocalUsageStatus::Failed { diagnostic },
        }
    }

    pub(crate) fn query(&self) -> &UsageQuery {
        match self {
            Self::Empty { query } | Self::Failed { query, .. } => query,
            Self::Ready(installed) | Self::Degraded { installed, .. } => &installed.query,
        }
    }

    pub(crate) fn installed(&self) -> Option<&InstalledGeneration> {
        match self {
            Self::Ready(installed) | Self::Degraded { installed, .. } => Some(installed),
            Self::Empty { .. } | Self::Failed { .. } => None,
        }
    }

    fn installed_mut(&mut self) -> Option<&mut InstalledGeneration> {
        match self {
            Self::Ready(installed) | Self::Degraded { installed, .. } => Some(installed),
            Self::Empty { .. } | Self::Failed { .. } => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn generation(&self) -> Option<&Generation> {
        self.installed().map(InstalledGeneration::generation)
    }

    pub(crate) fn fail_acquisition(&mut self, diagnostic: String) {
        let placeholder_query = self.query().clone();
        let previous = std::mem::replace(
            self,
            Self::Empty {
                query: placeholder_query,
            },
        );
        *self = match previous {
            Self::Ready(installed) | Self::Degraded { installed, .. } => Self::Degraded {
                installed,
                diagnostic,
            },
            Self::Empty { query } | Self::Failed { query, .. } => {
                Self::Failed { query, diagnostic }
            }
        };
    }

    pub(crate) fn install_generation(&mut self, generation: Generation) -> Result<()> {
        let installed = InstalledGeneration::new(generation, self.query().clone())?;
        *self = Self::Ready(Box::new(installed));
        Ok(())
    }

    pub(crate) fn project_view(&self, query: &UsageQuery) -> Result<UsageProjection> {
        let installed = self
            .installed()
            .ok_or_else(|| anyhow::anyhow!("local data generation is not installed"))?;
        Ok(installed.generation.project_usage(query)?)
    }

    pub(crate) fn prepare_projection(&self, query: UsageQuery) -> Result<PreparedProjection> {
        let installed = self
            .installed()
            .ok_or_else(|| anyhow::anyhow!("local data generation is not installed"))?;
        installed.prepare_projection(query)
    }

    pub(crate) fn install_projection(&mut self, projection: PreparedProjection) {
        self.installed_mut()
            .expect("prepared projection requires an installed generation")
            .install_projection(projection);
    }

    pub(crate) fn replace_uninstalled_query(&mut self, query: UsageQuery) {
        match self {
            Self::Empty {
                query: current_query,
            }
            | Self::Failed {
                query: current_query,
                ..
            } => *current_query = query,
            Self::Ready(_) | Self::Degraded { .. } => {
                panic!("installed local usage must be updated through a prepared projection")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn view_mut(&mut self) -> UsageProjectionMut<'_> {
        UsageProjectionMut {
            installed: self
                .installed_mut()
                .expect("test usage mutation requires an installed generation"),
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_view_for_test(&mut self, view: UsageProjection) {
        let periods = PeriodUsageCache::new(&view).expect("test period projection must succeed");
        let installed = self
            .installed_mut()
            .expect("test usage replacement requires an installed generation");
        installed.overview = derive_overview(&view, &installed.sessions, &installed.query);
        installed.view = view;
        installed.periods = periods;
    }

    #[cfg(test)]
    pub(crate) fn replace_sessions_for_test(&mut self, sessions: SessionSnapshot) {
        let installed = self
            .installed_mut()
            .expect("test session replacement requires an installed generation");
        installed.overview = derive_overview(&installed.view, &sessions, &installed.query);
        installed.sessions = sessions;
    }

    #[cfg(test)]
    pub(crate) fn set_query_for_test(&mut self, query: UsageQuery) {
        if self.installed().is_some() {
            let projection = self
                .prepare_projection(query)
                .expect("test projection must succeed");
            self.install_projection(projection);
            return;
        }

        match self {
            Self::Empty {
                query: current_query,
            }
            | Self::Failed {
                query: current_query,
                ..
            } => *current_query = query,
            Self::Ready(_) | Self::Degraded { .. } => {
                unreachable!("installed state handled before query-only mutation")
            }
        }
    }
}

#[cfg(test)]
pub(crate) struct UsageProjectionMut<'a> {
    installed: &'a mut InstalledGeneration,
}

#[cfg(test)]
impl Deref for UsageProjectionMut<'_> {
    type Target = UsageProjection;

    fn deref(&self) -> &Self::Target {
        &self.installed.view
    }
}

#[cfg(test)]
impl DerefMut for UsageProjectionMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.installed.view
    }
}

#[cfg(test)]
impl Drop for UsageProjectionMut<'_> {
    fn drop(&mut self) {
        self.installed.periods = PeriodUsageCache::new(&self.installed.view)
            .expect("test period projection must succeed");
    }
}

fn derive_overview(
    view: &UsageProjection,
    sessions: &SessionSnapshot,
    query: &UsageQuery,
) -> OverviewSummary {
    let main_session_count = sessions
        .client_summaries()
        .iter()
        .filter(|summary| query.clients.iter().any(|client| client == summary.client))
        .map(|summary| summary.main_session_count)
        .sum();
    OverviewSummary::derive(view, main_session_count)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::super::data::{DailyUsage, UsageTokenBreakdown};
    use super::*;
    use tokenx_engine::{ClientId, ClientSelection, ClientUniverse, GroupBy};

    fn query(client: ClientId) -> UsageQuery {
        UsageQuery {
            clients: ClientSelection::new([client]).unwrap(),
            group_by: GroupBy::Model,
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        }
    }

    fn generation(client: ClientId) -> Generation {
        super::super::generation_fixture_with_health(
            [client],
            tokenx_engine::FrozenUsageIndex::new(),
            Vec::new(),
            tokenx_engine::InputFootprint::default(),
            tokenx_engine::input_health::HealthSummary::default(),
        )
    }

    fn daily_usage(date: chrono::NaiveDate, tokens: UsageTokenBreakdown) -> DailyUsage {
        DailyUsage {
            date,
            tokens,
            cost: 0.0,
            client_breakdown: BTreeMap::new(),
            message_count: 1,
            turn_count: 1,
        }
    }

    #[test]
    fn cold_failure_is_explicit_and_has_no_installed_snapshot() {
        let mut state = LocalUsageState::new(query(ClientId::Codex));
        state.fail_acquisition("scan failed".to_string());

        assert!(matches!(
            state.status(),
            LocalUsageStatus::Failed {
                diagnostic: "scan failed"
            }
        ));
        assert!(state.installed().is_none());
    }

    #[test]
    fn warm_failure_retains_the_complete_installed_generation() {
        let mut state = LocalUsageState::new(query(ClientId::Codex));
        state
            .install_generation(generation(ClientId::Codex))
            .unwrap();
        let generation_universe = state.generation().unwrap().universe().clone();
        let installed_session_count = state
            .installed()
            .unwrap()
            .sessions()
            .client_summaries()
            .len();

        state.fail_acquisition("database locked".to_string());

        assert!(matches!(
            state.status(),
            LocalUsageStatus::Degraded {
                diagnostic: "database locked"
            }
        ));
        assert_eq!(state.generation().unwrap().universe(), &generation_universe);
        let installed = state.installed().unwrap();
        assert!(installed.view().models.is_empty());
        assert_eq!(
            installed.sessions().client_summaries().len(),
            installed_session_count
        );
    }

    #[test]
    fn failed_install_does_not_publish_a_partial_generation() {
        let universe = ClientUniverse::new([ClientId::Claude]).unwrap();
        let mut state = LocalUsageState::new(UsageQuery::full(
            &universe,
            GroupBy::Model,
            chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        ));
        assert!(state
            .install_generation(generation(ClientId::Codex))
            .is_err());
        assert_eq!(state.status(), LocalUsageStatus::Empty);
        assert!(state.installed().is_none());
    }

    #[test]
    fn installed_generation_reuses_materialized_period_projections() {
        let mut state = LocalUsageState::new(query(ClientId::Codex));
        state
            .install_generation(generation(ClientId::Codex))
            .unwrap();
        state.view_mut().daily = vec![daily_usage(
            chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            UsageTokenBreakdown {
                input: 42,
                ..UsageTokenBreakdown::default()
            },
        )];

        let installed = state.installed().unwrap();
        let monthly = installed.periods(PeriodKind::Monthly);
        let weekly = installed.periods(PeriodKind::Weekly);

        assert_eq!(monthly.len(), 1);
        assert_eq!(weekly.len(), 1);
        assert_eq!(monthly[0].tokens.input, 42);
        assert_eq!(
            monthly.as_ptr(),
            installed.periods(PeriodKind::Monthly).as_ptr()
        );
    }

    #[test]
    fn period_cache_creation_returns_projection_errors_before_install() {
        let view = UsageProjection {
            daily: vec![daily_usage(
                chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
                UsageTokenBreakdown {
                    input: u64::MAX,
                    output: 1,
                    ..UsageTokenBreakdown::default()
                },
            )],
            ..UsageProjection::default()
        };

        let error = PeriodUsageCache::new(&view)
            .err()
            .expect("overflowing period projection must fail");

        let diagnostic = format!("{error:#}");
        assert!(
            diagnostic.contains("period token totals"),
            "unexpected projection error: {diagnostic}"
        );
    }
}
