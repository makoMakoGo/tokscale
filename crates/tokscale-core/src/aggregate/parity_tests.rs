//! Aggregation engine contract tests. Reports whose old folds have been deleted
//! exercise the message-list entrypoints and the engine interface to keep
//! ordering and serialization contracts pinned.

#![cfg(test)]

use std::ffi::OsString;

use crate::aggregate::{AggregationConfig, AggregationEngine, DateRange, ViewSet};
use crate::sessions::UnifiedMessage;
use crate::usage_views::UsageData;
use crate::{sessionize, GroupBy, ModelReport, TokenBreakdown};
use serial_test::serial;

/// Pin `TZ=UTC` for date/hour bucketing that reads `chrono::Local`.
fn pin_tz() -> TzGuard {
    let old = std::env::var_os("TZ");
    std::env::set_var("TZ", "UTC");
    TzGuard { old }
}

struct TzGuard {
    old: Option<OsString>,
}

impl Drop for TzGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.old {
            std::env::set_var("TZ", value);
        } else {
            std::env::remove_var("TZ");
        }
    }
}

/// A timestamp at UTC noon for `YYYY-MM-DD`; tests pin `TZ=UTC` before any
/// local date/hour bucketing reads it.
fn ts(date: &str) -> i64 {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .expect("valid test date")
        .and_hms_opt(12, 0, 0)
        .expect("valid noon timestamp")
        .and_utc()
        .timestamp_millis()
}

fn msg(
    client: &str,
    model: &str,
    provider: &str,
    session: &str,
    date: &str,
    cost: f64,
) -> UnifiedMessage {
    UnifiedMessage::new(
        client,
        model,
        provider,
        session,
        ts(date),
        TokenBreakdown {
            input: 100,
            output: 50,
            cache_read: 10,
            cache_write: 5,
            reasoning: 3,
        },
        cost,
    )
}

#[allow(clippy::too_many_arguments)]
fn session_msg(
    session_id: &str,
    client: &str,
    provider: &str,
    model: &str,
    timestamp_ms: i64,
    tokens: TokenBreakdown,
    cost: f64,
) -> UnifiedMessage {
    UnifiedMessage::new(
        client,
        model,
        provider,
        session_id,
        timestamp_ms,
        tokens,
        cost,
    )
}

/// Corpus exercising every order-/tie-break-/drop-sensitive path the maps
/// flagged: >=2 equal-cost models; >=2 clients in a merged bucket with distinct
/// arrival order; same client:model across two providers (provider merge);
/// timestamp<=0 row (hourly skip); date.len()<7 drop; NaN cost (sanitation).
fn corpus() -> Vec<UnifiedMessage> {
    let mut msgs = vec![
        // Two equal-cost distinct models (model-entry NaN-last sort, no tie-break leg).
        msg(
            "claude",
            "claude-sonnet-4",
            "anthropic",
            "s1",
            "2024-06-10",
            5.0,
        ),
        msg(
            "claude",
            "claude-opus-4",
            "anthropic",
            "s1",
            "2024-06-10",
            5.0,
        ),
        // Two clients merged under GroupBy::Model, distinct arrival order.
        msg("codex", "gpt-5", "openai", "s2", "2024-06-11", 2.0),
        msg("gemini", "gpt-5", "openai", "s3", "2024-06-11", 8.0),
        // Same client:model across two providers (provider comma-merge + sort/dedup).
        msg(
            "claude",
            "claude-sonnet-4",
            "anthropic-bedrock",
            "s1",
            "2024-06-12",
            1.0,
        ),
        // A second month (month sort) + a NaN cost row (sanitation).
        msg(
            "claude",
            "claude-sonnet-4",
            "anthropic",
            "s1",
            "2024-05-01",
            f64::NAN,
        ),
    ];

    // A timestamp<=0 row is kept for non-hourly views, but skipped by hourly
    // distribution because it has no valid hour bucket.
    let mut zero_ts = msg(
        "claude",
        "claude-sonnet-4",
        "anthropic",
        "s1",
        "1970-01-01",
        0.5,
    );
    zero_ts.timestamp = 0;
    msgs.push(zero_ts);
    msgs
}

// ---- Message-list runners (production shape without parse/pricing) ----

