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

/// Compare two `serde_json::Value`s with the known nondeterministic array
/// orders normalized: `contributions[].clients` (drained from a HashMap in
/// `DayAccumulator::into_contribution`) and `entries[].models` (a HashSet in
/// `MonthAggregator`) are sorted on both sides before comparison. These three
/// sites — daily clients, monthly models, and equal-cost model entries — are
/// the C1.5 BLOCKERs: structurally HashMap/HashSet-order-dependent today.
/// Until C1.5 makes both paths deterministic, the harness normalizes their
/// array order and asserts everything ELSE is byte-identical. After C1.5 these
/// normalizers are removed in favor of strict `serde_json` string equality.
fn assert_json_equal_normalized(left: &str, right: &str) {
    let mut a: serde_json::Value = serde_json::from_str(left).expect("left json");
    let mut b: serde_json::Value = serde_json::from_str(right).expect("right json");
    normalize_nondeterministic_arrays(&mut a);
    normalize_nondeterministic_arrays(&mut b);
    assert_eq!(a, b, "byte parity (nondeterministic arrays normalized) failed");
}

fn normalize_nondeterministic_arrays(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            // MonthlyUsage.models is an unsorted HashSet->Vec (C1.5 BLOCKER).
            if let Some(models) = map.get_mut("models") {
                sort_json_array(models);
            }
            for (_, child) in map.iter_mut() {
                normalize_nondeterministic_arrays(child);
            }
        }
        serde_json::Value::Array(arr) => {
            // DailyContribution.clients is drained from a HashMap unsorted
            // (C1.5 BLOCKER). Sort by the `client` field for comparison.
            if arr.iter().all(is_client_contribution) {
                arr.sort_by(json_client_key);
            }
            for item in arr.iter_mut() {
                normalize_nondeterministic_arrays(item);
            }
        }
        _ => {}
    }
}

fn is_client_contribution(v: &serde_json::Value) -> bool {
    v.get("client").and_then(|c| c.as_str()).is_some()
        && v.get("model_id").and_then(|c| c.as_str()).is_some()
}

fn json_client_key(a: &serde_json::Value, b: &serde_json::Value) -> std::cmp::Ordering {
    let ak = a.get("client").and_then(|c| c.as_str()).unwrap_or("");
    let bk = b.get("client").and_then(|c| c.as_str()).unwrap_or("");
    ak.cmp(bk)
}

fn sort_json_array(v: &mut serde_json::Value) {
    if let serde_json::Value::Array(arr) = v {
        arr.sort_by(|a, b| a.to_string().cmp(&b.to_string()));
    }
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
    let days = (y - 1970) * 365 + (y - 1969) / 4 - (y - 1901) / 100 + (y - 1601) / 400
        + [0, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334][m.clamp(1, 12) as usize]
        + d
        - 1;
    days * 86_400_000 + 12 * 3_600_000
}

fn msg(client: &str, model: &str, provider: &str, session: &str, date: &str, cost: f64) -> UnifiedMessage {
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
        msg("claude", "claude-sonnet-4", "anthropic", "s1", "2024-06-10", 5.0),
        msg("claude", "claude-opus-4", "anthropic", "s1", "2024-06-10", 5.0),
        // Two clients merged under GroupBy::Model, distinct arrival order.
        msg("codex", "gpt-5", "openai", "s2", "2024-06-11", 2.0),
        msg("gemini", "gpt-5", "openai", "s3", "2024-06-11", 8.0),
        // Same client:model across two providers (provider comma-merge + sort/dedup).
        msg("claude", "claude-sonnet-4", "anthropic-bedrock", "s1", "2024-06-12", 1.0),
        // A second month (month sort) + a NaN cost row (sanitation).
        msg("claude", "claude-sonnet-4", "anthropic", "s1", "2024-05-01", f64::NAN),
    ];

    // A timestamp<=0 row (hourly fallback bucket) with a valid date derived from
    // the timestamp epoch fallback. Build with new() then clear the timestamp.
    let mut zero_ts = msg("claude", "claude-sonnet-4", "anthropic", "s1", "1970-01-01", 0.5);
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

    // DailyContribution.clients is drained from a HashMap unsorted (C1.5
    // BLOCKER); compare with those arrays normalized until C1.5.
    assert_json_equal_normalized(
        &serde_json::to_string(&old).unwrap(),
        &serde_json::to_string(&new).unwrap(),
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

    // MonthlyUsage.models is an unsorted HashSet->Vec today (the C1.5
    // BLOCKER); compare with models arrays normalized until C1.5.
    assert_json_equal_normalized(
        &serde_json::to_string(&old).unwrap(),
        &serde_json::to_string(&new).unwrap(),
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
        e.finish().time_metrics.expect("time-metrics view requested")
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
