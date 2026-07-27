#[cfg(test)]
mod map_as_vec_tests {
    use std::collections::HashMap;

    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    struct Wrapper {
        #[serde(with = "super::super::wire::map_as_vec")]
        values: HashMap<String, u64>,
    }

    #[test]
    fn duplicate_canonical_map_keys_are_rejected() {
        let error =
            serde_json::from_str::<Wrapper>(r#"{"values":[["duplicate",1],["duplicate",2]]}"#)
                .unwrap_err();

        assert!(error
            .to_string()
            .contains("duplicate key in canonical usage-index map"));
    }

    #[test]
    fn unique_canonical_map_keys_round_trip() {
        let parsed =
            serde_json::from_str::<Wrapper>(r#"{"values":[["first",1],["second",2]]}"#).unwrap();

        assert_eq!(parsed.values.len(), 2);
        assert_eq!(parsed.values["first"], 1);
        assert_eq!(parsed.values["second"], 2);
    }

    #[test]
    fn canonical_map_streams_large_entry_sequences_in_both_directions() {
        let expected = Wrapper {
            values: (0..4_096)
                .map(|index| (format!("key-{index}"), index))
                .collect(),
        };

        let encoded = bincode::serialize(&expected).unwrap();
        let restored: Wrapper = bincode::deserialize(&encoded).unwrap();

        assert_eq!(restored.values, expected.values);
    }

    #[test]
    fn canonical_map_serialization_retains_the_sequence_wire_shape() {
        let wrapper = Wrapper {
            values: HashMap::from([("only".to_string(), 7)]),
        };

        let encoded = serde_json::to_value(wrapper).unwrap();

        assert_eq!(encoded, serde_json::json!({"values": [["only", 7]]}));
    }

    #[test]
    fn oversized_sequence_hints_have_a_bounded_initial_allocation() {
        assert_eq!(
            super::super::wire::map_as_vec::initial_capacity(Some(usize::MAX)),
            super::super::wire::map_as_vec::MAX_INITIAL_CAPACITY
        );
        assert_eq!(super::super::wire::map_as_vec::initial_capacity(Some(7)), 7);
        assert_eq!(super::super::wire::map_as_vec::initial_capacity(None), 0);
    }
}

#[cfg(test)]
mod one_or_many_tests {
    use super::super::index::OneOrMany;

    #[test]
    fn iterates_singleton() {
        let values = OneOrMany::One((2, "second"));

        assert_eq!(
            values
                .into_stable_iter_by_key(|(key, _)| *key)
                .collect::<Vec<_>>(),
            vec![(2, "second")]
        );
    }

    #[test]
    fn promotes_to_many_and_sorts_stably() {
        let mut values = OneOrMany::One((2, "second"));
        values.push((1, "first"));
        values.push((2, "third"));

        assert_eq!(
            values
                .into_stable_iter_by_key(|(key, _)| *key)
                .collect::<Vec<_>>(),
            vec![(1, "first"), (2, "second"), (2, "third")]
        );
    }
}

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Arc,
};

use chrono::{Datelike, NaiveDate};

use super::index::{AgentInstanceKey, DailyBucket, DailyClientBucket};
use super::*;
use crate::aggregate::keys::{IdentitySet, WorkspaceKey, UNKNOWN_WORKSPACE_LABEL};
use crate::projection::{
    AgentEntry, ContributionDay, ContributionGrade, DailyClientInfo, DailyModelInfo, DailyUsage,
    HourlyUsage, PeriodKind, UsageGraphData, UsageProjection, UsageTokenBreakdown,
};
use crate::records::AttributedUsageRecord;
use crate::{ClientId, ClientUniverse, GroupBy};

struct TuiUsageHarness;

fn projection_date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 7, 26).unwrap()
}

impl TuiUsageHarness {
    fn aggregate_messages(
        &self,
        messages: Vec<AttributedUsageRecord>,
        group_by: &GroupBy,
    ) -> Result<UsageProjection, String> {
        let mut acc = UsageIndexBuilder::new();
        for message in &messages {
            acc.push(message).map_err(|error| error.to_string())?;
        }
        let acc = acc.finish();
        acc.project_usage(group_by, projection_date())
            .map_err(|error| error.to_string())
    }
}

fn make_workspace_message(
    client: ClientId,
    model_id: &str,
    provider_id: &str,
    session_id: &str,
    cost: f64,
    workspace_key: Option<&str>,
    workspace_label: Option<&str>,
) -> AttributedUsageRecord {
    let mut msg = AttributedUsageRecord::new(
        client,
        model_id,
        provider_id,
        session_id,
        1_735_689_600_000,
        crate::TokenBreakdown {
            input: 10,
            output: 5,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        cost,
    );
    msg.set_workspace(
        workspace_key.map(str::to_string),
        workspace_label.map(str::to_string),
    );
    msg
}

#[allow(clippy::too_many_arguments)]
fn make_message_with_tokens(
    client: ClientId,
    model_id: &str,
    provider_id: &str,
    session_id: &str,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
) -> AttributedUsageRecord {
    AttributedUsageRecord::new(
        client,
        model_id,
        provider_id,
        session_id,
        1_735_689_600_000,
        crate::TokenBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            reasoning,
        },
        0.0,
    )
}

fn daily_usage(date: NaiveDate, tokens: u64, cost: f64) -> DailyUsage {
    DailyUsage {
        date,
        tokens: UsageTokenBreakdown {
            input: tokens,
            ..UsageTokenBreakdown::default()
        },
        cost,
        client_breakdown: BTreeMap::new(),
        message_count: 1,
        turn_count: 1,
    }
}

fn contribution_day(graph: &UsageGraphData, date: NaiveDate) -> &ContributionDay {
    graph
        .weeks
        .iter()
        .flatten()
        .flatten()
        .find(|day| day.date == date)
        .expect("graph must contain the requested visible date")
}

#[test]
fn extreme_calendar_dates_return_typed_projection_errors() {
    let projection = |date| UsageProjection {
        daily: vec![daily_usage(date, 1, 0.0)],
        ..UsageProjection::default()
    };

    let monthly = build_period_usage(&projection(NaiveDate::MAX), PeriodKind::Monthly)
        .expect_err("the month after NaiveDate::MAX is not representable");
    assert_eq!(monthly.field(), "monthly period end date");

    let weekly = build_period_usage(&projection(NaiveDate::MAX), PeriodKind::Weekly)
        .expect_err("the week containing NaiveDate::MAX cannot extend past the date domain");
    assert!(matches!(
        weekly.field(),
        "weekly period start date" | "weekly period end date"
    ));

    let minimum_day = [daily_usage(NaiveDate::MIN, 1, 0.0)];
    let graph = build_contribution_graph_for_today(&minimum_day, NaiveDate::MIN)
        .expect_err("the visible graph window cannot precede NaiveDate::MIN");
    assert_eq!(graph.field(), "contribution graph start date");

    let streak = calculate_streaks_for_today(&minimum_day, NaiveDate::MIN)
        .expect_err("walking backward from NaiveDate::MIN must be fallible");
    assert_eq!(streak.field(), "current streak date traversal");

    let maximum_day = [daily_usage(NaiveDate::MAX, 1, 0.0)];
    let graph = build_contribution_graph_for_today(&maximum_day, NaiveDate::MAX)
        .expect("the graph loop must stop before advancing past NaiveDate::MAX");
    assert_eq!(
        graph
            .weeks
            .last()
            .and_then(|week| week.last())
            .and_then(Option::as_ref)
            .map(|day| day.date),
        Some(NaiveDate::MAX)
    );
}

#[test]
fn contribution_graph_rejects_overflowing_public_daily_usage() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 26).unwrap();
    let mut usage = daily_usage(today, u64::MAX, 0.0);
    usage.tokens.output = 1;

    let error = build_contribution_graph_for_today(&[usage], today).unwrap_err();

    assert_eq!(error.field(), "contribution graph token total");
}

#[test]
fn projection_uses_the_explicit_effective_date() {
    let activity_date = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let tokens = UsageTokenBreakdown {
        input: 10,
        ..UsageTokenBreakdown::default()
    };
    let mut index = FrozenUsageIndex::new();
    index.daily_map.insert(
        activity_date,
        DailyBucket {
            date: activity_date,
            clients: HashMap::from([(
                ClientId::Amp,
                DailyClientBucket {
                    tokens,
                    cost: 0.0,
                    message_count: 1,
                    turn_count: 1,
                    models: HashMap::new(),
                },
            )]),
        },
    );

    let on_activity_date = index.project_usage(&GroupBy::Model, activity_date).unwrap();
    let two_days_later = index
        .project_usage(
            &GroupBy::Model,
            NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
        )
        .unwrap();

    assert_eq!(on_activity_date.current_streak, 1);
    assert_eq!(two_days_later.current_streak, 0);
    let activity_cell = on_activity_date
        .graph
        .weeks
        .last()
        .unwrap()
        .last()
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(activity_cell.date, activity_date);
    assert_eq!(activity_cell.tokens, 10);
    assert_eq!(activity_cell.cost, 0.0);
    assert_eq!(activity_cell.grade, ContributionGrade::Peak);
    assert_eq!(
        two_days_later
            .graph
            .weeks
            .last()
            .unwrap()
            .last()
            .unwrap()
            .as_ref()
            .unwrap()
            .date,
        NaiveDate::from_ymd_opt(2026, 7, 26).unwrap()
    );
}

#[test]
fn contribution_graph_colors_unpriced_activity_by_tokens() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let lower_date = today.pred_opt().unwrap();
    let daily = [
        daily_usage(lower_date, 25, 0.0),
        daily_usage(today, 100, 0.0),
    ];

    let graph = build_contribution_graph_for_today(&daily, today).unwrap();
    let lower = contribution_day(&graph, lower_date);
    let peak = contribution_day(&graph, today);

    assert_eq!(lower.tokens, 25);
    assert_eq!(lower.cost, 0.0);
    assert!(lower.grade > ContributionGrade::Empty);
    assert_eq!(peak.tokens, 100);
    assert_eq!(peak.cost, 0.0);
    assert_eq!(peak.grade, ContributionGrade::Peak);
}

#[test]
fn contribution_graph_ignores_off_window_history_when_assigning_grades() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let days_to_sunday = today.weekday().num_days_from_sunday();
    let start_date = today - chrono::Duration::days(364 + days_to_sunday as i64);
    let off_window_date = start_date.pred_opt().unwrap();
    let visible_daily = [
        daily_usage(today - chrono::Duration::days(2), 10, 0.1),
        daily_usage(today - chrono::Duration::days(1), 100, 1.0),
        daily_usage(today, 1_000, 10.0),
    ];
    let mut daily = visible_daily.to_vec();
    daily.push(daily_usage(off_window_date, 1_000_000_000, 1_000_000.0));

    let baseline = build_contribution_graph_for_today(&visible_daily, today).unwrap();
    let graph = build_contribution_graph_for_today(&daily, today).unwrap();
    for usage in &visible_daily {
        assert_eq!(
            contribution_day(&graph, usage.date).grade,
            contribution_day(&baseline, usage.date).grade
        );
    }
    assert!(graph
        .weeks
        .iter()
        .flatten()
        .flatten()
        .all(|day| day.date != off_window_date));
}

#[test]
fn contribution_graph_cost_does_not_influence_equal_token_grades() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let expensive_date = today.pred_opt().unwrap();
    let free_date = expensive_date.pred_opt().unwrap();
    let daily = [
        daily_usage(free_date.pred_opt().unwrap(), 25, 0.01),
        daily_usage(free_date, 50, 0.0),
        daily_usage(expensive_date, 50, 10_000.0),
        daily_usage(today, 100, 1.0),
    ];

    let graph = build_contribution_graph_for_today(&daily, today).unwrap();
    let expensive = contribution_day(&graph, expensive_date);
    let free = contribution_day(&graph, free_date);

    assert_eq!(expensive.tokens, free.tokens);
    assert_eq!(expensive.cost, 10_000.0);
    assert_eq!(free.cost, 0.0);
    assert_eq!(expensive.grade, ContributionGrade::High);
    assert_eq!(free.grade, ContributionGrade::High);
}