fn model_report_from_entrypoint(msgs: &[UnifiedMessage], gb: &GroupBy) -> ModelReport {
    let entries = crate::aggregate_model_usage_entries_pub(msgs.to_vec(), gb);
    let total_input: i64 = entries.iter().map(|e| e.input).sum();
    let total_output: i64 = entries.iter().map(|e| e.output).sum();
    let total_cache_read: i64 = entries.iter().map(|e| e.cache_read).sum();
    let total_cache_write: i64 = entries.iter().map(|e| e.cache_write).sum();
    let total_messages: i32 = entries.iter().map(|e| e.message_count).sum();
    let total_cost: f64 = entries.iter().map(|e| e.cost).sum();
    ModelReport {
        health: Default::default(),
        entries,
        total_input,
        total_output,
        total_cache_read,
        total_cache_write,
        total_messages,
        total_cost,
        processing_time_ms: 0,
    }
}

fn neutralize_processing_time(report: &mut ModelReport) {
    report.processing_time_ms = 0;
}

fn engine_usage_data(msgs: &[UnifiedMessage], group_by: GroupBy) -> UsageData {
    let mut engine = AggregationEngine::new(AggregationConfig {
        group_by,
        date_range: DateRange::none(),
        views: ViewSet::TUI,
    });
    for msg in msgs {
        engine.push(msg);
    }
    engine.finish().tui_usage.expect("tui view requested")
}

#[test]
fn agents_view_keeps_client_dimension() {
    let mut opencode = UnifiedMessage::new_with_agent(
        "opencode",
        "gpt-5",
        "openai",
        "s1",
        ts("2024-06-10"),
        TokenBreakdown {
            input: 7,
            output: 5,
            cache_read: 3,
            cache_write: 2,
            reasoning: 1,
        },
        0.5,
        Some("shared-agent".to_string()),
    );
    opencode.message_count = 3;
    let messages = vec![
        opencode,
        UnifiedMessage::new_with_agent(
            "codex",
            "gpt-5",
            "openai",
            "s2",
            ts("2024-06-10"),
            TokenBreakdown {
                input: 11,
                output: 13,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.75,
            Some("shared-agent".to_string()),
        ),
    ];
    let mut engine = AggregationEngine::new(AggregationConfig {
        group_by: GroupBy::default(),
        date_range: DateRange::none(),
        views: ViewSet::AGENTS,
    });
    for message in &messages {
        engine.push(message);
    }

    let agents = engine.finish().agent_usage.expect("agents view requested");

    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].client, "codex");
    assert_eq!(agents[0].agent, "Shared Agent");
    assert_eq!(agents[0].tokens.total(), 24);
    assert_eq!(agents[1].client, "opencode");
    assert_eq!(agents[1].agent, "Shared Agent");
    assert_eq!(agents[1].tokens.total(), 18);
    assert_eq!(agents[1].message_count, 3);
}

// ---- Entrypoint consistency and engine contract tests ----

