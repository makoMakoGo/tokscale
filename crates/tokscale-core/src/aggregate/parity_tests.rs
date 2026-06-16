//! Cross-path parity harness: the OLD aggregator paths vs the NEW
//! `AggregationEngine`, over identical inputs, must produce byte-identical
//! serialized reports (with non-deterministic fields neutralized). Green here
//! is the gate for every deletion in C1.6-C1.9.

#![cfg(test)]

use crate::aggregate::{AggregationConfig, AggregationEngine, DateRange, ViewSet};
use crate::sessions::UnifiedMessage;
use crate::{aggregator, sessionize, GroupBy, ModelReport, TokenBreakdown};
use serial_test::serial;

/// Pin `TZ=UTC` for date/hour bucketing that reads `chrono::Local`.
fn pin_tz() {
    std::env::set_var("TZ", "UTC");
}

/// A timestamp at local noon for `YYYY-MM-DD`, stable across timezones within
/// a day shift of ±12h. Mirrors the existing `aggregator.rs` mock idiom.
fn ts(date: &str) -> i64 {
    // date = "YYYY-MM-DD"; encode noon-of-day in ms. Parsing kept minimal so the
    // test stays dependency-free; local-noon keeps day-bucketing stable.
    let (y, m, d) = (
        date[..4].parse::<i64>().unwrap_or(2024),
        date[5..7].parse::<i64>().unwrap_or(1),
        date[8..10].parse::<i64>().unwrap_or(1),
    );
    // Days since 1970-01-01 (Gregorian), *86400 + 12h, in ms. Good enough for
    // date-string bucketing (`date_string()` derives local date from timestamp).
    let days = (y - 1970) * 365 + (y - 1969) / 4 - (y - 1901) / 100
        + (y - 1601) / 400
        + [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][m.clamp(1, 12) as usize]
        + d
        - 1;
    days * 86_400_000 + 12 * 3_600_000
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

/// Corpus exercising every order-/tie-break-/drop-sensitive path the maps
/// flagged: >=2 equal-cost models; >=2 clients in a merged bucket with distinct
/// arrival order; same client:model across two providers (provider merge);
/// timestamp<=0 row (hourly fallback); date.len()<7 drop; NaN cost (sanitation).
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

    // A timestamp<=0 row (hourly fallback bucket) with a valid date derived from
    // the timestamp epoch fallback. Build with new() then clear the timestamp.
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

// ---- OLD-path runners (mirror the live aggregators minus parse/pricing) ----

fn old_model_report(msgs: &[UnifiedMessage], gb: &GroupBy) -> ModelReport {
    let entries = crate::aggregate_model_usage_entries_pub(msgs.to_vec(), gb);
    let total_input: i64 = entries.iter().map(|e| e.input).sum();
    let total_output: i64 = entries.iter().map(|e| e.output).sum();
    let total_cache_read: i64 = entries.iter().map(|e| e.cache_read).sum();
    let total_cache_write: i64 = entries.iter().map(|e| e.cache_write).sum();
    let total_messages: i32 = entries.iter().map(|e| e.message_count).sum();
    let total_cost: f64 = entries.iter().map(|e| e.cost).sum();
    ModelReport {
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

// ---- Parity tests ----

#[test]
#[serial]
fn parity_graph_result() {
    pin_tz();
    let msgs = corpus();

    // OLD path — exactly what generate_graph_with_loaded_pricing runs.
    let mut old = {
        let intervals = sessionize::sessionize(&msgs, sessionize::DEFAULT_IDLE_GAP_MS);
        let tm = sessionize::compute_time_metrics(&intervals, sessionize::DEFAULT_IDLE_GAP_MS);
        let dat = sessionize::compute_daily_active_time(&intervals);
        let contribs = aggregator::aggregate_by_date(msgs.clone());
        let mut r = aggregator::generate_graph_result(contribs, 0);
        r.time_metrics = Some(tm);
        for c in &mut r.contributions {
            if let Some(&ms) = dat.get(&c.date) {
                c.active_time_ms = Some(ms);
            }
        }
        r
    };

    // NEW path.
    let mut new = {
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

    // Neutralize non-deterministic meta.
    old.meta.generated_at.clear();
    new.meta.generated_at.clear();
    old.meta.processing_time_ms = 0;
    new.meta.processing_time_ms = 0;

    assert_eq!(
        serde_json::to_string(&old).unwrap(),
        serde_json::to_string(&new).unwrap(),
        "GraphResult byte parity failed",
    );
}

#[test]
#[serial]
fn parity_model_report_all_group_by() {
    pin_tz();
    let msgs = corpus();
    for gb in [
        GroupBy::Model,
        GroupBy::ClientModel,
        GroupBy::ClientProviderModel,
        GroupBy::WorkspaceModel,
        GroupBy::Session,
        GroupBy::ClientSession,
    ] {
        let mut old = old_model_report(&msgs, &gb);
        let mut e = AggregationEngine::new(AggregationConfig {
            group_by: gb.clone(),
            date_range: DateRange::none(),
            views: ViewSet::MODEL,
        });
        for m in &msgs {
            e.push(m);
        }
        let mut new = e.finish().model_report.expect("model view requested");
        neutralize_processing_time(&mut old);
        neutralize_processing_time(&mut new);
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            serde_json::to_string(&new).unwrap(),
            "ModelReport parity failed for {gb:?}",
        );
    }
}

#[test]
#[serial]
fn parity_monthly_report() {
    pin_tz();
    let msgs = corpus();

    // OLD path.
    let old = crate::monthly_report_from_messages_pub(msgs.clone());

    // NEW path.
    let new = {
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
        serde_json::to_string(&old).unwrap(),
        serde_json::to_string(&new).unwrap(),
        "MonthlyReport byte parity failed",
    );
}

#[test]
#[serial]
fn parity_hourly_report() {
    pin_tz();
    let msgs = corpus();

    let old = crate::hourly_report_from_messages_pub(msgs.clone());
    let new = {
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
        serde_json::to_string(&old).unwrap(),
        serde_json::to_string(&new).unwrap(),
        "HourlyReport byte parity failed",
    );
}

#[test]
#[serial]
fn parity_time_metrics() {
    pin_tz();
    let msgs = corpus();

    let intervals = sessionize::sessionize(&msgs, sessionize::DEFAULT_IDLE_GAP_MS);
    let metrics = sessionize::compute_time_metrics(&intervals, sessionize::DEFAULT_IDLE_GAP_MS);
    let old = crate::TimeMetricsReport {
        metrics,
        processing_time_ms: 0,
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
fn parity_session_contributions() {
    let msgs = corpus();
    let old = aggregator::aggregate_by_session(msgs.clone());
    let new = {
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
    // SessionContribution derives PartialEq.
    assert_eq!(old, new, "SessionContribution parity failed");
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
    pin_tz();
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
fn contract_hourly_timestamp_zero_fallback_bucket() {
    // lib.rs:2321 — a timestamp<=0 message lands in "{date} 00:00".
    pin_tz();
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
    assert_eq!(report.entries.len(), 1);
    assert!(report.entries[0].hour.ends_with("00:00"));
}

#[test]
#[serial]
fn contract_monthly_month_asc() {
    // lib.rs:2257 — monthly entries sort by month ASC.
    pin_tz();
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
    pin_tz();
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