#[test]
fn contribution_graph_log_mad_assigns_all_four_active_grades() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let tokens = [1, 2, 4, 16, 256, 1_024, 8_192, 131_072];
    let expected_grades = [
        ContributionGrade::Low,
        ContributionGrade::Low,
        ContributionGrade::Medium,
        ContributionGrade::Medium,
        ContributionGrade::High,
        ContributionGrade::High,
        ContributionGrade::Peak,
        ContributionGrade::Peak,
    ];
    let daily: Vec<DailyUsage> = tokens
        .iter()
        .enumerate()
        .map(|(index, token_total)| {
            daily_usage(
                today - chrono::Duration::days((tokens.len() - 1 - index) as i64),
                *token_total,
                0.0,
            )
        })
        .collect();

    let graph = build_contribution_graph_for_today(&daily, today).unwrap();
    for (usage, expected) in daily.iter().zip(expected_grades) {
        assert_eq!(contribution_day(&graph, usage.date).grade, expected);
    }
}

#[test]
fn contribution_graph_log_mad_resists_one_large_visible_outlier() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let tokens = [1, 2, 4, 8, 16, 32, 64, 1_u64 << 60];
    let expected_grades = [
        ContributionGrade::Low,
        ContributionGrade::Low,
        ContributionGrade::Medium,
        ContributionGrade::Medium,
        ContributionGrade::High,
        ContributionGrade::High,
        ContributionGrade::Peak,
        ContributionGrade::Peak,
    ];
    let daily: Vec<DailyUsage> = tokens
        .iter()
        .enumerate()
        .map(|(index, token_total)| {
            daily_usage(
                today - chrono::Duration::days((tokens.len() - 1 - index) as i64),
                *token_total,
                0.0,
            )
        })
        .collect();

    let graph = build_contribution_graph_for_today(&daily, today).unwrap();
    for (usage, expected) in daily.iter().zip(expected_grades) {
        assert_eq!(contribution_day(&graph, usage.date).grade, expected);
    }
}

#[test]
fn contribution_graph_handles_zero_mad_degeneracies() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let one_day = [daily_usage(today, 7, 0.0)];
    let one_day_graph = build_contribution_graph_for_today(&one_day, today).unwrap();
    assert_eq!(
        contribution_day(&one_day_graph, today).grade,
        ContributionGrade::Peak
    );

    let all_equal = [
        daily_usage(today - chrono::Duration::days(2), 42, 1.0),
        daily_usage(today - chrono::Duration::days(1), 42, 2.0),
        daily_usage(today, 42, 3.0),
    ];
    let all_equal_graph = build_contribution_graph_for_today(&all_equal, today).unwrap();
    assert!(all_equal.iter().all(|usage| {
        contribution_day(&all_equal_graph, usage.date).grade == ContributionGrade::Peak
    }));

    let fallback_daily = [
        daily_usage(today - chrono::Duration::days(3), 8, 0.0),
        daily_usage(today - chrono::Duration::days(2), 8, 1.0),
        daily_usage(today - chrono::Duration::days(1), 8, 2.0),
        daily_usage(today, 64, 3.0),
    ];
    let fallback_graph = build_contribution_graph_for_today(&fallback_daily, today).unwrap();
    for usage in &fallback_daily[..3] {
        assert_eq!(
            contribution_day(&fallback_graph, usage.date).grade,
            ContributionGrade::High
        );
    }
    assert_eq!(
        contribution_day(&fallback_graph, today).grade,
        ContributionGrade::Peak
    );
}

#[test]
fn contribution_graph_reserves_grade_zero_for_zero_tokens() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let zero_date = today.pred_opt().unwrap();
    let daily = [
        daily_usage(zero_date, 0, 1_000_000.0),
        daily_usage(today, 1, 0.0),
    ];

    let graph = build_contribution_graph_for_today(&daily, today).unwrap();
    let zero = contribution_day(&graph, zero_date);
    let active = contribution_day(&graph, today);

    assert_eq!(zero.tokens, 0);
    assert_eq!(zero.cost, 1_000_000.0);
    assert_eq!(zero.grade, ContributionGrade::Empty);
    assert_eq!(active.tokens, 1);
    assert_eq!(active.cost, 0.0);
    assert_eq!(active.grade, ContributionGrade::Peak);
}

#[test]
fn contribution_graph_retains_visible_token_and_cost_fields() {
    let today = NaiveDate::from_ymd_opt(2026, 7, 24).unwrap();
    let daily = [daily_usage(today, 100, 1.25)];

    let graph = build_contribution_graph_for_today(&daily, today).unwrap();
    let visible = contribution_day(&graph, today);

    assert_eq!(visible.tokens, 100);
    assert_eq!(visible.cost, 1.25);
}

#[test]
fn test_aggregate_messages_model_grouping_uses_finalized_provider_ids() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::OpenCode,
                    "mimo-v2.5-pro",
                    "xiaomi",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    ClientId::OpenCode,
                    "mimo-v2.5-pro",
                    "xiaomi",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::Model,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 1);
    assert_eq!(usage.models[0].model_id.as_ref(), "mimo-v2.5-pro");
    assert_eq!(usage.models[0].display_name.as_ref(), "mimo-v2.5-pro");
    assert_eq!(usage.models[0].provider.as_ref(), "xiaomi");
    assert_eq!(usage.models[0].cost, 3.0);
}

#[test]
fn projection_identity_clones_canonical_arcs_without_copying_payloads() {
    let message = make_workspace_message(
        ClientId::OpenCode,
        "mimo-v2.5-pro",
        "xiaomi",
        "session-1",
        1.0,
        Some("/repo-a"),
        Some("repo-a"),
    );
    let canonical_model = Arc::clone(&message.model_id);
    let canonical_provider = Arc::clone(&message.provider_id);
    let mut builder = UsageIndexBuilder::new();
    builder.push(&message).unwrap();
    let usage = builder
        .finish()
        .project_usage(&GroupBy::WorkspaceModel, projection_date())
        .unwrap();

    let model = &usage.models[0];
    let daily_model = &usage.daily[0].client_breakdown[&ClientId::OpenCode].models[0];
    let hourly_model = &usage.hourly[0].models[0];
    assert!(Arc::ptr_eq(&canonical_model, &model.model_id));
    assert!(Arc::ptr_eq(&model.model_id, &model.display_name));
    assert!(Arc::ptr_eq(&model.model_id, &daily_model.model_id));
    assert!(Arc::ptr_eq(
        &daily_model.model_id,
        &daily_model.display_name
    ));
    assert!(Arc::ptr_eq(&model.model_id, &hourly_model.model_id));
    assert!(Arc::ptr_eq(
        &hourly_model.model_id,
        &hourly_model.display_name
    ));
    assert!(Arc::ptr_eq(&canonical_provider, &model.provider));
    assert!(Arc::ptr_eq(&model.provider, &daily_model.provider));
    assert!(Arc::ptr_eq(&model.provider, &hourly_model.provider));
}

#[test]
fn period_projection_uses_explicit_grouping_identity() {
    fn day(date: NaiveDate, provider: &str) -> DailyUsage {
        DailyUsage {
            date,
            tokens: UsageTokenBreakdown {
                input: 1,
                ..UsageTokenBreakdown::default()
            },
            cost: 1.0,
            client_breakdown: BTreeMap::from([(
                ClientId::Claude,
                DailyClientInfo {
                    tokens: UsageTokenBreakdown {
                        input: 1,
                        ..UsageTokenBreakdown::default()
                    },
                    cost: 1.0,
                    models: vec![DailyModelInfo {
                        provider: Arc::from(provider),
                        model_id: Arc::from("gpt-5.5"),
                        display_name: Arc::from("gpt-5.5"),
                        workspace_key: None,
                        workspace_label: None,
                        tokens: UsageTokenBreakdown {
                            input: 1,
                            ..UsageTokenBreakdown::default()
                        },
                        cost: 1.0,
                        messages: 1,
                    }],
                },
            )]),
            message_count: 1,
            turn_count: 1,
        }
    }

    let daily = [
        day(NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(), "openai"),
        day(NaiveDate::from_ymd_opt(2026, 7, 2).unwrap(), "azure"),
    ];
    let merged = build_period_usage(
        &UsageProjection {
            group_by: GroupBy::Model,
            daily: daily.to_vec(),
            ..UsageProjection::default()
        },
        PeriodKind::Monthly,
    )
    .unwrap();
    assert_eq!(
        merged[0].client_breakdown[&ClientId::Claude].models.len(),
        1
    );
    assert_eq!(
        merged[0].client_breakdown[&ClientId::Claude].models[0].messages,
        2
    );

    let split = build_period_usage(
        &UsageProjection {
            group_by: GroupBy::ClientProviderModel,
            daily: daily.to_vec(),
            ..UsageProjection::default()
        },
        PeriodKind::Monthly,
    )
    .unwrap();
    let models = &split[0].client_breakdown[&ClientId::Claude].models;
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].provider.as_ref(), "azure");
    assert_eq!(models[1].provider.as_ref(), "openai");
}

#[test]
fn test_aggregate_messages_client_provider_model_uses_finalized_provider_ids() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::OpenCode,
                    "mimo-v2.5-pro",
                    "xiaomi",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    ClientId::OpenCode,
                    "mimo-v2.5-pro",
                    "xiaomi",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::ClientProviderModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 1);
    assert_eq!(usage.models[0].provider.as_ref(), "xiaomi");
    assert_eq!(usage.models[0].cost, 3.0);

    let daily_models = &usage.daily[0].client_breakdown[&ClientId::OpenCode].models;
    assert_eq!(daily_models.len(), 1);
    let daily_model = &daily_models[0];
    assert_eq!(daily_model.provider.as_ref(), "xiaomi");
    assert_eq!(daily_model.display_name.as_ref(), "mimo-v2.5-pro");
}

#[test]
fn test_client_provider_model_daily_detail_label_matches_models_tab() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![make_workspace_message(
                ClientId::OpenCode,
                "gpt-5.5",
                "openai",
                "session-1",
                1.0,
                None,
                None,
            )],
            &GroupBy::ClientProviderModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 1);
    assert_eq!(usage.models[0].model_id.as_ref(), "gpt-5.5");
    assert_eq!(usage.models[0].display_name.as_ref(), "gpt-5.5");
    assert_eq!(usage.models[0].provider.as_ref(), "openai");

    let daily_models = &usage.daily[0].client_breakdown[&ClientId::OpenCode].models;
    assert_eq!(daily_models.len(), 1);
    let daily_model = &daily_models[0];
    assert_eq!(daily_model.provider.as_ref(), "openai");
    assert_eq!(daily_model.display_name.as_ref(), "gpt-5.5");
}

#[test]
fn test_client_provider_model_keeps_same_model_distinct_by_provider() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::OpenCode,
                    "gpt-5.5",
                    "openai",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    ClientId::OpenCode,
                    "gpt-5.5",
                    "microsoft",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::ClientProviderModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 2);

    let daily_models = &usage.daily[0].client_breakdown[&ClientId::OpenCode].models;
    assert_eq!(daily_models.len(), 2);
    assert!(daily_models
        .iter()
        .any(|model| model.provider.as_ref() == "openai"));
    assert!(daily_models
        .iter()
        .any(|model| model.provider.as_ref() == "microsoft"));
    assert!(daily_models
        .iter()
        .all(|model| model.display_name.as_ref() == "gpt-5.5"));
}

#[test]
fn test_aggregate_messages_uses_finalized_kimi_provider() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::Claude,
                    "kimi-for-coding",
                    "kimi",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    ClientId::Claude,
                    "kimi-for-coding",
                    "kimi",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::ClientProviderModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 1);
    assert_eq!(usage.models[0].provider.as_ref(), "kimi");
    assert_eq!(usage.models[0].cost, 3.0);
}