#[test]
#[serial]
fn contract_graph_result() {
    let _tz = pin_tz();
    let mut msgs = corpus();
    msgs[0].duration_ms = Some(60_000);

    let mut actual = {
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: GroupBy::ClientModel,
            date_range: DateRange::none(),
            views: ViewSet::GRAPH | ViewSet::TIME_METRICS,
        });
        for m in &msgs {
            e.push(m);
        }
        e.finish().graph.expect("graph view requested")
    };

    actual.meta.generated_at.clear();
    actual.meta.processing_time_ms = 0;

    assert_eq!(actual.contributions.len(), 5);
    assert!(
        actual
            .contributions
            .windows(2)
            .all(|days| days[0].date <= days[1].date),
        "graph contributions should be date-sorted",
    );
    assert_eq!(
        actual.meta.date_range_start,
        actual.contributions.first().unwrap().date
    );
    assert_eq!(
        actual.meta.date_range_end,
        actual.contributions.last().unwrap().date
    );
    let time_metrics = actual.time_metrics.as_ref().expect("graph time metrics");
    assert_eq!(time_metrics.total_active_time_ms, 60_000);
    assert_eq!(time_metrics.total_wall_time_ms, 60_000);
    assert_eq!(time_metrics.longest_continuous_ms, 60_000);
    assert_eq!(time_metrics.max_concurrent_sessions, 2);
    assert_eq!(time_metrics.session_count, 5);

    let june_10 = actual
        .contributions
        .iter()
        .find(|day| day.date == "2024-06-10")
        .expect("June 10 contribution");
    assert_eq!(june_10.totals.tokens, 336);
    assert!((june_10.totals.cost - 10.0).abs() < 0.0001);
    assert_eq!(june_10.totals.messages, 2);
    assert_eq!(june_10.intensity, 4);
    assert_eq!(june_10.active_time_ms, Some(60_000));
    assert_eq!(june_10.token_breakdown.input, 200);
    assert_eq!(june_10.token_breakdown.output, 100);
    assert_eq!(june_10.token_breakdown.cache_read, 20);
    assert_eq!(june_10.token_breakdown.cache_write, 10);
    assert_eq!(june_10.token_breakdown.reasoning, 6);
    assert_eq!(june_10.clients.len(), 2);
    assert_eq!(june_10.clients[0].client, "claude");
    assert_eq!(june_10.clients[0].model_id, "claude-opus-4");
    assert_eq!(june_10.clients[0].provider_id, "anthropic");
    assert_eq!(june_10.clients[0].tokens.total(), 168);
    assert_eq!(june_10.clients[0].messages, 1);
    assert_eq!(june_10.clients[1].client, "claude");
    assert_eq!(june_10.clients[1].model_id, "claude-sonnet-4");
    assert_eq!(june_10.clients[1].provider_id, "anthropic");
    assert_eq!(june_10.clients[1].tokens.total(), 168);
    assert_eq!(june_10.clients[1].messages, 1);

    let total_tokens: i64 = actual
        .contributions
        .iter()
        .map(|day| day.totals.tokens)
        .sum();
    let total_cost: f64 = actual.contributions.iter().map(|day| day.totals.cost).sum();
    assert_eq!(
        actual.summary.total_tokens, total_tokens,
        "graph summary tokens should derive from contributions",
    );
    assert!(
        (actual.summary.total_cost - total_cost).abs() < 0.0001,
        "graph summary cost should derive from contributions",
    );
    assert_eq!(actual.summary.total_days, 5);
    assert_eq!(actual.summary.active_days, 5);
    assert!((actual.summary.average_per_day - 4.3).abs() < 0.0001);
    assert!((actual.summary.max_cost_in_single_day - 10.0).abs() < 0.0001);
    assert_eq!(actual.summary.clients, vec!["claude", "codex", "gemini"]);
    assert_eq!(
        actual.summary.models,
        vec!["claude-opus-4", "claude-sonnet-4", "gpt-5"]
    );

    assert_eq!(actual.years.len(), 2);
    assert_eq!(actual.years[0].year, "1970");
    assert_eq!(actual.years[0].total_tokens, 168);
    assert!((actual.years[0].total_cost - 0.5).abs() < 0.0001);
    assert_eq!(actual.years[0].range_start, "1970-01-01");
    assert_eq!(actual.years[0].range_end, "1970-01-01");
    assert_eq!(actual.years[1].year, "2024");
    assert_eq!(actual.years[1].total_tokens, 1008);
    assert!((actual.years[1].total_cost - 21.0).abs() < 0.0001);
    assert_eq!(actual.years[1].range_start, "2024-05-01");
    assert_eq!(actual.years[1].range_end, "2024-06-12");
}

#[test]
#[serial]
fn graph_daily_accumulator_keeps_client_model_tuple_keys_distinct() {
    let _tz = pin_tz();
    let msgs = vec![
        msg("a", "b:c", "test-provider", "s1", "2024-06-10", 1.0),
        msg("a:b", "c", "test-provider", "s2", "2024-06-10", 2.0),
    ];

    let mut engine = AggregationEngine::new(AggregationConfig {
        group_by: GroupBy::ClientModel,
        date_range: DateRange::none(),
        views: ViewSet::GRAPH,
    });
    for message in &msgs {
        engine.push(message);
    }

    let graph = engine.finish().graph.expect("graph view requested");
    assert_eq!(graph.contributions.len(), 1);
    let clients = &graph.contributions[0].clients;
    assert_eq!(clients.len(), 2);
    assert_eq!(clients[0].client, "a");
    assert_eq!(clients[0].model_id, "b:c");
    assert_eq!(clients[1].client, "a:b");
    assert_eq!(clients[1].model_id, "c");
}

#[test]
#[serial]
fn graph_daily_accumulator_merges_provider_ids_for_same_client_model() {
    let _tz = pin_tz();
    let msgs = vec![
        msg(
            "claude",
            "claude-sonnet-4",
            "anthropic-bedrock",
            "s1",
            "2024-06-10",
            1.0,
        ),
        msg(
            "claude",
            "claude-sonnet-4",
            "anthropic",
            "s1",
            "2024-06-10",
            2.0,
        ),
    ];

    let mut engine = AggregationEngine::new(AggregationConfig {
        group_by: GroupBy::ClientModel,
        date_range: DateRange::none(),
        views: ViewSet::GRAPH,
    });
    for message in &msgs {
        engine.push(message);
    }

    let graph = engine.finish().graph.expect("graph view requested");
    assert_eq!(graph.contributions.len(), 1);
    let clients = &graph.contributions[0].clients;
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].client, "claude");
    assert_eq!(clients[0].model_id, "claude-sonnet-4");
    assert_eq!(clients[0].provider_id, "anthropic, anthropic-bedrock");
    assert_eq!(clients[0].messages, 2);
    assert_eq!(clients[0].tokens.total(), 336);
    assert!((clients[0].cost - 3.0).abs() < 0.0001);
}

