use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
use std::sync::OnceLock;
use tokscale_core::{
    aggregate_unified_messages, AggregationConfig, DateRange, GroupBy, TokenBreakdown,
    UnifiedMessage, ViewSet,
};

const MESSAGE_COUNT: usize = 100_000;

const CLIENTS: &[&str] = &["opencode", "claude", "codex", "zed"];
const MODELS: &[&str] = &["gpt-5.5", "claude-sonnet-4.5", "qwen3-coder", "kimi-k2.5"];
const PROVIDERS: &[&str] = &["openai", "anthropic", "qwen", "kimi"];
const WORKSPACES: &[&str] = &[
    "/repo/tokscale",
    "/repo/tokscale-cli",
    "/repo/tokscale-core",
    "/repo/client-work",
    "/repo/bench",
];
const AGENTS: &[&str] = &["Sisyphus", "Planner-Sisyphus", "reviewer", "implementer"];

#[derive(Clone, Copy, Debug)]
enum Cardinality {
    Low,
    High,
}

fn synthetic_messages(cardinality: Cardinality) -> Vec<UnifiedMessage> {
    let mut messages = Vec::with_capacity(MESSAGE_COUNT);
    let base_timestamp = 1_735_689_600_000i64;

    for index in 0..MESSAGE_COUNT {
        let client = CLIENTS[index % CLIENTS.len()];
        let model = MODELS[(index / 3) % MODELS.len()];
        let provider = PROVIDERS[(index / 7) % PROVIDERS.len()];
        let session_id = match cardinality {
            Cardinality::Low => format!("session-{}", index % 8_192),
            Cardinality::High => format!("session-{index}"),
        };
        let timestamp = base_timestamp + (index as i64 * 60_000);
        let input = 80 + (index % 2048) as i64;
        let output = 20 + (index % 512) as i64;
        let cache_read = (index % 1024) as i64;
        let cache_write = (index % 128) as i64;
        let reasoning = (index % 64) as i64;
        let cost = (input + output + cache_read + cache_write + reasoning) as f64 * 0.000_001;

        let mut message = UnifiedMessage::new_with_agent(
            client,
            model,
            provider,
            session_id,
            timestamp,
            TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            },
            cost,
            Some(AGENTS[index % AGENTS.len()].to_string()),
        );

        match cardinality {
            Cardinality::Low => {
                let workspace = WORKSPACES[index % WORKSPACES.len()];
                message.set_workspace(Some(workspace.to_string()), Some(workspace.to_string()));
            }
            Cardinality::High => {
                let workspace = format!("/repo/unique-{index}");
                message.set_workspace(Some(workspace.clone()), Some(workspace));
            }
        }
        message.duration_ms = Some(500 + (index % 15_000) as i64);
        message.message_count = 1 + (index % 3) as i32;
        message.is_turn_start = index % 2 == 0;
        messages.push(message);
    }

    messages
}

fn benchmark_messages(cardinality: Cardinality) -> &'static [UnifiedMessage] {
    static LOW: OnceLock<Vec<UnifiedMessage>> = OnceLock::new();
    static HIGH: OnceLock<Vec<UnifiedMessage>> = OnceLock::new();
    match cardinality {
        Cardinality::Low => LOW.get_or_init(|| synthetic_messages(Cardinality::Low)),
        Cardinality::High => HIGH.get_or_init(|| synthetic_messages(Cardinality::High)),
    }
}

fn push_and_finish(messages: &[UnifiedMessage], views: ViewSet, group_by: GroupBy) -> usize {
    let views = aggregate_unified_messages(
        black_box(messages),
        AggregationConfig {
            group_by,
            date_range: DateRange::none(),
            views,
        },
    );

    let mut rows = 0usize;
    rows += views
        .tui_usage
        .as_ref()
        .map(|usage| {
            let graph_days = usage
                .graph
                .as_ref()
                .map(|graph| {
                    graph
                        .weeks
                        .iter()
                        .flatten()
                        .filter(|day| day.is_some())
                        .count()
                })
                .unwrap_or(0);
            usage.models.len()
                + usage.agents.len()
                + usage.daily.len()
                + usage.hourly.len()
                + graph_days
        })
        .unwrap_or(0);
    rows += views
        .model_report
        .as_ref()
        .map(|report| report.entries.len())
        .unwrap_or(0);
    rows += views
        .monthly_report
        .as_ref()
        .map(|report| report.entries.len())
        .unwrap_or(0);
    rows += views
        .hourly_report
        .as_ref()
        .map(|report| report.entries.len())
        .unwrap_or(0);
    rows += views
        .graph
        .as_ref()
        .map(|graph| graph.contributions.len())
        .unwrap_or(0);
    rows += views
        .session_contributions
        .as_ref()
        .map(|sessions| sessions.len())
        .unwrap_or(0);
    rows += views
        .agent_usage
        .as_ref()
        .map(|agents| agents.len())
        .unwrap_or(0);
    if views.time_metrics.is_some() {
        rows += 1;
    }
    rows
}

fn bench_aggregation_engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("aggregation_engine_push_finish");
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));

    let cases = [
        (
            "tui_client_model",
            ViewSet::TUI,
            GroupBy::ClientModel,
            Cardinality::Low,
        ),
        (
            "tui_workspace_model",
            ViewSet::TUI,
            GroupBy::WorkspaceModel,
            Cardinality::Low,
        ),
        (
            "model_only",
            ViewSet::MODEL,
            GroupBy::ClientProviderModel,
            Cardinality::Low,
        ),
        (
            "workspace_model",
            ViewSet::MODEL,
            GroupBy::WorkspaceModel,
            Cardinality::Low,
        ),
        (
            "monthly_hourly",
            ViewSet::MONTHLY | ViewSet::HOURLY,
            GroupBy::ClientModel,
            Cardinality::Low,
        ),
        (
            "graph_sessions_time",
            ViewSet::GRAPH | ViewSet::SESSIONS | ViewSet::TIME_METRICS,
            GroupBy::ClientModel,
            Cardinality::Low,
        ),
        (
            "all_views",
            ViewSet::TUI
                | ViewSet::MODEL
                | ViewSet::MONTHLY
                | ViewSet::HOURLY
                | ViewSet::GRAPH
                | ViewSet::SESSIONS
                | ViewSet::TIME_METRICS
                | ViewSet::AGENTS,
            GroupBy::ClientProviderModel,
            Cardinality::Low,
        ),
        (
            "tui_session_high_cardinality",
            ViewSet::TUI,
            GroupBy::Session,
            Cardinality::High,
        ),
        (
            "tui_workspace_high_cardinality",
            ViewSet::TUI,
            GroupBy::WorkspaceModel,
            Cardinality::High,
        ),
        (
            "model_session_high_cardinality",
            ViewSet::MODEL,
            GroupBy::Session,
            Cardinality::High,
        ),
        (
            "model_workspace_high_cardinality",
            ViewSet::MODEL,
            GroupBy::WorkspaceModel,
            Cardinality::High,
        ),
    ];

    for (name, views, group_by, cardinality) in cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &(views, group_by, cardinality),
            |b, (views, group_by, cardinality)| {
                let messages = benchmark_messages(*cardinality);
                b.iter(|| black_box(push_and_finish(messages, *views, group_by.clone())));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_aggregation_engine);
criterion_main!(benches);