#[test]
fn test_aggregate_messages_builds_agent_usage() {
    let loader = TuiUsageHarness;
    let messages = vec![
        AttributedUsageRecord::new_with_agent(
            ClientId::OpenCode,
            "claude-sonnet-4",
            "anthropic",
            "session-1",
            1_735_689_600_000,
            crate::TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.25,
            Some("Builder".to_string()),
        ),
        AttributedUsageRecord::new_with_agent(
            ClientId::RooCode,
            "claude-sonnet-4",
            "anthropic",
            "session-2",
            1_735_689_700_000,
            crate::TokenBreakdown {
                input: 20,
                output: 10,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            2.75,
            Some("Builder".to_string()),
        ),
    ];

    let usage = loader
        .aggregate_messages(messages, &GroupBy::Model)
        .unwrap();

    assert_eq!(usage.agents.len(), 2);
    let opencode = usage
        .agents
        .iter()
        .find(|agent| agent.client == ClientId::OpenCode)
        .unwrap();
    assert_eq!(opencode.agent.as_ref(), "Builder");
    assert_eq!(opencode.message_count, 1);
    assert!((opencode.cost - 1.25).abs() < f64::EPSILON);
    assert_eq!(opencode.tokens.total(), 15);

    let roocode = usage
        .agents
        .iter()
        .find(|agent| agent.client == ClientId::RooCode)
        .unwrap();
    assert_eq!(roocode.agent.as_ref(), "Builder");
    assert_eq!(roocode.message_count, 1);
    assert!((roocode.cost - 2.75).abs() < f64::EPSILON);
    assert_eq!(roocode.tokens.total(), 30);
}

#[test]
fn test_aggregate_messages_orders_model_clients_by_total_tokens() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_message_with_tokens(
                    ClientId::OpenCode,
                    "gpt-5.5",
                    "openai",
                    "session-opencode",
                    10,
                    0,
                    0,
                    0,
                    0,
                ),
                make_message_with_tokens(
                    ClientId::Codex,
                    "gpt-5.5",
                    "openai",
                    "session-codex",
                    30,
                    0,
                    0,
                    0,
                    0,
                ),
                make_message_with_tokens(
                    ClientId::Pi,
                    "gpt-5.5",
                    "openai",
                    "session-pi",
                    100,
                    0,
                    0,
                    0,
                    0,
                ),
            ],
            &GroupBy::Model,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 1);
    assert_eq!(
        usage.models[0].clients,
        [ClientId::Pi, ClientId::Codex, ClientId::OpenCode]
    );
}

#[test]
fn test_aggregate_messages_groups_by_workspace_and_model() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1.25,
                    Some("/repo-a"),
                    Some("repo-a"),
                ),
                make_workspace_message(
                    ClientId::Qwen,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-2",
                    2.75,
                    Some("/repo-a"),
                    Some("repo-a"),
                ),
            ],
            &GroupBy::WorkspaceModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 1);
    assert_eq!(usage.models[0].workspace_key.as_deref(), Some("/repo-a"));
    assert_eq!(usage.models[0].workspace_label.as_deref(), Some("repo-a"));
    assert_eq!(usage.models[0].model_id.as_ref(), "claude-sonnet-4.5");
    assert_eq!(usage.models[0].display_name.as_ref(), "claude-sonnet-4.5");
    assert_eq!(usage.models[0].clients, [ClientId::Claude, ClientId::Qwen]);
    assert_eq!(usage.models[0].session_count, 2);
    assert_eq!(usage.models[0].cost, 4.0);
}

#[test]
fn test_aggregate_messages_workspace_grouping_keeps_unknown_bucket_visible() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1.0,
                    None,
                    None,
                ),
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::WorkspaceModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 1);
    assert_eq!(usage.models[0].workspace_key, None);
    assert_eq!(
        usage.models[0].workspace_label.as_deref(),
        Some(UNKNOWN_WORKSPACE_LABEL)
    );
    assert_eq!(usage.models[0].session_count, 2);
    assert_eq!(usage.models[0].cost, 3.0);
}

#[test]
fn test_aggregate_messages_workspace_grouping_keeps_real_unknown_workspace_separate() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("unknown-workspace"),
                    Some("unknown-workspace"),
                ),
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-2",
                    2.0,
                    None,
                    None,
                ),
            ],
            &GroupBy::WorkspaceModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 2);
    assert!(usage.models.iter().any(|model| {
        model.workspace_key.as_deref() == Some("unknown-workspace")
            && model.workspace_label.as_deref() == Some("unknown-workspace")
            && (model.cost - 1.0).abs() < f64::EPSILON
    }));
    assert!(usage.models.iter().any(|model| {
        model.workspace_key.is_none()
            && model.workspace_label.as_deref() == Some(UNKNOWN_WORKSPACE_LABEL)
            && (model.cost - 2.0).abs() < f64::EPSILON
    }));
}

#[test]
fn test_aggregate_messages_workspace_grouping_splits_daily_models_by_workspace() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("/repo-a"),
                    Some("repo-a"),
                ),
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-2",
                    2.0,
                    Some("/repo-b"),
                    Some("repo-b"),
                ),
            ],
            &GroupBy::WorkspaceModel,
        )
        .unwrap();

    assert_eq!(usage.daily.len(), 1);
    let claude = usage.daily[0]
        .client_breakdown
        .get(&ClientId::Claude)
        .unwrap();
    // The workspace dimension travels in structured fields; display_name
    // and model_id stay the bare canonical model (ADR 0010).
    let daily_identities: Vec<_> = claude
        .models
        .iter()
        .map(|info| {
            (
                info.display_name.clone(),
                info.model_id.clone(),
                info.workspace_key.clone(),
                info.workspace_label.clone(),
            )
        })
        .collect();
    assert_eq!(
        daily_identities,
        vec![
            (
                Arc::from("claude-sonnet-4.5"),
                Arc::from("claude-sonnet-4.5"),
                Some(Arc::from("/repo-a")),
                Some(Arc::from("repo-a")),
            ),
            (
                Arc::from("claude-sonnet-4.5"),
                Arc::from("claude-sonnet-4.5"),
                Some(Arc::from("/repo-b")),
                Some(Arc::from("repo-b")),
            ),
        ]
    );
}

#[test]
fn test_aggregate_messages_workspace_grouping_disambiguates_identical_labels() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("/srv/team-a/demo"),
                    Some("demo"),
                ),
                make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-2",
                    2.0,
                    Some("/srv/team-b/demo"),
                    Some("demo"),
                ),
            ],
            &GroupBy::WorkspaceModel,
        )
        .unwrap();

    assert_eq!(usage.daily.len(), 1);
    let claude = usage.daily[0]
        .client_breakdown
        .get(&ClientId::Claude)
        .unwrap();
    assert_eq!(claude.models.len(), 2);

    let display_names: Vec<_> = claude
        .models
        .iter()
        .map(|info| info.display_name.clone())
        .collect();
    assert_eq!(
        display_names,
        vec![
            Arc::from("claude-sonnet-4.5"),
            Arc::from("claude-sonnet-4.5")
        ]
    );
    let workspace_keys: Vec<_> = claude
        .models
        .iter()
        .map(|info| info.workspace_key.clone())
        .collect();
    assert_eq!(
        workspace_keys,
        vec![
            Some(Arc::from("/srv/team-a/demo")),
            Some(Arc::from("/srv/team-b/demo"))
        ]
    );
}

#[test]
fn daily_model_identity_fields_follow_the_group_by_contract() {
    let loader = TuiUsageHarness;
    for group_by in [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
    ] {
        let usage = loader
            .aggregate_messages(
                vec![make_workspace_message(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("/repo-a"),
                    Some("repo-a"),
                )],
                &group_by,
            )
            .unwrap();

        let models = &usage.daily[0].client_breakdown[&ClientId::Claude].models;
        assert_eq!(models.len(), 1);
        let info = &models[0];
        assert_eq!(info.model_id.as_ref(), "claude-sonnet-4.5");
        assert_eq!(info.display_name.as_ref(), "claude-sonnet-4.5");
        if group_by == GroupBy::WorkspaceModel {
            assert_eq!(info.workspace_key.as_deref(), Some("/repo-a"));
            assert_eq!(info.workspace_label.as_deref(), Some("repo-a"));
        } else {
            assert_eq!(info.workspace_key, None);
            assert_eq!(info.workspace_label, None);
        }

        let hourly = &usage.hourly[0].models;
        assert_eq!(hourly.len(), 1);
        assert_eq!(hourly[0].model_id.as_ref(), "claude-sonnet-4.5");
    }
}

#[test]
fn test_aggregate_messages_workspace_grouping_avoids_separator_key_collisions() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                make_workspace_message(
                    ClientId::Claude,
                    "c",
                    "anthropic",
                    "session-1",
                    1.0,
                    Some("a:b"),
                    Some("workspace-ab"),
                ),
                make_workspace_message(
                    ClientId::Claude,
                    "b:c",
                    "anthropic",
                    "session-2",
                    2.0,
                    Some("a"),
                    Some("workspace-a"),
                ),
            ],
            &GroupBy::WorkspaceModel,
        )
        .unwrap();

    assert_eq!(usage.models.len(), 2);
    assert!(usage.models.iter().any(|model| {
        model.workspace_key.as_deref() == Some("a:b")
            && model.model_id.as_ref() == "c"
            && (model.cost - 1.0).abs() < f64::EPSILON
    }));
    assert!(usage.models.iter().any(|model| {
        model.workspace_key.as_deref() == Some("a")
            && model.model_id.as_ref() == "b:c"
            && (model.cost - 2.0).abs() < f64::EPSILON
    }));

    let claude = usage.daily[0]
        .client_breakdown
        .get(&ClientId::Claude)
        .unwrap();
    assert_eq!(claude.models.len(), 2);
}

#[test]
fn test_aggregate_messages_client_provider_model_splits_providers_in_daily_breakdown() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                AttributedUsageRecord::new(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1_735_689_600_000,
                    crate::TokenBreakdown {
                        input: 10,
                        output: 5,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    1.0,
                ),
                AttributedUsageRecord::new(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "microsoft",
                    "session-2",
                    1_735_689_600_000,
                    crate::TokenBreakdown {
                        input: 20,
                        output: 10,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    2.0,
                ),
            ],
            &GroupBy::ClientProviderModel,
        )
        .unwrap();

    assert_eq!(usage.daily.len(), 1);
    let claude = usage.daily[0]
        .client_breakdown
        .get(&ClientId::Claude)
        .unwrap();
    assert_eq!(claude.models.len(), 2);

    let anthropic_model = claude
        .models
        .iter()
        .find(|model| model.provider.as_ref() == "anthropic")
        .unwrap();
    assert_eq!(anthropic_model.display_name.as_ref(), "claude-sonnet-4.5");
    assert_eq!(anthropic_model.provider.as_ref(), "anthropic");
    assert_eq!(anthropic_model.tokens.total(), 15);
    assert_eq!(anthropic_model.messages, 1);

    let copilot_model = claude
        .models
        .iter()
        .find(|model| model.provider.as_ref() == "microsoft")
        .unwrap();
    assert_eq!(copilot_model.display_name.as_ref(), "claude-sonnet-4.5");
    assert_eq!(copilot_model.provider.as_ref(), "microsoft");
    assert_eq!(copilot_model.tokens.total(), 30);
    assert_eq!(copilot_model.messages, 1);
}