#[test]
#[serial]
fn entrypoint_model_report_matches_engine_all_group_by() {
    let _tz = pin_tz();
    let msgs = corpus();
    for gb in [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
        GroupBy::Session,
        GroupBy::ClientSession,
    ] {
        let mut expected = model_report_from_entrypoint(&msgs, &gb);
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: gb.clone(),
            date_range: DateRange::none(),
            views: ViewSet::MODEL,
        });
        for m in &msgs {
            e.push(m);
        }
        let mut actual = e.finish().model_report.expect("model view requested");
        neutralize_processing_time(&mut expected);
        neutralize_processing_time(&mut actual);
        assert_eq!(
            serde_json::to_string(&expected).unwrap(),
            serde_json::to_string(&actual).unwrap(),
            "ModelReport entrypoint consistency failed for {gb:?}",
        );
    }
}

#[test]
#[serial]
fn entrypoint_monthly_report_matches_engine() {
    let _tz = pin_tz();
    let msgs = corpus();

    let expected = crate::monthly_report_from_messages_pub(msgs.clone());

    let actual = {
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: GroupBy::ClientModel,
            date_range: DateRange::none(),
            views: ViewSet::MONTHLY,
        });
        for m in &msgs {
            e.push(m);
        }
        e.finish().monthly_report.expect("monthly view requested")
    };

    assert_eq!(
        serde_json::to_string(&expected).unwrap(),
        serde_json::to_string(&actual).unwrap(),
        "MonthlyReport entrypoint consistency failed",
    );
}

#[test]
#[serial]
fn entrypoint_hourly_report_matches_engine() {
    let _tz = pin_tz();
    let msgs = corpus();

    let expected = crate::hourly_report_from_messages_pub(msgs.clone());
    let actual = {
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: GroupBy::ClientModel,
            date_range: DateRange::none(),
            views: ViewSet::HOURLY,
        });
        for m in &msgs {
            e.push(m);
        }
        e.finish().hourly_report.expect("hourly view requested")
    };
    assert_eq!(
        serde_json::to_string(&expected).unwrap(),
        serde_json::to_string(&actual).unwrap(),
        "HourlyReport entrypoint consistency failed",
    );
}

#[test]
#[serial]
fn parity_time_metrics() {
    let _tz = pin_tz();
    let msgs = corpus();

    let intervals = sessionize::sessionize_time_intervals(&msgs, sessionize::DEFAULT_IDLE_GAP_MS);
    let metrics = sessionize::compute_time_metrics(&intervals, sessionize::DEFAULT_IDLE_GAP_MS);
    let old = crate::TimeMetricsReport {
        metrics,
        processing_time_ms: 0,
        health: Default::default(),
    };
    let new = {
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: GroupBy::ClientModel,
            date_range: DateRange::none(),
            views: ViewSet::TIME_METRICS,
        });
        for m in &msgs {
            e.push(m);
        }
        e.finish()
            .time_metrics
            .expect("time-metrics view requested")
    };
    assert_eq!(
        serde_json::to_string(&old).unwrap(),
        serde_json::to_string(&new).unwrap(),
        "TimeMetricsReport byte parity failed",
    );
}

