#[cfg(test)]
use std::ops::{Deref, DerefMut};
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use tokenx_engine::{ClientId, Generation, GroupBy, UsageQuery};

use super::data::{
    build_period_usage, DailyClientInfo, OverviewSummary, PeriodKind, PeriodUsage, UsageProjection,
    UsageTokenBreakdown,
};
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

#[derive(Debug, Clone)]
pub(crate) struct DetailRow {
    pub(crate) clients: Arc<[ClientId]>,
    pub(crate) provider: Arc<str>,
    pub(crate) model: Arc<str>,
    pub(crate) model_id: Arc<str>,
    pub(crate) workspace: Option<Arc<str>>,
    pub(crate) tokens: UsageTokenBreakdown,
    pub(crate) cost: f64,
    pub(crate) messages: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeriodDetailSelection {
    pub kind: PeriodKind,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DetailSelections {
    pub(crate) daily: Option<NaiveDate>,
    pub(crate) period: Option<PeriodDetailSelection>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
enum DetailModelIdentity {
    Model(Arc<str>),
    ClientModel(ClientId, Arc<str>),
    ClientProviderModel(ClientId, Arc<str>, Arc<str>),
    WorkspaceModel(Option<Arc<str>>, Arc<str>),
}

struct DetailRowAccumulator {
    client_totals: HashMap<ClientId, ClientContributionOrder>,
    providers: Vec<Arc<str>>,
    model: Arc<str>,
    model_id: Arc<str>,
    workspace: Option<Arc<str>>,
    tokens: UsageTokenBreakdown,
    cost: f64,
    messages: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ClientContributionOrder {
    first_seen: usize,
    total_tokens: u64,
}

#[derive(Default)]
struct DetailProjectionCache {
    daily: Option<(NaiveDate, Arc<[DetailRow]>)>,
    period: Option<(PeriodDetailSelection, Arc<[DetailRow]>)>,
}

impl DetailProjectionCache {
    fn materialize_selected(
        view: &UsageProjection,
        periods: &PeriodUsageCache,
        selections: DetailSelections,
    ) -> Result<Self> {
        let mut cache = Self::default();
        if let Some(date) = selections.daily {
            cache.materialize_daily(view, date)?;
        }
        if let Some(selection) = selections.period {
            cache.materialize_period(periods, selection, view.group_by)?;
        }
        Ok(cache)
    }

    fn materialize_daily(&mut self, view: &UsageProjection, date: NaiveDate) -> Result<()> {
        if self
            .daily
            .as_ref()
            .is_some_and(|(cached_date, _)| *cached_date == date)
        {
            return Ok(());
        }
        let Some(day) = view.daily.iter().find(|day| day.date == date) else {
            self.daily = None;
            return Ok(());
        };
        let rows = build_detail_rows(&day.client_breakdown, view.group_by)
            .with_context(|| format!("failed to materialize daily detail for {date}"))?;
        self.daily = Some((date, rows));
        Ok(())
    }

    fn materialize_period(
        &mut self,
        periods: &PeriodUsageCache,
        selection: PeriodDetailSelection,
        group_by: GroupBy,
    ) -> Result<()> {
        if self
            .period
            .as_ref()
            .is_some_and(|(cached_selection, _)| *cached_selection == selection)
        {
            return Ok(());
        }
        let Some(period) = periods.get(selection.kind).iter().find(|period| {
            period.start_date == selection.start_date && period.end_date == selection.end_date
        }) else {
            self.period = None;
            return Ok(());
        };
        let rows = build_detail_rows(&period.client_breakdown, group_by).with_context(|| {
            format!(
                "failed to materialize {:?} detail for {} through {}",
                selection.kind, selection.start_date, selection.end_date
            )
        })?;
        self.period = Some((selection, rows));
        Ok(())
    }

    fn daily(&self, date: NaiveDate) -> &[DetailRow] {
        self.daily
            .as_ref()
            .filter(|(cached_date, _)| *cached_date == date)
            .map(|(_, rows)| rows.as_ref())
            .unwrap_or_default()
    }

    fn period(&self, selection: PeriodDetailSelection) -> &[DetailRow] {
        self.period
            .as_ref()
            .filter(|(cached_selection, _)| *cached_selection == selection)
            .map(|(_, rows)| rows.as_ref())
            .unwrap_or_default()
    }
}

fn detail_model_identity(
    client: ClientId,
    model: &tokenx_engine::projection::DailyModelInfo,
    group_by: GroupBy,
) -> DetailModelIdentity {
    match group_by {
        GroupBy::Model => DetailModelIdentity::Model(Arc::clone(&model.model_id)),
        GroupBy::ClientModel => {
            DetailModelIdentity::ClientModel(client, Arc::clone(&model.model_id))
        }
        GroupBy::ClientProviderModel => DetailModelIdentity::ClientProviderModel(
            client,
            Arc::clone(&model.provider),
            Arc::clone(&model.model_id),
        ),
        GroupBy::WorkspaceModel => DetailModelIdentity::WorkspaceModel(
            model.workspace_key.clone(),
            Arc::clone(&model.model_id),
        ),
    }
}

fn ordered_clients_by_token_contribution(
    client_totals: &HashMap<ClientId, ClientContributionOrder>,
) -> Arc<[ClientId]> {
    let mut clients = client_totals
        .iter()
        .map(|(client, totals)| (*client, *totals))
        .collect::<Vec<_>>();
    clients.sort_by(|(left_client, left), (right_client, right)| {
        right
            .total_tokens
            .cmp(&left.total_tokens)
            .then_with(|| left.first_seen.cmp(&right.first_seen))
            .then_with(|| left_client.cmp(right_client))
    });
    clients
        .into_iter()
        .map(|(client, _)| client)
        .collect::<Vec<_>>()
        .into()
}

fn provider_label(providers: &[Arc<str>]) -> Arc<str> {
    match providers {
        [] => Arc::from(""),
        [provider] => Arc::clone(provider),
        providers => providers
            .iter()
            .map(|provider| provider.as_ref())
            .collect::<Vec<_>>()
            .join(", ")
            .into(),
    }
}

fn build_detail_rows(
    client_breakdown: &BTreeMap<ClientId, DailyClientInfo>,
    group_by: GroupBy,
) -> Result<Arc<[DetailRow]>> {
    let mut rows_by_key: BTreeMap<DetailModelIdentity, DetailRowAccumulator> = BTreeMap::new();

    for (client, client_info) in client_breakdown {
        for model_info in &client_info.models {
            let row = rows_by_key
                .entry(detail_model_identity(*client, model_info, group_by))
                .or_insert_with(|| DetailRowAccumulator {
                    client_totals: HashMap::new(),
                    providers: Vec::new(),
                    model: if model_info.display_name.is_empty() {
                        Arc::clone(&model_info.model_id)
                    } else {
                        Arc::clone(&model_info.display_name)
                    },
                    model_id: Arc::clone(&model_info.model_id),
                    workspace: model_info
                        .workspace_label
                        .clone()
                        .or_else(|| model_info.workspace_key.clone()),
                    tokens: UsageTokenBreakdown::default(),
                    cost: 0.0,
                    messages: 0,
                });

            let model_total = model_info.tokens.checked_total().ok_or_else(|| {
                anyhow::anyhow!(
                    "detail token total overflow for client {} model {}",
                    client,
                    model_info.model_id
                )
            })?;
            let client_count = row.client_totals.len();
            let client_total =
                row.client_totals
                    .entry(*client)
                    .or_insert_with(|| ClientContributionOrder {
                        first_seen: client_count,
                        total_tokens: 0,
                    });
            client_total.total_tokens = client_total
                .total_tokens
                .checked_add(model_total)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "detail client token total overflow for client {} model {}",
                        client,
                        model_info.model_id
                    )
                })?;

            if !model_info.provider.is_empty()
                && !row
                    .providers
                    .iter()
                    .any(|provider| provider == &model_info.provider)
            {
                row.providers.push(Arc::clone(&model_info.provider));
            }
            row.tokens = row.tokens.checked_add(&model_info.tokens).ok_or_else(|| {
                anyhow::anyhow!(
                    "detail token bucket overflow for model {}",
                    model_info.model_id
                )
            })?;
            if row.tokens.checked_total().is_none() {
                anyhow::bail!(
                    "detail token total overflow for model {}",
                    model_info.model_id
                );
            }
            row.cost += model_info.cost;
            if !row.cost.is_finite() {
                anyhow::bail!("detail cost overflow for model {}", model_info.model_id);
            }
            row.messages = row
                .messages
                .checked_add(model_info.messages)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "detail message count overflow for model {}",
                        model_info.model_id
                    )
                })?;
        }
    }

    Ok(rows_by_key
        .into_values()
        .map(|row| DetailRow {
            clients: ordered_clients_by_token_contribution(&row.client_totals),
            provider: provider_label(&row.providers),
            model: row.model,
            model_id: row.model_id,
            workspace: row.workspace,
            tokens: row.tokens,
            cost: row.cost,
            messages: row.messages,
        })
        .collect::<Vec<_>>()
        .into())
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
    details: DetailProjectionCache,
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
    details: DetailProjectionCache,
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
    fn new(
        generation: Generation,
        query: UsageQuery,
        detail_selections: DetailSelections,
    ) -> Result<Self> {
        let view = generation.project_usage(&query)?;
        let periods = PeriodUsageCache::new(&view)?;
        let details =
            DetailProjectionCache::materialize_selected(&view, &periods, detail_selections)?;
        let sessions = SessionSnapshot::new(generation.sessions(), generation.input_footprint());
        let overview = derive_overview(&view, &sessions, &query);
        Ok(Self {
            query,
            view,
            periods,
            details,
            overview,
            sessions,
            generation,
        })
    }

    fn prepare_projection(
        &self,
        query: UsageQuery,
        detail_selections: DetailSelections,
    ) -> Result<PreparedProjection> {
        let view = self.generation.project_usage(&query)?;
        let periods = PeriodUsageCache::new(&view)?;
        let details =
            DetailProjectionCache::materialize_selected(&view, &periods, detail_selections)?;
        let overview = derive_overview(&view, &self.sessions, &query);
        Ok(PreparedProjection {
            query,
            view,
            periods,
            details,
            overview,
        })
    }

    fn install_projection(&mut self, projection: PreparedProjection) {
        self.query = projection.query;
        self.view = projection.view;
        self.periods = projection.periods;
        self.details = projection.details;
        self.overview = projection.overview;
    }

    fn materialize_daily_detail(&mut self, date: NaiveDate) -> Result<()> {
        self.details.materialize_daily(&self.view, date)
    }

    fn materialize_period_detail(&mut self, selection: PeriodDetailSelection) -> Result<()> {
        self.details
            .materialize_period(&self.periods, selection, self.query.group_by)
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

    pub(crate) fn daily_detail(&self, date: NaiveDate) -> &[DetailRow] {
        self.details.daily(date)
    }

    pub(crate) fn period_detail(&self, selection: PeriodDetailSelection) -> &[DetailRow] {
        self.details.period(selection)
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

    pub(crate) fn install_generation(
        &mut self,
        generation: Generation,
        detail_selections: DetailSelections,
    ) -> Result<()> {
        let installed =
            InstalledGeneration::new(generation, self.query().clone(), detail_selections)?;
        *self = Self::Ready(Box::new(installed));
        Ok(())
    }

    pub(crate) fn project_view(&self, query: &UsageQuery) -> Result<UsageProjection> {
        let installed = self
            .installed()
            .ok_or_else(|| anyhow::anyhow!("local data generation is not installed"))?;
        Ok(installed.generation.project_usage(query)?)
    }

    pub(crate) fn prepare_projection(
        &self,
        query: UsageQuery,
        detail_selections: DetailSelections,
    ) -> Result<PreparedProjection> {
        let installed = self
            .installed()
            .ok_or_else(|| anyhow::anyhow!("local data generation is not installed"))?;
        installed.prepare_projection(query, detail_selections)
    }

    pub(crate) fn install_projection(&mut self, projection: PreparedProjection) {
        self.installed_mut()
            .expect("prepared projection requires an installed generation")
            .install_projection(projection);
    }

    pub(crate) fn materialize_daily_detail(&mut self, date: NaiveDate) -> Result<()> {
        self.installed_mut()
            .ok_or_else(|| anyhow::anyhow!("local data generation is not installed"))?
            .materialize_daily_detail(date)
    }

    pub(crate) fn materialize_period_detail(
        &mut self,
        selection: PeriodDetailSelection,
    ) -> Result<()> {
        self.installed_mut()
            .ok_or_else(|| anyhow::anyhow!("local data generation is not installed"))?
            .materialize_period_detail(selection)
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
    pub(crate) fn replace_view_for_test(
        &mut self,
        view: UsageProjection,
        detail_selections: DetailSelections,
    ) {
        let periods = PeriodUsageCache::new(&view).expect("test period projection must succeed");
        let details =
            DetailProjectionCache::materialize_selected(&view, &periods, detail_selections)
                .expect("test detail projection must succeed");
        let installed = self
            .installed_mut()
            .expect("test usage replacement requires an installed generation");
        installed.overview = derive_overview(&view, &installed.sessions, &installed.query);
        installed.view = view;
        installed.periods = periods;
        installed.details = details;
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
                .prepare_projection(query, DetailSelections::default())
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
        self.installed.details = DetailProjectionCache::default();
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

    use super::super::data::{DailyClientInfo, DailyModelInfo, DailyUsage, UsageTokenBreakdown};
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

    fn detail_model(model_id: Arc<str>, tokens: u64) -> DailyClientInfo {
        let tokens = UsageTokenBreakdown {
            input: tokens,
            ..UsageTokenBreakdown::default()
        };
        DailyClientInfo {
            tokens: tokens.clone(),
            cost: 0.0,
            models: vec![DailyModelInfo {
                provider: "openai".into(),
                model_id: Arc::clone(&model_id),
                display_name: Arc::clone(&model_id),
                workspace_key: None,
                workspace_label: None,
                tokens,
                cost: 0.0,
                messages: 1,
            }],
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
            .install_generation(generation(ClientId::Codex), DetailSelections::default())
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
            .install_generation(generation(ClientId::Codex), DetailSelections::default())
            .is_err());
        assert_eq!(state.status(), LocalUsageStatus::Empty);
        assert!(state.installed().is_none());
    }

    #[test]
    fn installed_generation_reuses_materialized_period_projections() {
        let mut state = LocalUsageState::new(query(ClientId::Codex));
        state
            .install_generation(generation(ClientId::Codex), DetailSelections::default())
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
    fn detail_cache_reuses_same_key_and_interned_model_identity() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap();
        let model_id: Arc<str> = "gpt-5".into();
        let view = UsageProjection {
            group_by: GroupBy::Model,
            daily: vec![DailyUsage {
                date,
                tokens: UsageTokenBreakdown {
                    input: 42,
                    ..UsageTokenBreakdown::default()
                },
                cost: 0.0,
                client_breakdown: BTreeMap::from([(
                    ClientId::Codex,
                    detail_model(Arc::clone(&model_id), 42),
                )]),
                message_count: 1,
                turn_count: 1,
            }],
            ..UsageProjection::default()
        };
        let mut cache = DetailProjectionCache::default();

        cache.materialize_daily(&view, date).unwrap();
        let first = Arc::clone(&cache.daily.as_ref().unwrap().1);
        cache.materialize_daily(&view, date).unwrap();
        let repeated = Arc::clone(&cache.daily.as_ref().unwrap().1);

        assert!(Arc::ptr_eq(&first, &repeated));
        assert!(Arc::ptr_eq(&first[0].model_id, &model_id));
    }

    #[test]
    fn detail_materialization_reports_cross_client_overflow() {
        let model_id: Arc<str> = "gpt-5".into();
        let client_breakdown = BTreeMap::from([
            (
                ClientId::Claude,
                detail_model(Arc::clone(&model_id), u64::MAX),
            ),
            (ClientId::Codex, detail_model(Arc::clone(&model_id), 1)),
        ]);

        let error = build_detail_rows(&client_breakdown, GroupBy::Model)
            .expect_err("cross-client detail overflow must fail");

        assert!(format!("{error:#}").contains("detail token bucket overflow"));
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