#[test]
fn test_aggregate_messages_keeps_same_model_split_across_clients_in_daily_breakdown() {
    let loader = TuiUsageHarness;
    let usage = loader
        .aggregate_messages(
            vec![
                AttributedUsageRecord::new(
                    ClientId::Claude,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-1",
                    1_735_689_600_000,
                    crate::TokenBreakdown {
                        input: 10,
                        output: 5,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    1.0,
                ),
                AttributedUsageRecord::new(
                    ClientId::Gemini,
                    "claude-sonnet-4.5",
                    "anthropic",
                    "session-2",
                    1_735_689_600_000,
                    crate::TokenBreakdown {
                        input: 20,
                        output: 10,
                        cache_read: 0,
                        cache_write: 0,
                        reasoning: 0,
                    },
                    2.0,
                ),
            ],
            &GroupBy::Model,
        )
        .unwrap();

    assert_eq!(usage.daily.len(), 1);
    assert_eq!(usage.daily[0].client_breakdown.len(), 2);

    let claude = usage.daily[0]
        .client_breakdown
        .get(&ClientId::Claude)
        .unwrap();
    assert_eq!(claude.cost, 1.0);
    assert_eq!(claude.models.len(), 1);
    let claude_model = &claude.models[0];
    assert_eq!(claude_model.display_name.as_ref(), "claude-sonnet-4.5");
    assert_eq!(claude_model.tokens.total(), 15);

    let gemini = usage.daily[0]
        .client_breakdown
        .get(&ClientId::Gemini)
        .unwrap();
    assert_eq!(gemini.cost, 2.0);
    assert_eq!(gemini.models.len(), 1);
    let gemini_model = &gemini.models[0];
    assert_eq!(gemini_model.display_name.as_ref(), "claude-sonnet-4.5");
    assert_eq!(gemini_model.tokens.total(), 30);
}

#[test]
fn test_aggregate_messages_does_not_reinterpret_opencode_agent_variants() {
    let loader = TuiUsageHarness;
    let messages = vec![
        AttributedUsageRecord::new_with_agent(
            ClientId::OpenCode,
            "claude-opus-4.6",
            "anthropic",
            "session-1",
            1_735_689_600_000,
            crate::TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 100,
                cache_write: 20,
                reasoning: 0,
            },
            1.5,
            Some("Sisyphus".to_string()),
        ),
        AttributedUsageRecord::new_with_agent(
            ClientId::OpenCode,
            "claude-opus-4.6",
            "anthropic",
            "session-2",
            1_735_689_700_000,
            crate::TokenBreakdown {
                input: 20,
                output: 10,
                cache_read: 200,
                cache_write: 40,
                reasoning: 0,
            },
            2.5,
            Some("Sisyphus (Ultraworker)".to_string()),
        ),
    ];

    let usage = loader
        .aggregate_messages(messages, &GroupBy::Model)
        .unwrap();

    assert_eq!(usage.agents.len(), 2);
    assert!(usage.agents.iter().any(|agent| {
        agent.agent.as_ref() == "Sisyphus"
            && agent.client == ClientId::OpenCode
            && agent.message_count == 1
    }));
    assert!(usage.agents.iter().any(|agent| {
        agent.agent.as_ref() == "Sisyphus (Ultraworker)"
            && agent.client == ClientId::OpenCode
            && agent.message_count == 1
    }));
}