#[test]
#[serial]
fn contract_session_contributions() {
    let t = TokenBreakdown {
        input: 100,
        output: 50,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };
    let msgs = vec![
        session_msg(
            "s-a",
            "codex",
            "openai",
            "gpt-5",
            1_700_000_001_000,
            t.clone(),
            0.01,
        ),
        session_msg(
            "s-a",
            "codex",
            "openai",
            "gpt-5",
            1_700_000_002_000,
            t.clone(),
            0.01,
        ),
        session_msg(
            "s-b",
            "amp",
            "anthropic",
            "claude-haiku-4.5",
            1_700_000_005_000,
            t.clone(),
            0.02,
        ),
        session_msg(
            "s-b",
            "amp",
            "anthropic",
            "claude-haiku-4.5",
            1_700_000_006_000,
            t.clone(),
            0.02,
        ),
        session_msg(
            "s-c",
            "claude",
            "anthropic",
            "claude-sonnet-4.5",
            1_700_000_100_000,
            t.clone(),
            0.05,
        ),
        session_msg(
            "s-c",
            "claude",
            "anthropic",
            "claude-sonnet-4.5",
            1_700_000_101_000,
            t.clone(),
            0.05,
        ),
        session_msg(
            "shared",
            "amp",
            "anthropic",
            "claude-haiku-4.5",
            1_700_000_110_000,
            TokenBreakdown {
                input: 10,
                output: 10,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.001,
        ),
        session_msg(
            "shared",
            "codex",
            "openai",
            "gpt-5",
            1_700_000_111_000,
            TokenBreakdown {
                input: 1000,
                output: 500,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            0.50,
        ),
    ];

    let sessions = {
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: GroupBy::ClientModel,
            date_range: DateRange::none(),
            views: ViewSet::SESSIONS,
        });
        for m in &msgs {
            e.push(m);
        }
        e.finish()
            .session_contributions
            .expect("sessions view requested")
    };

    assert_eq!(sessions.len(), 4);
    assert_eq!(sessions[0].session_id, "shared");
    assert_eq!(sessions[1].session_id, "s-c");
    assert_eq!(sessions[2].session_id, "s-b");
    assert_eq!(sessions[3].session_id, "s-a");

    let s_a = sessions
        .iter()
        .find(|session| session.session_id == "s-a")
        .unwrap();
    assert_eq!(s_a.totals.messages, 2);
    assert_eq!(s_a.totals.tokens, 300);
    assert!((s_a.totals.cost - 0.02).abs() < 1e-9);
    assert_eq!(s_a.token_breakdown.input, 200);
    assert_eq!(s_a.token_breakdown.output, 100);
    assert_eq!(s_a.token_breakdown.cache_read, 0);
    assert_eq!(s_a.token_breakdown.cache_write, 0);
    assert_eq!(s_a.token_breakdown.reasoning, 0);
    assert_eq!(s_a.first_seen, 1_700_000_001);
    assert_eq!(s_a.last_seen, 1_700_000_002);
    assert_eq!(s_a.clients.len(), 1);
    assert_eq!(s_a.clients[0].client, "codex");
    assert_eq!(s_a.clients[0].provider_id, "openai");
    assert_eq!(s_a.clients[0].model_id, "gpt-5");
    assert_eq!(s_a.clients[0].tokens.input, 200);
    assert_eq!(s_a.clients[0].tokens.output, 100);
    assert_eq!(s_a.clients[0].messages, 2);
    assert!((s_a.clients[0].cost - 0.02).abs() < 1e-9);

    let shared = sessions
        .iter()
        .find(|session| session.session_id == "shared")
        .unwrap();
    assert_eq!(shared.client, "codex");
    assert_eq!(shared.provider, "openai");
    assert_eq!(shared.model, "gpt-5");
    assert_eq!(shared.totals.messages, 2);
    assert_eq!(shared.totals.tokens, 1520);
    assert_eq!(shared.token_breakdown.input, 1010);
    assert_eq!(shared.token_breakdown.output, 510);
    assert_eq!(shared.token_breakdown.cache_read, 0);
    assert_eq!(shared.token_breakdown.cache_write, 0);
    assert_eq!(shared.token_breakdown.reasoning, 0);
    assert_eq!(shared.first_seen, 1_700_000_110);
    assert_eq!(shared.last_seen, 1_700_000_111);
    assert_eq!(shared.clients.len(), 2);
    assert_eq!(shared.clients[0].client, "codex");
    assert_eq!(shared.clients[0].provider_id, "openai");
    assert_eq!(shared.clients[0].model_id, "gpt-5");
    assert_eq!(shared.clients[0].tokens.input, 1000);
    assert_eq!(shared.clients[0].tokens.output, 500);
    assert_eq!(shared.clients[0].messages, 1);
    assert!((shared.clients[0].cost - 0.50).abs() < 1e-9);
    assert_eq!(shared.clients[1].client, "amp");
    assert_eq!(shared.clients[1].provider_id, "anthropic");
    assert_eq!(shared.clients[1].model_id, "claude-haiku-4.5");
    assert_eq!(shared.clients[1].tokens.input, 10);
    assert_eq!(shared.clients[1].tokens.output, 10);
    assert_eq!(shared.clients[1].messages, 1);
    assert!((shared.clients[1].cost - 0.001).abs() < 1e-9);
    assert!((shared.totals.cost - 0.501).abs() < 1e-9);
}

#[test]
#[serial]
fn session_accumulator_keeps_client_provider_model_tuple_keys_distinct() {
    let _tz = pin_tz();
    let msgs = vec![
        msg("a", "d", "b:c", "shared-session", "2024-06-10", 1.0),
        msg("a:b", "d", "c", "shared-session", "2024-06-10", 2.0),
    ];

    let mut engine = AggregationEngine::new(AggregationConfig {
        group_by: GroupBy::ClientModel,
        date_range: DateRange::none(),
        views: ViewSet::SESSIONS,
    });
    for message in &msgs {
        engine.push(message);
    }

    let sessions = engine
        .finish()
        .session_contributions
        .expect("sessions view requested");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].clients.len(), 2);

    let mut entries: Vec<_> = sessions[0]
        .clients
        .iter()
        .map(|entry| {
            (
                entry.client.as_str(),
                entry.provider_id.as_str(),
                entry.model_id.as_str(),
            )
        })
        .collect();
    entries.sort();
    assert_eq!(entries, vec![("a", "b:c", "d"), ("a:b", "c", "d")]);
}

#[test]
#[serial]
fn contract_tui_view_materializes_usage_data() {
    let _tz = pin_tz();
    let msgs = corpus();
    let mut engine = AggregationEngine::new(AggregationConfig {
        group_by: GroupBy::Model,
        date_range: DateRange::none(),
        views: ViewSet::TUI,
    });
    for msg in &msgs {
        engine.push(msg);
    }

    let data = engine.finish().tui_usage.expect("tui view requested");
    assert!(!data.models.is_empty(), "TUI models should be materialized");
    assert!(
        !data.daily.is_empty(),
        "TUI daily usage should be materialized"
    );
    assert!(
        !data.hourly.is_empty(),
        "TUI hourly usage should be materialized"
    );
    assert!(data.graph.is_some(), "TUI graph should be materialized");
    assert_eq!(
        data.total_tokens,
        data.models
            .iter()
            .map(|model| model.tokens.total())
            .sum::<u64>(),
        "TUI total tokens should derive from finished model entries",
    );
    assert!(!data.loading);
    assert!(data.error.is_none());
}

#[test]
#[serial]
fn contract_tui_workspace_provider_daily_and_streaks() {
    let _tz = pin_tz();
    let today = chrono::Local::now().date_naive();
    let yesterday = today - chrono::Duration::days(1);
    let two_days_ago = today - chrono::Duration::days(2);
    let today_s = today.format("%Y-%m-%d").to_string();
    let yesterday_s = yesterday.format("%Y-%m-%d").to_string();
    let two_days_ago_s = two_days_ago.format("%Y-%m-%d").to_string();

    let mut first = msg(
        "claude",
        "claude-sonnet-4",
        "anthropic",
        "workspace-session",
        &today_s,
        2.0,
    );
    first.set_workspace(Some("/Users/alice/repo-a".to_string()), None);
    first.is_turn_start = true;

    let mut second = msg(
        "claude",
        "claude-sonnet-4",
        "anthropic-bedrock",
        "workspace-session",
        &yesterday_s,
        3.0,
    );
    second.set_workspace(Some("/Users/alice/repo-a".to_string()), None);

    let third = msg(
        "codex",
        "gpt-5",
        "openai",
        "codex-session",
        &two_days_ago_s,
        1.0,
    );

    let data = engine_usage_data(
        &[first.clone(), second.clone(), third.clone()],
        GroupBy::WorkspaceModel,
    );

    assert_eq!(data.current_streak, 3);
    assert_eq!(data.longest_streak, 3);

    let workspace_model = data
        .models
        .iter()
        .find(|entry| entry.model == "claude-sonnet-4")
        .expect("workspace model entry");
    assert_eq!(
        workspace_model.workspace_key.as_deref(),
        Some("/Users/alice/repo-a")
    );
    assert_eq!(workspace_model.workspace_label.as_deref(), Some("repo-a"));
    assert_eq!(workspace_model.provider, "anthropic, anthropic-bedrock");
    assert_eq!(workspace_model.session_count, 1);

    let reversed = engine_usage_data(
        &[second.clone(), first.clone(), third],
        GroupBy::WorkspaceModel,
    );
    let reversed_workspace_model = reversed
        .models
        .iter()
        .find(|entry| entry.model == "claude-sonnet-4")
        .expect("reversed workspace model entry");
    assert_eq!(
        reversed_workspace_model.provider,
        "anthropic, anthropic-bedrock"
    );

    let today_usage = data
        .daily
        .iter()
        .find(|day| day.date == today)
        .expect("today usage");
    assert_eq!(today_usage.message_count, 1);
    assert_eq!(today_usage.turn_count, 1);

    let claude_source = today_usage
        .client_breakdown
        .get("claude")
        .expect("claude daily client");
    assert_eq!(claude_source.cost, 2.0);
    let daily_model = claude_source
        .models
        .values()
        .find(|model| model.model_id == "claude-sonnet-4")
        .expect("workspace daily model");
    assert_eq!(daily_model.display_name, "claude-sonnet-4");
    assert_eq!(
        daily_model.workspace_key.as_deref(),
        Some("/Users/alice/repo-a")
    );
    assert_eq!(daily_model.workspace_label.as_deref(), Some("repo-a"));
    assert_eq!(daily_model.provider, "anthropic");
    assert_eq!(daily_model.messages, 1);
}