#[test]
fn test_aggregate_messages_does_not_normalize_opencode_agent_case() {
    let loader = TuiUsageHarness;
    let messages = vec![
        AttributedUsageRecord::new_with_agent(
            ClientId::OpenCode,
            "claude-opus-4.6",
            "anthropic",
            "session-1",
            1_735_689_600_000,
            crate::TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.5,
            Some("Hephaestus".to_string()),
        ),
        AttributedUsageRecord::new_with_agent(
            ClientId::OpenCode,
            "claude-opus-4.6",
            "anthropic",
            "session-2",
            1_735_689_700_000,
            crate::TokenBreakdown {
                input: 20,
                output: 10,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            2.5,
            Some("hephaestus".to_string()),
        ),
    ];

    let usage = loader
        .aggregate_messages(messages, &GroupBy::Model)
        .unwrap();

    assert_eq!(usage.agents.len(), 2);
    assert!(usage
        .agents
        .iter()
        .any(|agent| agent.agent.as_ref() == "Hephaestus"));
    assert!(usage
        .agents
        .iter()
        .any(|agent| agent.agent.as_ref() == "hephaestus"));
}

#[test]
fn test_aggregate_messages_does_not_merge_omo_variants_for_non_opencode_clients() {
    let loader = TuiUsageHarness;
    let messages = vec![
        AttributedUsageRecord::new_with_agent(
            ClientId::Claude,
            "claude-opus-4.6",
            "anthropic",
            "session-1",
            1_735_689_600_000,
            crate::TokenBreakdown {
                input: 10,
                output: 5,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.5,
            Some("Sisyphus".to_string()),
        ),
        AttributedUsageRecord::new_with_agent(
            ClientId::Claude,
            "claude-opus-4.6",
            "anthropic",
            "session-2",
            1_735_689_700_000,
            crate::TokenBreakdown {
                input: 20,
                output: 10,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            2.5,
            Some("Sisyphus (Ultraworker)".to_string()),
        ),
    ];

    let usage = loader
        .aggregate_messages(messages, &GroupBy::Model)
        .unwrap();

    assert_eq!(usage.agents.len(), 2);
    assert!(usage
        .agents
        .iter()
        .any(|agent| agent.agent.as_ref() == "Sisyphus"));
    assert!(usage
        .agents
        .iter()
        .any(|agent| agent.agent.as_ref() == "Sisyphus (Ultraworker)"));
}

fn collision_message(
    client: ClientId,
    provider: &str,
    session: &str,
    model: &str,
    input: i64,
    timestamp: i64,
) -> AttributedUsageRecord {
    AttributedUsageRecord::new(
        client,
        model,
        provider,
        session,
        timestamp,
        crate::TokenBreakdown {
            input,
            ..crate::TokenBreakdown::default()
        },
        input as f64,
    )
}

#[test]
fn top_level_models_preserve_structured_buckets_with_colliding_display_text() {
    let timestamp = 1_735_689_600_000;
    let cases = [
        (
            GroupBy::ClientModel,
            collision_message(ClientId::Codex, "first", "same", "c", 10, timestamp),
            collision_message(ClientId::Amp, "second", "same", "b:c", 20, timestamp),
        ),
        (
            GroupBy::ClientProviderModel,
            collision_message(ClientId::Amp, "b:c", "same", "d", 10, timestamp),
            collision_message(ClientId::Amp, "b", "same", "c:d", 20, timestamp),
        ),
    ];

    for (group_by, first, second) in cases {
        let mut acc = UsageIndexBuilder::new();
        acc.push(&first).unwrap();
        acc.push(&second).unwrap();
        let acc = acc.finish();
        let usage = acc.project_usage(&group_by, projection_date()).unwrap();
        assert_eq!(usage.models.len(), 2);
        assert_eq!(usage.models[0].tokens.total(), 20);
        assert_eq!(usage.models[0].cost, 20.0);
        assert_eq!(usage.models[1].tokens.total(), 10);
        assert_eq!(usage.models[1].cost, 10.0);
    }
}

#[test]
fn daily_and_hourly_models_preserve_collision_free_structured_identity() {
    let timestamp = 1_735_689_600_000;
    let first = collision_message(ClientId::Amp, "b:c", "same", "d", 10, timestamp);
    let second = collision_message(ClientId::Amp, "b", "same", "c:d", 20, timestamp);
    let mut acc = UsageIndexBuilder::new();
    acc.push(&first).unwrap();
    acc.push(&second).unwrap();
    let acc = acc.finish();
    let usage = acc
        .project_usage(&GroupBy::ClientProviderModel, projection_date())
        .unwrap();

    let daily = &usage.daily[0].client_breakdown[&ClientId::Amp].models;
    assert_eq!(daily.len(), 2);
    let first_daily = daily
        .iter()
        .find(|model| model.provider.as_ref() == "b:c")
        .unwrap();
    assert_eq!(first_daily.provider.as_ref(), "b:c");
    assert_eq!(first_daily.model_id.as_ref(), "d");
    assert_eq!(first_daily.display_name.as_ref(), "d");
    assert_eq!(first_daily.tokens.total(), 10);
    assert_eq!(first_daily.cost, 10.0);
    assert_eq!(first_daily.messages, 1);
    let second_daily = daily
        .iter()
        .find(|model| model.provider.as_ref() == "b")
        .unwrap();
    assert_eq!(second_daily.provider.as_ref(), "b");
    assert_eq!(second_daily.model_id.as_ref(), "c:d");
    assert_eq!(second_daily.display_name.as_ref(), "c:d");
    assert_eq!(second_daily.tokens.total(), 20);
    assert_eq!(second_daily.cost, 20.0);
    assert_eq!(second_daily.messages, 1);

    let hourly = &usage.hourly[0].models;
    assert_eq!(hourly.len(), 2);
    let first_hourly = hourly
        .iter()
        .find(|model| model.provider.as_ref() == "b:c")
        .unwrap();
    assert_eq!(first_hourly.provider.as_ref(), "b:c");
    assert_eq!(first_hourly.model_id.as_ref(), "d");
    assert_eq!(first_hourly.display_name.as_ref(), "d");
    assert_eq!(first_hourly.tokens.total(), 10);
    assert_eq!(first_hourly.cost, 10.0);
    let second_hourly = hourly
        .iter()
        .find(|model| model.provider.as_ref() == "b")
        .unwrap();
    assert_eq!(second_hourly.provider.as_ref(), "b");
    assert_eq!(second_hourly.model_id.as_ref(), "c:d");
    assert_eq!(second_hourly.display_name.as_ref(), "c:d");
    assert_eq!(second_hourly.tokens.total(), 20);
    assert_eq!(second_hourly.cost, 20.0);
}

#[test]
fn hourly_client_identity_order_is_deterministic() {
    let timestamp = 1_735_689_600_000;
    let mut acc = UsageIndexBuilder::new();
    acc.push(&collision_message(
        ClientId::Zed,
        "provider",
        "session-z",
        "model",
        10,
        timestamp,
    ))
    .unwrap();
    acc.push(&collision_message(
        ClientId::Amp,
        "provider",
        "session-a",
        "model",
        20,
        timestamp,
    ))
    .unwrap();

    let acc = acc.finish();
    let usage = acc
        .project_usage(&GroupBy::Model, projection_date())
        .unwrap();

    assert_eq!(
        usage.hourly[0]
            .clients
            .iter()
            .map(|client| client.as_str())
            .collect::<Vec<_>>(),
        ["amp", "zed"]
    );
}

#[test]
fn workspace_maps_tag_unknown_and_known_keys_separately() {
    let timestamp = 1_735_689_600_000;
    let mut unknown = collision_message(
        ClientId::Codex,
        "provider",
        "session",
        "model",
        10,
        timestamp,
    );
    unknown.workspace_key = None;
    unknown.workspace_label = None;
    let mut known = collision_message(
        ClientId::Codex,
        "provider",
        "session",
        "model",
        20,
        timestamp,
    );
    known.workspace_key = Some(Arc::from(""));
    known.workspace_label = Some(Arc::from("Empty workspace key"));

    let mut acc = UsageIndexBuilder::new();
    acc.push(&unknown).unwrap();
    acc.push(&known).unwrap();
    let acc = acc.finish();
    let usage = acc
        .project_usage(&GroupBy::WorkspaceModel, projection_date())
        .unwrap();

    assert_eq!(usage.models.len(), 2);
    let daily_models = &usage.daily[0].client_breakdown[&ClientId::Codex].models;
    assert_eq!(daily_models.len(), 2);
    assert!(daily_models
        .iter()
        .any(|model| model.workspace_key.is_none()));
    assert!(daily_models
        .iter()
        .any(|model| model.workspace_key.as_deref() == Some("")));
}

#[test]
fn structured_session_and_agent_instance_identities_do_not_alias_delimiters() {
    let timestamp = 1_735_689_600_000;
    let model_messages = [
        collision_message(ClientId::Codex, "provider", "c", "model", 10, timestamp),
        collision_message(ClientId::Amp, "provider", "b:c", "model", 20, timestamp),
    ];
    let mut model_acc = UsageIndexBuilder::new();
    for message in &model_messages {
        model_acc.push(message).unwrap();
    }
    let model_acc = model_acc.finish();
    assert_eq!(
        model_acc
            .project_usage(&GroupBy::Model, projection_date())
            .unwrap()
            .models[0]
            .session_count,
        2
    );

    let mut explicit = AttributedUsageRecord::new_with_agent(
        ClientId::Amp,
        "model",
        "provider",
        "session",
        timestamp,
        crate::TokenBreakdown::default(),
        0.0,
        Some("builder".to_string()),
    );
    explicit.set_agent_instance(Some("a:b:c".to_string()));
    let derived_left = AttributedUsageRecord::new_with_agent(
        ClientId::Amp,
        "model",
        "provider",
        "c",
        timestamp,
        crate::TokenBreakdown::default(),
        0.0,
        Some("builder".to_string()),
    );
    let derived_right = AttributedUsageRecord::new_with_agent(
        ClientId::Amp,
        "model",
        "provider",
        "b:c",
        timestamp,
        crate::TokenBreakdown::default(),
        0.0,
        Some("builder".to_string()),
    );
    let mut agent_acc = UsageIndexBuilder::new();
    for message in [&explicit, &derived_left, &derived_right] {
        agent_acc.push(message).unwrap();
    }
    let agent_acc = agent_acc.finish();
    assert_eq!(
        agent_acc
            .project_usage(&GroupBy::Model, projection_date())
            .unwrap()
            .agents[0]
            .instance_count,
        3
    );
}

// ---- group-by re-projection (issue #161) ----

#[allow(clippy::too_many_arguments)]
fn reprojection_message(
    client: ClientId,
    model: &str,
    provider: &str,
    session: &str,
    timestamp: i64,
    input: i64,
    output: i64,
    cost: f64,
    workspace_key: Option<&str>,
    workspace_label: Option<&str>,
    is_turn_start: bool,
    agent: Option<&str>,
) -> AttributedUsageRecord {
    let mut msg = AttributedUsageRecord::new_with_agent(
        client,
        model,
        provider,
        session,
        timestamp,
        crate::TokenBreakdown {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        cost,
        agent.map(str::to_string),
    );
    msg.set_workspace(
        workspace_key.map(str::to_string),
        workspace_label.map(str::to_string),
    );
    msg.is_turn_start = is_turn_start;
    msg
}

/// Noon-based UTC timestamps keep day and hour buckets stable across test
/// host time zones.
fn reprojection_ts(day_offset: i64, hour: i64) -> i64 {
    (1_749_945_600 + day_offset * 86_400 + hour * 3_600) * 1_000
}

/// One corpus exercising every dimension a grouping can re-fold: shared
/// models across clients, one client:model pair across two providers,
/// workspaces with/without labels and an unknown workspace, two sessions
/// per client, two days, two hours, turn starts, and an agent.
fn reprojection_corpus() -> Vec<AttributedUsageRecord> {
    vec![
        reprojection_message(
            ClientId::Claude,
            "gpt-5.5",
            "openai",
            "s1",
            reprojection_ts(0, 12),
            100,
            50,
            0.1,
            Some("/repo-a"),
            Some("repo-a"),
            true,
            Some("builder"),
        ),
        reprojection_message(
            ClientId::Codex,
            "gpt-5.5",
            "openai",
            "s2",
            reprojection_ts(0, 12),
            200,
            60,
            0.2,
            Some("/repo-a"),
            Some("repo-a"),
            false,
            None,
        ),
        reprojection_message(
            ClientId::Claude,
            "gpt-5.5",
            "azure",
            "s1",
            reprojection_ts(0, 13),
            300,
            70,
            0.3,
            Some("/repo-b"),
            None,
            false,
            None,
        ),
        reprojection_message(
            ClientId::Claude,
            "claude-sonnet-4.5",
            "anthropic",
            "s3",
            reprojection_ts(1, 12),
            400,
            80,
            0.4,
            None,
            None,
            true,
            None,
        ),
        reprojection_message(
            ClientId::Qwen,
            "gpt-5.5",
            "openai",
            "s4",
            reprojection_ts(1, 13),
            500,
            90,
            0.5,
            Some("/repo-a"),
            Some("repo-a"),
            false,
            None,
        ),
    ]
}

fn reprojection_index() -> FrozenUsageIndex {
    let mut acc = UsageIndexBuilder::new();
    for message in reprojection_corpus() {
        acc.push(&message).unwrap();
    }
    acc.finish()
}

fn container_capacities(index: &FrozenUsageIndex) -> Vec<(usize, usize)> {
    let mut capacities = vec![
        (
            index.usage_totals_by_client.len(),
            index.usage_totals_by_client.capacity(),
        ),
        (index.model_map.len(), index.model_map.capacity()),
        (index.agent_map.len(), index.agent_map.capacity()),
        (index.daily_map.len(), index.daily_map.capacity()),
        (index.hourly_map.len(), index.hourly_map.capacity()),
    ];
    for agent in index.agent_map.values() {
        if let IdentitySet::Many(instances) = &agent.instances {
            capacities.push((instances.len(), instances.capacity()));
        }
    }
    for daily in index.daily_map.values() {
        capacities.push((daily.clients.len(), daily.clients.capacity()));
        capacities.extend(
            daily
                .clients
                .values()
                .map(|client| (client.models.len(), client.models.capacity())),
        );
    }
    for hourly in index.hourly_map.values() {
        capacities.push((hourly.clients.len(), hourly.clients.capacity()));
        capacities.extend(
            hourly
                .clients
                .values()
                .map(|client| (client.models.len(), client.models.capacity())),
        );
    }
    capacities
}

fn reserve_frozen_index_storage(index: &mut FrozenUsageIndex, additional: usize) {
    index.usage_totals_by_client.reserve(additional);
    index.model_map.reserve(additional);
    index.agent_map.reserve(additional);
    index.daily_map.reserve(additional);
    index.hourly_map.reserve(additional);
    for agent in index.agent_map.values_mut() {
        agent
            .instances
            .insert(AgentInstanceKey::Explicit(crate::records::intern::intern(
                "capacity-test-instance",
            )));
        if let IdentitySet::Many(instances) = &mut agent.instances {
            instances.reserve(additional);
        }
    }
    for daily in index.daily_map.values_mut() {
        daily.clients.reserve(additional);
        for client in daily.clients.values_mut() {
            client.models.reserve(additional);
        }
    }
    for hourly in index.hourly_map.values_mut() {
        hourly.clients.reserve(additional);
        for client in hourly.clients.values_mut() {
            client.models.reserve(additional);
        }
    }
}

fn assert_tokens_eq(left: &UsageTokenBreakdown, right: &UsageTokenBreakdown) {
    assert_eq!(left.input, right.input);
    assert_eq!(left.output, right.output);
    assert_eq!(left.cache_read, right.cache_read);
    assert_eq!(left.cache_write, right.cache_write);
    assert_eq!(left.reasoning, right.reasoning);
}

fn assert_agents_eq(left: &[AgentEntry], right: &[AgentEntry]) {
    assert_eq!(left.len(), right.len());
    for (left, right) in left.iter().zip(right) {
        assert_eq!(left.agent, right.agent);
        assert_eq!(left.client, right.client);
        assert_tokens_eq(&left.tokens, &right.tokens);
        assert_eq!(left.cost.to_bits(), right.cost.to_bits());
        assert_eq!(left.message_count, right.message_count);
        assert_eq!(left.instance_count, right.instance_count);
    }
}

fn assert_graph_eq(left: &UsageGraphData, right: &UsageGraphData) {
    assert_eq!(left.weeks.len(), right.weeks.len());
    for (left_week, right_week) in left.weeks.iter().zip(&right.weeks) {
        assert_eq!(left_week.len(), right_week.len());
        for (left_day, right_day) in left_week.iter().zip(right_week) {
            match (left_day, right_day) {
                (Some(left), Some(right)) => {
                    assert_eq!(left.date, right.date);
                    assert_eq!(left.tokens, right.tokens);
                    assert_eq!(left.cost.to_bits(), right.cost.to_bits());
                    assert_eq!(left.grade, right.grade);
                }
                (None, None) => {}
                _ => panic!("graph day presence mismatch"),
            }
        }
    }
}

fn assert_usage_data_eq(left: &UsageProjection, right: &UsageProjection) {
    assert_eq!(left.group_by, right.group_by);
    assert_eq!(left.total_tokens, right.total_tokens);
    assert_eq!(left.total_cost.to_bits(), right.total_cost.to_bits());
    assert_eq!(left.current_streak, right.current_streak);
    assert_eq!(left.longest_streak, right.longest_streak);

    assert_eq!(left.models.len(), right.models.len());
    for (left, right) in left.models.iter().zip(&right.models) {
        assert_eq!(left.model_id, right.model_id);
        assert_eq!(left.display_name, right.display_name);
        assert_eq!(left.provider, right.provider);
        assert_eq!(left.clients, right.clients);
        assert_eq!(left.workspace_key, right.workspace_key);
        assert_eq!(left.workspace_label, right.workspace_label);
        assert_tokens_eq(&left.tokens, &right.tokens);
        assert_eq!(left.cost.to_bits(), right.cost.to_bits());
        assert_eq!(left.session_count, right.session_count);
    }

    assert_agents_eq(&left.agents, &right.agents);

    assert_eq!(left.daily.len(), right.daily.len());
    for (left, right) in left.daily.iter().zip(&right.daily) {
        assert_eq!(left.date, right.date);
        assert_tokens_eq(&left.tokens, &right.tokens);
        assert_eq!(left.cost.to_bits(), right.cost.to_bits());
        assert_eq!(left.message_count, right.message_count);
        assert_eq!(left.turn_count, right.turn_count);
        assert_eq!(left.client_breakdown.len(), right.client_breakdown.len());
        for ((left_client, left_info), (right_client, right_info)) in
            left.client_breakdown.iter().zip(&right.client_breakdown)
        {
            assert_eq!(left_client, right_client);
            assert_tokens_eq(&left_info.tokens, &right_info.tokens);
            assert_eq!(left_info.cost.to_bits(), right_info.cost.to_bits());
            assert_eq!(left_info.models.len(), right_info.models.len());
            for (left_model, right_model) in left_info.models.iter().zip(&right_info.models) {
                assert_eq!(left_model.provider, right_model.provider);
                assert_eq!(left_model.model_id, right_model.model_id);
                assert_eq!(left_model.display_name, right_model.display_name);
                assert_eq!(left_model.workspace_key, right_model.workspace_key);
                assert_eq!(left_model.workspace_label, right_model.workspace_label);
                assert_tokens_eq(&left_model.tokens, &right_model.tokens);
                assert_eq!(left_model.cost.to_bits(), right_model.cost.to_bits());
                assert_eq!(left_model.messages, right_model.messages);
            }
        }
    }

    assert_eq!(left.hourly.len(), right.hourly.len());
    for (left, right) in left.hourly.iter().zip(&right.hourly) {
        assert_eq!(left.datetime, right.datetime);
        assert_tokens_eq(&left.tokens, &right.tokens);
        assert_eq!(left.cost.to_bits(), right.cost.to_bits());
        assert_eq!(left.clients, right.clients);
        assert_eq!(left.message_count, right.message_count);
        assert_eq!(left.turn_count, right.turn_count);
        assert_eq!(left.models.len(), right.models.len());
        for (left_model, right_model) in left.models.iter().zip(&right.models) {
            assert_eq!(left_model.provider, right_model.provider);
            assert_eq!(left_model.model_id, right_model.model_id);
            assert_eq!(left_model.display_name, right_model.display_name);
            assert_tokens_eq(&left_model.tokens, &right_model.tokens);
            assert_eq!(left_model.cost.to_bits(), right_model.cost.to_bits());
        }
    }

    assert_graph_eq(&left.graph, &right.graph);
}

#[test]
fn reprojection_is_repeatable_and_deterministic() {
    let acc = reprojection_index();
    for group_by in [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
    ] {
        let first = acc.project_usage(&group_by, projection_date()).unwrap();
        let second = acc.project_usage(&group_by, projection_date()).unwrap();
        assert_usage_data_eq(&first, &second);
    }
}

#[test]
fn totals_use_canonical_full_and_client_scoped_folds() {
    let acc = reprojection_index();

    let full = acc
        .project_usage(&GroupBy::Model, projection_date())
        .unwrap();
    assert_eq!(full.total_tokens, 1_850);
    assert_eq!(full.total_cost.to_bits(), 1.5_f64.to_bits());

    let all_selected = acc
        .project_usage_for_clients(
            &GroupBy::Model,
            &HashSet::from([ClientId::Claude, ClientId::Codex, ClientId::Qwen]),
            projection_date(),
        )
        .unwrap();
    assert_eq!(all_selected.total_tokens, full.total_tokens);
    assert_eq!(all_selected.total_cost.to_bits(), full.total_cost.to_bits());

    let claude_and_codex = acc
        .project_usage_for_clients(
            &GroupBy::Model,
            &HashSet::from([ClientId::Claude, ClientId::Codex]),
            projection_date(),
        )
        .unwrap();
    assert_eq!(claude_and_codex.total_tokens, 1_260);
    assert_eq!(claude_and_codex.total_cost.to_bits(), 1.0_f64.to_bits());

    let qwen = acc
        .project_usage_for_clients(
            &GroupBy::Model,
            &HashSet::from([ClientId::Qwen]),
            projection_date(),
        )
        .unwrap();
    assert_eq!(qwen.total_tokens, 590);
    assert_eq!(qwen.total_cost.to_bits(), 0.5_f64.to_bits());
}

#[test]
fn model_only_projection_matches_complete_projection_models_and_totals() {
    let acc = reprojection_index();
    let selected = HashSet::from([ClientId::Claude, ClientId::Codex]);

    for group_by in [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
    ] {
        let complete = acc.project_usage(&group_by, projection_date()).unwrap();
        let models = acc.project_models(&group_by).unwrap();

        assert_eq!(models.models, complete.models);
        assert_eq!(models.total_tokens, complete.total_tokens);
        assert_eq!(models.total_cost.to_bits(), complete.total_cost.to_bits());

        let complete = acc
            .project_usage_for_clients(&group_by, &selected, projection_date())
            .unwrap();
        let models = acc
            .project_models_for_clients(&group_by, &selected)
            .unwrap();

        assert_eq!(models.models, complete.models);
        assert_eq!(models.total_tokens, complete.total_tokens);
        assert_eq!(models.total_cost.to_bits(), complete.total_cost.to_bits());
    }
}

#[test]
fn frozen_index_wire_round_trip_preserves_full_and_subset_totals() {
    let acc = reprojection_index();
    let encoded = serde_json::to_vec(&acc).expect("serialize frozen usage index");
    let restored = serde_json::from_slice::<FrozenUsageIndexWire>(&encoded)
        .expect("deserialize frozen usage-index wire")
        .into_index();
    restored
        .validate(
            &ClientUniverse::new([ClientId::Claude, ClientId::Codex, ClientId::Qwen]).unwrap(),
        )
        .expect("restored usage index remains semantically valid");

    for group_by in [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
    ] {
        assert_usage_data_eq(
            &acc.project_usage(&group_by, projection_date()).unwrap(),
            &restored
                .project_usage(&group_by, projection_date())
                .unwrap(),
        );
    }

    let selected = HashSet::from([ClientId::Claude, ClientId::Codex]);
    assert_usage_data_eq(
        &acc.project_usage_for_clients(&GroupBy::Model, &selected, projection_date())
            .unwrap(),
        &restored
            .project_usage_for_clients(&GroupBy::Model, &selected, projection_date())
            .unwrap(),
    );
}

#[test]
fn finish_compacts_every_persisted_hash_container() {
    const EXCESS_CAPACITY: usize = 4_096;

    let mut builder = UsageIndexBuilder::new();
    for message in reprojection_corpus() {
        builder.push(&message).unwrap();
    }
    reserve_frozen_index_storage(&mut builder.index, EXCESS_CAPACITY);
    let before = container_capacities(&builder.index);
    assert!(!before.is_empty());
    assert!(before
        .iter()
        .all(|(len, capacity)| *capacity >= len + EXCESS_CAPACITY));

    let compact = builder.finish();
    let after = container_capacities(&compact);
    assert_eq!(after.len(), before.len());
    assert!(after
        .iter()
        .all(|(len, capacity)| *capacity >= *len && *capacity < EXCESS_CAPACITY));
}

#[test]
fn cache_deserialization_compacts_every_persisted_hash_container() {
    const EXCESS_CAPACITY: usize = 4_096;

    let mut builder = UsageIndexBuilder::new();
    for message in reprojection_corpus() {
        builder.push(&message).unwrap();
    }
    reserve_frozen_index_storage(&mut builder.index, EXCESS_CAPACITY);
    let before = container_capacities(&builder.index);
    assert!(!before.is_empty());
    assert!(before
        .iter()
        .all(|(len, capacity)| *capacity >= len + EXCESS_CAPACITY));

    let encoded =
        bincode::serialize(&builder.index).expect("serialize oversized frozen usage index");
    let mut restored = bincode::deserialize::<FrozenUsageIndexWire>(&encoded)
        .expect("deserialize frozen usage-index wire")
        .into_index();
    let after = container_capacities(&restored);
    assert_eq!(after.len(), before.len());
    assert!(after
        .iter()
        .all(|(len, capacity)| *capacity >= *len && *capacity < EXCESS_CAPACITY));

    restored.shrink_to_fit();
    assert_eq!(container_capacities(&restored), after);
}

#[test]
fn cache_deserialization_restores_canonical_arc_sharing() {
    let index = reprojection_index();
    let encoded = bincode::serialize(&index).expect("serialize frozen usage index");
    drop(index);

    let restored = bincode::deserialize::<FrozenUsageIndexWire>(&encoded)
        .expect("deserialize frozen usage-index wire")
        .into_index();
    let (model_key, model_bucket) = restored
        .model_map
        .iter()
        .find(|(key, _)| {
            key.client == ClientId::Claude
                && key.provider.as_ref() == "openai"
                && key.session.as_ref() == "s1"
                && key.model.as_ref() == "gpt-5.5"
        })
        .expect("canonical model bucket");
    let (daily_key, daily_bucket) = restored
        .daily_map
        .values()
        .flat_map(|day| day.clients.get(&ClientId::Claude))
        .flat_map(|client| client.models.iter())
        .find(|(key, _)| {
            key.provider.as_ref() == "openai"
                && key.session.as_ref() == "s1"
                && key.model.as_ref() == "gpt-5.5"
        })
        .expect("daily model bucket");
    let hourly_key = restored
        .hourly_map
        .values()
        .flat_map(|hour| hour.clients.get(&ClientId::Claude))
        .flat_map(|client| client.models.keys())
        .find(|key| key.provider.as_ref() == "openai" && key.model.as_ref() == "gpt-5.5")
        .expect("hourly model bucket");

    assert!(Arc::ptr_eq(&model_key.provider, &daily_key.provider));
    assert!(Arc::ptr_eq(&model_key.provider, &hourly_key.provider));
    assert!(Arc::ptr_eq(&model_key.session, &daily_key.session));
    assert!(Arc::ptr_eq(&model_key.model, &daily_key.model));
    assert!(Arc::ptr_eq(&model_key.model, &hourly_key.model));
    assert!(Arc::ptr_eq(
        &model_bucket.workspace_label,
        &daily_bucket.workspace_label
    ));
    let (WorkspaceKey::Known(model_workspace), WorkspaceKey::Known(daily_workspace)) =
        (&model_key.workspace, &daily_key.workspace)
    else {
        panic!("test model must retain a known workspace");
    };
    assert!(Arc::ptr_eq(model_workspace, daily_workspace));
}

#[test]
fn frozen_index_serialization_excludes_builder_sequence_state() {
    let mut builder = UsageIndexBuilder::new();
    builder.push(&reprojection_corpus()[0]).unwrap();
    assert_eq!(builder.next_sequence, 1);

    let encoded = serde_json::to_value(builder.finish()).expect("serialize frozen usage index");
    let object = encoded.as_object().expect("usage index must be an object");
    assert!(!object.contains_key("next_sequence"));
    assert!(!object.contains_key("nextSequence"));
}

#[test]
fn frozen_index_validation_accepts_coherent_materialized_totals() {
    let index = reprojection_index();
    let universe =
        ClientUniverse::new([ClientId::Claude, ClientId::Codex, ClientId::Qwen]).unwrap();

    assert_eq!(index.validate(&universe), Ok(()));
}

#[test]
fn frozen_index_validation_rejects_an_indexed_client_outside_the_universe() {
    let index = reprojection_index();
    let universe = ClientUniverse::new([ClientId::Claude, ClientId::Codex]).unwrap();

    assert_eq!(
        index.validate(&universe),
        Err(UsageIndexValidationError::IndexedClientOutsideUniverse {
            index: "usage_totals_by_client",
            client: ClientId::Qwen,
        })
    );
}

#[test]
fn frozen_index_validation_rejects_disagreeing_model_totals() {
    let mut index = reprojection_index();
    index
        .model_map
        .values_mut()
        .next()
        .expect("corpus has a model bucket")
        .tokens
        .input += 1;
    let universe =
        ClientUniverse::new([ClientId::Claude, ClientId::Codex, ClientId::Qwen]).unwrap();

    assert!(matches!(
        index.validate(&universe),
        Err(UsageIndexValidationError::TokenTotalsMismatch {
            index: "model_map",
            ..
        })
    ));
}

#[test]
fn frozen_index_validation_rejects_every_invalid_persisted_cost() {
    #[derive(Clone, Copy)]
    enum CostLocation {
        Usage,
        Model,
        Agent,
        Daily,
        DailyModel,
        Hourly,
        HourlyModel,
    }

    let make_index = || {
        let record = AttributedUsageRecord::new_with_agent(
            ClientId::OpenCode,
            "claude-opus-4.6",
            "anthropic",
            "invalid-cost-session",
            1_735_689_600_000,
            crate::TokenBreakdown {
                input: 1,
                ..crate::TokenBreakdown::default()
            },
            0.25,
            Some("Sisyphus".to_string()),
        );
        let mut builder = UsageIndexBuilder::new();
        builder.push(&record).unwrap();
        builder.finish()
    };
    let universe = ClientUniverse::new([ClientId::OpenCode]).unwrap();

    for (invalid_cost, expected_kind) in [
        (f64::NAN, InvalidCostKind::NonFinite),
        (f64::INFINITY, InvalidCostKind::NonFinite),
        (-0.01, InvalidCostKind::Negative),
    ] {
        for (location, expected_index) in [
            (CostLocation::Usage, "usage_totals_by_client"),
            (CostLocation::Model, "model_map"),
            (CostLocation::Agent, "agent_map"),
            (CostLocation::Daily, "daily_map"),
            (CostLocation::DailyModel, "daily_map.models"),
            (CostLocation::Hourly, "hourly_map"),
            (CostLocation::HourlyModel, "hourly_map.models"),
        ] {
            let mut index = make_index();
            match location {
                CostLocation::Usage => {
                    index
                        .usage_totals_by_client
                        .values_mut()
                        .next()
                        .unwrap()
                        .cost = invalid_cost;
                }
                CostLocation::Model => {
                    index.model_map.values_mut().next().unwrap().cost = invalid_cost;
                }
                CostLocation::Agent => {
                    index.agent_map.values_mut().next().unwrap().cost = invalid_cost;
                }
                CostLocation::Daily => {
                    index
                        .daily_map
                        .values_mut()
                        .next()
                        .unwrap()
                        .clients
                        .values_mut()
                        .next()
                        .unwrap()
                        .cost = invalid_cost;
                }
                CostLocation::DailyModel => {
                    index
                        .daily_map
                        .values_mut()
                        .next()
                        .unwrap()
                        .clients
                        .values_mut()
                        .next()
                        .unwrap()
                        .models
                        .values_mut()
                        .next()
                        .unwrap()
                        .cost = invalid_cost;
                }
                CostLocation::Hourly => {
                    index
                        .hourly_map
                        .values_mut()
                        .next()
                        .unwrap()
                        .clients
                        .values_mut()
                        .next()
                        .unwrap()
                        .cost = invalid_cost;
                }
                CostLocation::HourlyModel => {
                    index
                        .hourly_map
                        .values_mut()
                        .next()
                        .unwrap()
                        .clients
                        .values_mut()
                        .next()
                        .unwrap()
                        .models
                        .values_mut()
                        .next()
                        .unwrap()
                        .cost = invalid_cost;
                }
            }

            assert_eq!(
                index.validate(&universe),
                Err(UsageIndexValidationError::InvalidCost {
                    index: expected_index,
                    client: ClientId::OpenCode,
                    kind: expected_kind,
                })
            );
        }
    }
}

#[test]
fn generation_validation_rejects_projection_overflow_without_mutating_the_index() {
    let records = [
        collision_message(
            ClientId::Claude,
            "openai",
            "claude-session",
            "claude-model",
            1,
            1_735_689_600_000,
        ),
        collision_message(
            ClientId::Codex,
            "openai",
            "codex-session",
            "codex-model",
            1,
            1_735_689_600_000,
        ),
    ];
    let mut builder = UsageIndexBuilder::new();
    for record in &records {
        builder.push(record).unwrap();
    }
    let mut index = builder.finish();
    for totals in index.usage_totals_by_client.values_mut() {
        totals.tokens = UsageTokenBreakdown {
            input: u64::MAX,
            ..UsageTokenBreakdown::default()
        };
    }
    for model in index.model_map.values_mut() {
        model.tokens = UsageTokenBreakdown {
            input: u64::MAX,
            ..UsageTokenBreakdown::default()
        };
        model.contribution_tokens = u64::MAX;
    }

    let universe = ClientUniverse::new([ClientId::Claude, ClientId::Codex]).unwrap();
    let before = bincode::serialize(&index).unwrap();
    assert_eq!(
        index.validate(&universe),
        Err(UsageIndexValidationError::ProjectionOverflow {
            group_by: GroupBy::Model,
            field: "total token breakdown",
        })
    );
    assert_eq!(bincode::serialize(&index).unwrap(), before);

    let acquisition = crate::AcquisitionConfig::new(
        std::path::PathBuf::from("/tmp/tokenx-projection-overflow"),
        crate::DateRange::none(),
        universe.clone(),
        crate::scanner::ScannerSettings::default(),
        crate::CalendarContext::explicit("UTC").unwrap(),
        crate::PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
    )
    .unwrap();
    let error = crate::Generation::new(
        acquisition,
        crate::SourceFingerprint::from_bytes([0; 32]),
        index,
        Vec::new(),
        crate::InputFootprint::from_client_bytes([(ClientId::Claude, 0), (ClientId::Codex, 0)])
            .unwrap(),
        crate::input_health::HealthSummary::default(),
        Vec::new(),
    )
    .unwrap_err();
    assert_eq!(
        error,
        crate::GenerationError::InvalidUsageIndex(UsageIndexValidationError::ProjectionOverflow {
            group_by: GroupBy::Model,
            field: "total token breakdown",
        })
    );
}

#[test]
fn agent_sort_rejects_overflowing_token_totals() {
    let record = AttributedUsageRecord::new_with_agent(
        ClientId::OpenCode,
        "claude-opus-4.6",
        "anthropic",
        "agent-overflow-session",
        1_735_689_600_000,
        crate::TokenBreakdown {
            input: 1,
            ..crate::TokenBreakdown::default()
        },
        0.0,
        Some("Sisyphus".to_string()),
    );
    let mut builder = UsageIndexBuilder::new();
    builder.push(&record).unwrap();
    let mut index = builder.finish();
    let agent = index
        .agent_map
        .values_mut()
        .next()
        .expect("fixture creates one agent");
    agent.tokens.input = u64::MAX;
    agent.tokens.output = 1;

    let error = index
        .project_usage(&GroupBy::Model, projection_date())
        .unwrap_err();

    assert_eq!(error.field(), "agent token total");
}

#[test]
fn generation_deserialization_rejects_an_overflowing_agent_index_before_projection() {
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct InvalidGenerationWire {
        acquisition: crate::AcquisitionConfig,
        source_fingerprint: crate::SourceFingerprint,
        usage_index: FrozenUsageIndex,
        sessions: Vec<crate::SessionUsage>,
        input_footprint: crate::InputFootprint,
        health: crate::input_health::HealthSummary,
        pricing_diagnostics: crate::pricing::PricingDiagnostics,
    }

    let record = AttributedUsageRecord::new_with_agent(
        ClientId::OpenCode,
        "claude-opus-4.6",
        "anthropic",
        "persisted-agent-overflow",
        1_735_689_600_000,
        crate::TokenBreakdown {
            input: 1,
            ..crate::TokenBreakdown::default()
        },
        0.0,
        Some("Sisyphus".to_string()),
    );
    let mut builder = UsageIndexBuilder::new();
    builder.push(&record).unwrap();
    let mut usage_index = builder.finish();
    let agent = usage_index
        .agent_map
        .values_mut()
        .next()
        .expect("fixture creates one agent");
    agent.tokens.input = u64::MAX;
    agent.tokens.output = 1;
    let universe = ClientUniverse::new([ClientId::OpenCode]).unwrap();
    let acquisition = crate::AcquisitionConfig::new(
        std::path::PathBuf::from("/tmp/tokenx-generation-deserialize-overflow"),
        crate::DateRange::none(),
        universe,
        crate::scanner::ScannerSettings::default(),
        crate::CalendarContext::explicit("UTC").unwrap(),
        crate::PricingContext::explicit_with_catalog("test-custom", "test-catalog"),
    )
    .unwrap();
    let encoded = bincode::serialize(&InvalidGenerationWire {
        acquisition,
        source_fingerprint: crate::SourceFingerprint::from_bytes([0; 32]),
        usage_index,
        sessions: Vec::new(),
        input_footprint: crate::InputFootprint::from_client_bytes([(ClientId::OpenCode, 1)])
            .unwrap(),
        health: crate::input_health::HealthSummary::default(),
        pricing_diagnostics: Vec::new(),
    })
    .unwrap();

    let error = bincode::deserialize::<crate::Generation>(&encoded).unwrap_err();

    assert!(error.to_string().contains("agent_map"));
    assert!(error.to_string().contains("overflowing token totals"));
}

#[test]
fn cross_day_overflow_is_rejected_during_index_validation() {
    let mut builder = UsageIndexBuilder::new();
    for timestamp in [1_735_689_600_000, 1_735_776_000_000] {
        builder
            .push(&collision_message(
                ClientId::Claude,
                "openai",
                "session",
                "model",
                1,
                timestamp,
            ))
            .unwrap();
    }
    let mut index = builder.finish();
    index
        .usage_totals_by_client
        .get_mut(&ClientId::Claude)
        .unwrap()
        .tokens = UsageTokenBreakdown {
        input: u64::MAX,
        ..UsageTokenBreakdown::default()
    };
    for model in index.model_map.values_mut() {
        model.tokens = UsageTokenBreakdown {
            input: u64::MAX,
            ..UsageTokenBreakdown::default()
        };
    }
    for day in index.daily_map.values_mut() {
        let client = day.clients.get_mut(&ClientId::Claude).unwrap();
        client.tokens = UsageTokenBreakdown {
            input: u64::MAX,
            ..UsageTokenBreakdown::default()
        };
        for model in client.models.values_mut() {
            model.tokens = UsageTokenBreakdown {
                input: u64::MAX,
                ..UsageTokenBreakdown::default()
            };
        }
    }

    let universe = ClientUniverse::new([ClientId::Claude]).unwrap();
    let before = bincode::serialize(&index).unwrap();
    assert_eq!(
        index.validate(&universe),
        Err(UsageIndexValidationError::ProjectionOverflow {
            group_by: GroupBy::Model,
            field: "cross-day token totals",
        })
    );
    assert_eq!(bincode::serialize(&index).unwrap(), before);
}

#[test]
fn cross_day_and_hour_counts_are_rejected_during_index_validation() {
    let make_index = || {
        let mut builder = UsageIndexBuilder::new();
        for timestamp in [1_735_689_600_000, 1_735_776_000_000] {
            builder
                .push(&collision_message(
                    ClientId::Claude,
                    "openai",
                    "session",
                    "model",
                    1,
                    timestamp,
                ))
                .unwrap();
        }
        builder.finish()
    };
    let universe = ClientUniverse::new([ClientId::Claude]).unwrap();

    for (hourly, turns, expected_field) in [
        (false, false, "cross-day message count"),
        (false, true, "cross-day turn count"),
        (true, false, "cross-hour message count"),
        (true, true, "cross-hour turn count"),
    ] {
        let mut index = make_index();
        let mut values = [u32::MAX, 1].into_iter();
        if hourly {
            for bucket in index.hourly_map.values_mut() {
                let client = bucket.clients.get_mut(&ClientId::Claude).unwrap();
                if turns {
                    client.turn_count = values.next().unwrap();
                } else {
                    client.message_count = values.next().unwrap();
                }
            }
        } else {
            for bucket in index.daily_map.values_mut() {
                let client = bucket.clients.get_mut(&ClientId::Claude).unwrap();
                if turns {
                    client.turn_count = values.next().unwrap();
                } else {
                    client.message_count = values.next().unwrap();
                }
            }
        }

        assert_eq!(
            index.validate(&universe),
            Err(UsageIndexValidationError::ProjectionOverflow {
                group_by: GroupBy::Model,
                field: expected_field,
            })
        );
    }
}

#[test]
fn cross_day_and_hour_costs_are_rejected_during_index_validation() {
    let make_index = || {
        let mut builder = UsageIndexBuilder::new();
        for timestamp in [1_735_689_600_000, 1_735_776_000_000] {
            builder
                .push(&collision_message(
                    ClientId::Claude,
                    "openai",
                    "session",
                    "model",
                    1,
                    timestamp,
                ))
                .unwrap();
        }
        builder.finish()
    };
    let universe = ClientUniverse::new([ClientId::Claude]).unwrap();

    for (hourly, expected_field) in [(false, "cross-day cost"), (true, "cross-hour cost")] {
        let mut index = make_index();
        if hourly {
            for bucket in index.hourly_map.values_mut() {
                bucket.clients.get_mut(&ClientId::Claude).unwrap().cost = f64::MAX;
            }
        } else {
            for bucket in index.daily_map.values_mut() {
                bucket.clients.get_mut(&ClientId::Claude).unwrap().cost = f64::MAX;
            }
        }

        assert_eq!(
            index.validate(&universe),
            Err(UsageIndexValidationError::ProjectionOverflow {
                group_by: GroupBy::Model,
                field: expected_field,
            })
        );
    }
}

#[test]
fn independently_built_indexes_project_identically_across_hash_seeds() {
    // Separate accumulators create independently seeded HashMaps.
    let first = reprojection_index();
    let second = reprojection_index();
    for group_by in [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
    ] {
        assert_usage_data_eq(
            &first.project_usage(&group_by, projection_date()).unwrap(),
            &second.project_usage(&group_by, projection_date()).unwrap(),
        );
    }
}

#[test]
fn client_projection_matches_a_fresh_fold_of_only_the_selected_clients() {
    let corpus = reprojection_corpus();
    let full = reprojection_index();
    for selected in [
        HashSet::from([ClientId::Claude]),
        HashSet::from([ClientId::Codex]),
        HashSet::from([ClientId::Claude, ClientId::Codex]),
    ] {
        let mut expected = UsageIndexBuilder::new();
        for message in &corpus {
            if selected.contains(&message.client) {
                expected.push(message).unwrap();
            }
        }
        let expected = expected.finish();
        for group_by in [
            GroupBy::Model,
            GroupBy::ClientModel,
            GroupBy::ClientProviderModel,
            GroupBy::WorkspaceModel,
        ] {
            assert_usage_data_eq(
                &full
                    .project_usage_for_clients(&group_by, &selected, projection_date())
                    .unwrap(),
                &expected
                    .project_usage(&group_by, projection_date())
                    .unwrap(),
            );
        }
    }
}

#[test]
fn reprojection_switching_groupings_does_not_pollute_state() {
    let acc = reprojection_index();
    let baseline = acc
        .project_usage(&GroupBy::Model, projection_date())
        .unwrap();
    let _ = acc.project_usage(&GroupBy::ClientProviderModel, projection_date());
    let _ = acc.project_usage(&GroupBy::WorkspaceModel, projection_date());
    let rerun = acc
        .project_usage(&GroupBy::Model, projection_date())
        .unwrap();
    assert_usage_data_eq(&baseline, &rerun);
}

#[test]
fn reprojection_preserves_group_independent_views() {
    let acc = reprojection_index();
    let reference = acc
        .project_usage(&GroupBy::Model, projection_date())
        .unwrap();
    for group_by in [
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
    ] {
        let projected = acc.project_usage(&group_by, projection_date()).unwrap();
        assert_eq!(projected.total_tokens, reference.total_tokens);
        assert!((projected.total_cost - reference.total_cost).abs() < 1e-9);
        assert_agents_eq(&projected.agents, &reference.agents);
        assert_eq!(
            (projected.current_streak, projected.longest_streak),
            (reference.current_streak, reference.longest_streak)
        );

        // Day- and hour-level rollups are accumulated per message, not
        // re-folded, so they stay bit-identical across groupings.
        let daily_rollup = |data: &UsageProjection| {
            data.daily
                .iter()
                .map(|day| {
                    (
                        day.date,
                        day.tokens.total(),
                        day.cost.to_bits(),
                        day.message_count,
                        day.turn_count,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(daily_rollup(&projected), daily_rollup(&reference));
        let hourly_rollup = |data: &UsageProjection| {
            data.hourly
                .iter()
                .map(|hour| {
                    (
                        hour.datetime,
                        hour.tokens.total(),
                        hour.cost.to_bits(),
                        hour.message_count,
                        hour.turn_count,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(hourly_rollup(&projected), hourly_rollup(&reference));
        assert_graph_eq(&projected.graph, &reference.graph);
    }
}

#[test]
fn reprojection_derives_each_grouping_from_one_frozen_index() {
    let corpus = reprojection_corpus();
    let day0 = test_calendar_fields(corpus[0].timestamp)
        .expect("day0 UTC calendar fields")
        .0;
    let day1 = test_calendar_fields(corpus[3].timestamp)
        .expect("day1 UTC calendar fields")
        .0;
    let day0_hour12 = timestamp_to_hour(corpus[0].timestamp).expect("day0 hour12");
    let day0_hour13 = timestamp_to_hour(corpus[2].timestamp).expect("day0 hour13");
    let mut acc = UsageIndexBuilder::new();
    for message in &corpus {
        acc.push(message).unwrap();
    }
    let acc = acc.finish();

    let model = acc
        .project_usage(&GroupBy::Model, projection_date())
        .unwrap();
    assert_eq!(model.models.len(), 2);
    let gpt = model
        .models
        .iter()
        .find(|entry| entry.model_id.as_ref() == "gpt-5.5")
        .expect("merged gpt-5.5 entry");
    assert_eq!(gpt.provider.as_ref(), "azure, openai");
    assert_eq!(
        gpt.clients,
        [ClientId::Qwen, ClientId::Claude, ClientId::Codex]
    );
    assert_eq!(gpt.session_count, 3);
    assert_eq!(gpt.tokens.total(), 1370);
    assert!((gpt.cost - 1.1).abs() < 1e-9);
    assert_eq!(gpt.workspace_key, None);
    assert_eq!(gpt.workspace_label, None);
    let sonnet = model
        .models
        .iter()
        .find(|entry| entry.model_id.as_ref() == "claude-sonnet-4.5")
        .expect("claude-sonnet-4.5 entry");
    assert_eq!(sonnet.session_count, 1);

    // Model grouping merges providers in the daily detail, attributing
    // the first-seen provider, and keeps bare-model hourly keys.
    let day0_claude = &model
        .daily
        .iter()
        .find(|day| day.date == day0)
        .expect("day0 usage")
        .client_breakdown[&ClientId::Claude];
    let gpt_daily = day0_claude
        .models
        .iter()
        .find(|entry| entry.model_id.as_ref() == "gpt-5.5")
        .unwrap();
    assert_eq!(gpt_daily.provider.as_ref(), "openai");
    assert_eq!(gpt_daily.tokens.total(), 520);
    assert!((gpt_daily.cost - 0.4).abs() < 1e-9);
    let hour12 = model
        .hourly
        .iter()
        .find(|hour| hour.datetime == day0_hour12)
        .expect("day0 hour12 usage");
    assert!(hour12
        .models
        .iter()
        .any(|entry| entry.model_id.as_ref() == "gpt-5.5"));

    let client_model = acc
        .project_usage(&GroupBy::ClientModel, projection_date())
        .unwrap();
    assert_eq!(client_model.models.len(), 4);
    let claude_gpt = client_model
        .models
        .iter()
        .find(|entry| {
            entry.model_id.as_ref() == "gpt-5.5" && entry.clients.as_slice() == [ClientId::Claude]
        })
        .expect("claude gpt-5.5 entry");
    assert_eq!(claude_gpt.provider.as_ref(), "azure, openai");
    assert_eq!(claude_gpt.tokens.total(), 520);
    assert_eq!(claude_gpt.session_count, 1);
    let day0_claude = &client_model
        .daily
        .iter()
        .find(|day| day.date == day0)
        .expect("day0 usage")
        .client_breakdown[&ClientId::Claude];
    assert!(day0_claude
        .models
        .iter()
        .any(|entry| entry.model_id.as_ref() == "gpt-5.5"));

    let cpm = acc
        .project_usage(&GroupBy::ClientProviderModel, projection_date())
        .unwrap();
    assert_eq!(cpm.models.len(), 5);
    let day0_claude = &cpm
        .daily
        .iter()
        .find(|day| day.date == day0)
        .expect("day0 usage")
        .client_breakdown[&ClientId::Claude];
    assert!(day0_claude
        .models
        .iter()
        .any(|entry| entry.provider.as_ref() == "openai"));
    assert!(day0_claude
        .models
        .iter()
        .any(|entry| entry.provider.as_ref() == "azure"));
    // Only ClientProviderModel splits hourly models by provider.
    let cpm_hour13 = cpm
        .hourly
        .iter()
        .find(|hour| hour.datetime == day0_hour13)
        .expect("day0 hour13 usage");
    assert_eq!(cpm_hour13.models.len(), 1);
    assert!(cpm_hour13
        .models
        .iter()
        .any(|entry| entry.provider.as_ref() == "azure" && entry.model_id.as_ref() == "gpt-5.5"));
    let model_hour13 = model
        .hourly
        .iter()
        .find(|hour| hour.datetime == day0_hour13)
        .expect("day0 hour13 usage");
    assert!(model_hour13
        .models
        .iter()
        .any(|entry| entry.model_id.as_ref() == "gpt-5.5"));

    let workspace = acc
        .project_usage(&GroupBy::WorkspaceModel, projection_date())
        .unwrap();
    assert_eq!(workspace.models.len(), 3);
    let repo_a = workspace
        .models
        .iter()
        .find(|entry| entry.workspace_key.as_deref() == Some("/repo-a"))
        .expect("repo-a workspace entry");
    assert_eq!(repo_a.workspace_label.as_deref(), Some("repo-a"));
    assert_eq!(
        repo_a.clients,
        [ClientId::Qwen, ClientId::Codex, ClientId::Claude]
    );
    assert_eq!(repo_a.session_count, 3);
    assert_eq!(repo_a.tokens.total(), 1000);
    let repo_b = workspace
        .models
        .iter()
        .find(|entry| entry.workspace_key.as_deref() == Some("/repo-b"))
        .expect("repo-b workspace entry");
    assert_eq!(repo_b.workspace_label.as_deref(), Some("repo-b"));
    let unknown = workspace
        .models
        .iter()
        .find(|entry| entry.workspace_key.is_none())
        .expect("unknown workspace entry");
    assert_eq!(
        unknown.workspace_label.as_deref(),
        Some(UNKNOWN_WORKSPACE_LABEL)
    );
    let day0_claude = &workspace
        .daily
        .iter()
        .find(|day| day.date == day0)
        .expect("day0 usage")
        .client_breakdown[&ClientId::Claude];
    assert!(day0_claude
        .models
        .iter()
        .any(|entry| entry.workspace_key.as_deref() == Some("/repo-a")));
    assert!(day0_claude
        .models
        .iter()
        .any(|entry| entry.workspace_key.as_deref() == Some("/repo-b")));
    let day1_claude = &workspace
        .daily
        .iter()
        .find(|day| day.date == day1)
        .expect("day1 usage")
        .client_breakdown[&ClientId::Claude];
    assert!(day1_claude.models.iter().any(
        |entry| entry.workspace_key.is_none() && entry.model_id.as_ref() == "claude-sonnet-4.5"
    ));
}

fn hourly(hour: u32, input_tokens: u64, cost: f64) -> HourlyUsage {
    HourlyUsage {
        datetime: NaiveDate::from_ymd_opt(2024, 6, 10)
            .unwrap()
            .and_hms_opt(hour, 0, 0)
            .unwrap(),
        tokens: UsageTokenBreakdown {
            input: input_tokens,
            ..UsageTokenBreakdown::default()
        },
        cost,
        clients: BTreeSet::new(),
        models: Vec::new(),
        message_count: 0,
        turn_count: 0,
    }
}

#[test]
fn find_peak_hour_breaks_token_ties_deterministically() {
    let high_cost = vec![hourly(8, 100, 2.0), hourly(12, 100, 3.0)];
    assert_eq!(find_peak_hour(&high_cost), Ok(Some((12, 100, 3.0))));

    let earliest_hour = vec![hourly(10, 100, 2.0), hourly(8, 100, 2.0)];
    assert_eq!(find_peak_hour(&earliest_hour), Ok(Some((8, 100, 2.0))));
}

#[test]
fn period_helpers_return_typed_overflow_errors() {
    let hourly = vec![hourly(8, u64::MAX, 0.0), hourly(8, 1, 0.0)];

    assert_eq!(
        aggregate_by_period(&hourly).unwrap_err().field(),
        "period-profile token total"
    );
    assert_eq!(
        find_peak_hour(&hourly).unwrap_err().field(),
        "peak-hour token total"
    );

    let daily = [(10, u64::MAX), (11, 1)]
        .into_iter()
        .map(|(day, input)| DailyUsage {
            date: NaiveDate::from_ymd_opt(2024, 6, day).unwrap(),
            tokens: UsageTokenBreakdown {
                input,
                ..UsageTokenBreakdown::default()
            },
            cost: 0.0,
            client_breakdown: BTreeMap::new(),
            message_count: 1,
            turn_count: 0,
        })
        .collect();
    let usage = UsageProjection {
        daily,
        ..UsageProjection::default()
    };

    assert_eq!(
        build_period_usage(&usage, PeriodKind::Monthly)
            .unwrap_err()
            .field(),
        "period token totals"
    );
}