#[test]
#[serial]
fn contract_tui_hourly_invalid_timestamp_is_not_bucketed() {
    let _tz = pin_tz();
    let mut zero = msg(
        "claude",
        "claude-sonnet-4",
        "anthropic",
        "zero-session",
        "1970-01-01",
        1.0,
    );
    zero.timestamp = 0;

    let data = engine_usage_data(&[zero], GroupBy::Model);

    assert!(data.hourly.is_empty());
    assert_eq!(data.total_tokens, 168);
    assert_eq!(data.total_cost, 1.0);
    assert_eq!(data.models.len(), 1);
    assert_eq!(data.models[0].tokens.total(), 168);
    assert_eq!(data.daily.len(), 1);
    assert_eq!(data.daily[0].tokens.total(), 168);
    assert_eq!(data.daily[0].message_count, 1);
}

// ===========================================================================
// Contract tests (C1.4) — pin the sort/tie-break comparators that the existing
// suite leaves uncovered. These assert the EXACT ordering, independent of the
// diff harness above, so a regression is caught even when the two paths happen
// to agree. Focused legs that depend on the C1.5 BLOCKERs are #[ignore]'d and
// enabled when C1.5 makes them deterministic.
// ===========================================================================

#[test]
fn contract_merged_clients_first_seen_tie_break() {
    // ordered_clients_by_token_contribution: total_tokens DESC -> first_seen
    // ASC -> client ASC. Existing tests cover only the total_tokens leg with
    // distinct totals. Pin the first_seen tie-break here.
    use crate::{ordered_clients_by_token_contribution, ClientContributionOrder};
    use std::collections::HashMap;
    let mut m = HashMap::new();
    // Equal tokens (50): first_seen decides -> b(0) before a(1).
    m.insert(
        "a".to_string(),
        ClientContributionOrder {
            first_seen: 1,
            total_tokens: 50,
        },
    );
    m.insert(
        "b".to_string(),
        ClientContributionOrder {
            first_seen: 0,
            total_tokens: 50,
        },
    );
    assert_eq!(ordered_clients_by_token_contribution(&m), "b, a");
}

#[test]
fn contract_merged_clients_name_tie_break() {
    // Equal tokens AND equal first_seen -> client name ASC.
    use crate::{ordered_clients_by_token_contribution, ClientContributionOrder};
    use std::collections::HashMap;
    let mut m = HashMap::new();
    m.insert(
        "zeta".to_string(),
        ClientContributionOrder {
            first_seen: 0,
            total_tokens: 7,
        },
    );
    m.insert(
        "alpha".to_string(),
        ClientContributionOrder {
            first_seen: 0,
            total_tokens: 7,
        },
    );
    assert_eq!(ordered_clients_by_token_contribution(&m), "alpha, zeta");
}

#[test]
fn contract_model_entry_cost_desc_nan_last() {
    // lib.rs:2122 — entries sort cost DESC, NaN sorts last.
    use crate::GroupBy;
    fn mk(model: &str, cost: f64) -> UnifiedMessage {
        UnifiedMessage::new(
            "c",
            model,
            "p",
            "s",
            ts("2024-06-10"),
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost,
        )
    }
    // Distinct costs, descending: 9.0, 5.0, 1.0, then NaN last.
    let msgs = vec![
        mk("m1", 1.0),
        mk("m2", 9.0),
        mk("m3", f64::NAN),
        mk("m4", 5.0),
    ];
    let entries = crate::aggregate_model_usage_entries_pub(msgs, &GroupBy::Model);
    let models: Vec<&str> = entries.iter().map(|e| e.model.as_str()).collect();
    assert_eq!(models, vec!["m2", "m4", "m1", "m3"]);
}

#[test]
fn contract_model_entry_equal_cost_tie_break() {
    // lib.rs:2122 has NO secondary key on equal cost. C1.5 will add
    // model -> provider -> ... so two equal-cost entries order by model ASC.
    use crate::GroupBy;
    fn mk(model: &str, cost: f64) -> UnifiedMessage {
        UnifiedMessage::new(
            "c",
            model,
            "p",
            "s",
            ts("2024-06-10"),
            TokenBreakdown {
                input: 1,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost,
        )
    }
    let msgs = vec![mk("zzz", 5.0), mk("aaa", 5.0)];
    let entries = crate::aggregate_model_usage_entries_pub(msgs, &GroupBy::Model);
    let models: Vec<&str> = entries.iter().map(|e| e.model.as_str()).collect();
    assert_eq!(models, vec!["aaa", "zzz"]);
}

#[test]
#[serial]
fn contract_hourly_full_key_asc() {
    // lib.rs:2375 — hourly entries sort by the full "YYYY-MM-DD HH:00" key ASC.
    let _tz = pin_tz();
    let msgs = vec![
        UnifiedMessage::new(
            "c",
            "m",
            "p",
            "s",
            ts("2024-06-10"),
            TokenBreakdown {
                input: 1,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.0,
        ),
        UnifiedMessage::new(
            "c",
            "m",
            "p",
            "s",
            ts("2024-05-01"),
            TokenBreakdown {
                input: 1,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.0,
        ),
    ];
    let report = crate::hourly_report_from_messages_pub(msgs);
    assert_eq!(report.entries.len(), 2);
    // May precedes June.
    assert!(report.entries[0].hour < report.entries[1].hour);
}

#[test]
#[serial]
fn contract_hourly_invalid_timestamp_is_not_bucketed() {
    // Hourly distribution only uses valid timestamp-derived hour buckets.
    let _tz = pin_tz();
    let mut zero = UnifiedMessage::new(
        "c",
        "m",
        "p",
        "s",
        ts("1970-01-01"),
        TokenBreakdown {
            input: 1,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
        1.0,
    );
    zero.timestamp = 0;
    let report = crate::hourly_report_from_messages_pub(vec![zero]);
    assert!(report.entries.is_empty());
}

#[test]
#[serial]
fn contract_monthly_month_asc() {
    // lib.rs:2257 — monthly entries sort by month ASC.
    let _tz = pin_tz();
    let msgs = vec![
        UnifiedMessage::new(
            "c",
            "m",
            "p",
            "s",
            ts("2024-06-15"),
            TokenBreakdown {
                input: 1,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.0,
        ),
        UnifiedMessage::new(
            "c",
            "m",
            "p",
            "s",
            ts("2024-05-15"),
            TokenBreakdown {
                input: 1,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.0,
        ),
    ];
    let report = crate::monthly_report_from_messages_pub(msgs);
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.entries[0].month, "2024-05");
    assert_eq!(report.entries[1].month, "2024-06");
}

#[test]
#[serial]
fn contract_monthly_models_sorted() {
    // MonthlyUsage.models is an unsorted HashSet->Vec today; C1.5 will sort it.
    let _tz = pin_tz();
    let msgs = vec![
        UnifiedMessage::new(
            "c",
            "zzz-model",
            "p",
            "s",
            ts("2024-06-15"),
            TokenBreakdown {
                input: 1,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.0,
        ),
        UnifiedMessage::new(
            "c",
            "aaa-model",
            "p",
            "s",
            ts("2024-06-16"),
            TokenBreakdown {
                input: 1,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            1.0,
        ),
    ];
    let report = crate::monthly_report_from_messages_pub(msgs);
    assert_eq!(report.entries[0].models, vec!["aaa-model", "zzz-model"]);
}

#[test]
#[serial]
fn contract_monthly_preserves_reasoning_bucket() {
    let _tz = pin_tz();
    let report = crate::monthly_report_from_messages_pub(vec![UnifiedMessage::new(
        "omp",
        "gpt-5.5",
        "openai",
        "reasoning-session",
        ts("2026-01-01"),
        TokenBreakdown {
            input: 100,
            output: 25,
            cache_read: 10,
            cache_write: 5,
            reasoning: 25,
        },
        1.0,
    )]);

    assert_eq!(report.entries.len(), 1);
    let entry = &report.entries[0];
    assert_eq!(entry.reasoning, 25);
    assert_eq!(
        entry.input + entry.output + entry.cache_read + entry.cache_write + entry.reasoning,
        165
    );
}
