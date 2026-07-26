use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use std::collections::HashSet;
use std::sync::OnceLock;
use tokenx_engine::{
    build_usage_index, AttributedUsageRecord, CalendarContext, ClientId, DateRange, GroupBy,
    TokenBreakdown,
};

const MESSAGE_COUNT: usize = 100_000;

fn effective_date() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap()
}

fn calendar() -> CalendarContext {
    CalendarContext::explicit("UTC").unwrap()
}

const CLIENTS: &[ClientId] = &[
    ClientId::OpenCode,
    ClientId::Claude,
    ClientId::Codex,
    ClientId::Zed,
];
const MODELS: &[&str] = &["gpt-5.5", "claude-sonnet-4.5", "qwen3-coder", "kimi-k2.5"];
const PROVIDERS: &[&str] = &["openai", "anthropic", "qwen", "kimi"];
const WORKSPACES: &[&str] = &[
    "/repo/tokenx",
    "/repo/tokenx",
    "/repo/tokenx-engine",
    "/repo/client-work",
    "/repo/bench",
];
const AGENTS: &[&str] = &["Sisyphus", "Planner-Sisyphus", "reviewer", "implementer"];

#[derive(Clone, Copy, Debug)]
enum Cardinality {
    Low,
    High,
}

fn synthetic_messages(cardinality: Cardinality) -> Vec<AttributedUsageRecord> {
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

        let mut message = AttributedUsageRecord::new_with_agent(
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
        message.message_count = 1 + (index % 3) as i32;
        message.is_turn_start = index % 2 == 0;
        messages.push(message);
    }

    messages
}

fn benchmark_messages(cardinality: Cardinality) -> &'static [AttributedUsageRecord] {
    static LOW: OnceLock<Vec<AttributedUsageRecord>> = OnceLock::new();
    static HIGH: OnceLock<Vec<AttributedUsageRecord>> = OnceLock::new();
    match cardinality {
        Cardinality::Low => LOW.get_or_init(|| synthetic_messages(Cardinality::Low)),
        Cardinality::High => HIGH.get_or_init(|| synthetic_messages(Cardinality::High)),
    }
}

/// Synthetic production-shaped corpus: 100,000 messages, 20 per session, and
/// 5,000 repeated canonical fine keys. This is deterministic benchmark data,
/// not a sample of production usage.
fn synthetic_production_shaped_messages() -> Vec<AttributedUsageRecord> {
    const MESSAGES_PER_SESSION: usize = 20;

    let mut messages = Vec::with_capacity(MESSAGE_COUNT);
    let base_timestamp = 1_735_689_600_000i64;

    for index in 0..MESSAGE_COUNT {
        let session_index = index / MESSAGES_PER_SESSION;
        let message_index = index % MESSAGES_PER_SESSION;
        let client = CLIENTS[session_index % CLIENTS.len()];
        let model_index = (session_index / CLIENTS.len()) % MODELS.len();
        let model = MODELS[model_index];
        let provider = PROVIDERS[model_index];
        let workspace = WORKSPACES[(session_index / 3) % WORKSPACES.len()];
        let timestamp =
            base_timestamp + (session_index as i64 * 30 * 60_000) + (message_index as i64 * 60_000);
        let input = 80 + (message_index % 32) as i64;
        let output = 20 + (message_index % 16) as i64;
        let cache_read = (message_index % 8) as i64;
        let cache_write = (message_index % 4) as i64;
        let reasoning = (message_index % 3) as i64;
        let cost = (input + output + cache_read + cache_write + reasoning) as f64 * 0.000_001;

        let mut message = AttributedUsageRecord::new_with_agent(
            client,
            model,
            provider,
            format!("session-{session_index}"),
            timestamp,
            TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            },
            cost,
            Some(AGENTS[session_index % AGENTS.len()].to_string()),
        );
        message.set_workspace(Some(workspace.to_string()), Some(workspace.to_string()));
        message.message_count = 1;
        message.is_turn_start = message_index.is_multiple_of(2);
        messages.push(message);
    }

    messages
}

fn production_shaped_messages() -> &'static [AttributedUsageRecord] {
    static MESSAGES: OnceLock<Vec<AttributedUsageRecord>> = OnceLock::new();
    MESSAGES.get_or_init(synthetic_production_shaped_messages)
}

/// Deterministic 100,000-message corpus where each WorkspaceModel group merges
/// exactly two canonical fine keys. Pair members differ by session while sharing
/// workspace, model, client, provider, timestamp, and local day.
fn synthetic_pair_heavy_messages() -> Vec<AttributedUsageRecord> {
    const FINE_KEYS_PER_GROUP: usize = 2;

    debug_assert_eq!(MESSAGE_COUNT % FINE_KEYS_PER_GROUP, 0);
    let mut messages = Vec::with_capacity(MESSAGE_COUNT);
    let base_timestamp = 1_735_689_600_000i64;

    for index in 0..MESSAGE_COUNT {
        let group_index = index / FINE_KEYS_PER_GROUP;
        let fine_key_index = index % FINE_KEYS_PER_GROUP;
        let client = CLIENTS[group_index % CLIENTS.len()];
        let model_index = (group_index / CLIENTS.len()) % MODELS.len();
        let model = MODELS[model_index];
        let provider = PROVIDERS[model_index];
        let workspace = format!("/repo/pair-{group_index}");
        let timestamp = base_timestamp + group_index as i64 * 60_000;
        let input = 80 + (index % 2048) as i64;
        let output = 20 + (index % 512) as i64;
        let cache_read = (index % 1024) as i64;
        let cache_write = (index % 128) as i64;
        let reasoning = (index % 64) as i64;
        let cost = (input + output + cache_read + cache_write + reasoning) as f64 * 0.000_001;

        let mut message = AttributedUsageRecord::new_with_agent(
            client,
            model,
            provider,
            format!("pair-{group_index}-session-{fine_key_index}"),
            timestamp,
            TokenBreakdown {
                input,
                output,
                cache_read,
                cache_write,
                reasoning,
            },
            cost,
            Some(AGENTS[group_index % AGENTS.len()].to_string()),
        );
        message.set_workspace(Some(workspace.clone()), Some(workspace));
        message.message_count = 1;
        message.is_turn_start = fine_key_index == 0;
        messages.push(message);
    }

    messages
}

fn pair_heavy_messages() -> &'static [AttributedUsageRecord] {
    static MESSAGES: OnceLock<Vec<AttributedUsageRecord>> = OnceLock::new();
    MESSAGES.get_or_init(synthetic_pair_heavy_messages)
}

fn push_finish_and_project(messages: &[AttributedUsageRecord], group_by: GroupBy) -> usize {
    let usage = build_usage_index(black_box(messages), DateRange::none(), calendar())
        .unwrap()
        .project_usage(&group_by, effective_date())
        .unwrap();

    let graph_days = usage
        .graph
        .weeks
        .iter()
        .flatten()
        .filter(|day| day.is_some())
        .count();
    usage.models.len() + usage.agents.len() + usage.daily.len() + usage.hourly.len() + graph_days
}

fn bench_generation_accumulator(c: &mut Criterion) {
    let mut group = c.benchmark_group("generation_accumulator_push_finish_project");
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));

    let cases = [
        ("tui_client_model", GroupBy::ClientModel, Cardinality::Low),
        ("tui_model", GroupBy::Model, Cardinality::Low),
        (
            "tui_workspace_model",
            GroupBy::WorkspaceModel,
            Cardinality::Low,
        ),
        (
            "tui_client_provider_model",
            GroupBy::ClientProviderModel,
            Cardinality::Low,
        ),
        (
            "tui_model_high_session_cardinality",
            GroupBy::Model,
            Cardinality::High,
        ),
        (
            "tui_workspace_high_cardinality",
            GroupBy::WorkspaceModel,
            Cardinality::High,
        ),
    ];

    for (name, group_by, cardinality) in cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &(group_by, cardinality),
            |b, (group_by, cardinality)| {
                let messages = benchmark_messages(*cardinality);
                b.iter(|| black_box(push_finish_and_project(messages, *group_by)));
            },
        );
    }

    group.finish();
}

fn bench_frozen_usage_index_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("frozen_usage_index_build");
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));

    let cases = [
        (
            "production_shaped_repeated_fine_keys_100k",
            production_shaped_messages(),
        ),
        (
            "high_unique_fine_keys_100k",
            benchmark_messages(Cardinality::High),
        ),
    ];

    for (name, messages) in cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &messages,
            |b, messages| {
                b.iter_batched(
                    || (),
                    |_| build_usage_index(black_box(*messages), DateRange::none(), calendar()),
                    BatchSize::PerIteration,
                );
            },
        );
    }

    let messages = production_shaped_messages();
    let january = DateRange::bounded(
        Some(chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()),
        Some(chrono::NaiveDate::from_ymd_opt(2025, 1, 31).unwrap()),
    )
    .unwrap();
    group.bench_function("production_shaped_typed_date_filter_100k", |b| {
        b.iter_batched(
            || january.clone(),
            |date_range| build_usage_index(black_box(messages), date_range, calendar()),
            BatchSize::PerIteration,
        );
    });

    group.finish();
}

fn bench_frozen_usage_index_project_usage(c: &mut Criterion) {
    let mut group = c.benchmark_group("frozen_usage_index_project_usage");
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));

    let low_messages = benchmark_messages(Cardinality::Low);
    let high_messages = benchmark_messages(Cardinality::High);
    let cases = [
        ("low_client_model", low_messages, GroupBy::ClientModel),
        ("low_workspace_model", low_messages, GroupBy::WorkspaceModel),
        (
            "low_client_provider_model",
            low_messages,
            GroupBy::ClientProviderModel,
        ),
        ("high_model", high_messages, GroupBy::Model),
        (
            "high_workspace_model",
            high_messages,
            GroupBy::WorkspaceModel,
        ),
        (
            "production_shaped_initial_model",
            production_shaped_messages(),
            GroupBy::Model,
        ),
        (
            "pair_heavy_workspace_model",
            pair_heavy_messages(),
            GroupBy::WorkspaceModel,
        ),
    ];

    for (name, messages, group_by) in cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &(messages, group_by),
            |b, (messages, group_by)| {
                let frozen_index =
                    build_usage_index(messages, DateRange::none(), calendar()).unwrap();
                b.iter_batched(
                    || (),
                    |_| frozen_index.project_usage(black_box(group_by), effective_date()),
                    BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

fn bench_frozen_usage_index_project_usage_for_clients(c: &mut Criterion) {
    let mut group = c.benchmark_group("frozen_usage_index_project_usage_for_clients");
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));

    let single_client = HashSet::from([ClientId::Claude]);
    let multi_client = HashSet::from([ClientId::Claude, ClientId::Codex]);
    let cases = [
        ("production_shaped_single_client_model_100k", &single_client),
        ("production_shaped_multi_client_model_100k", &multi_client),
    ];
    let messages = production_shaped_messages();

    for (name, selected) in cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &selected,
            |b, selected| {
                let frozen_index =
                    build_usage_index(messages, DateRange::none(), calendar()).unwrap();
                b.iter_batched(
                    || (),
                    |_| {
                        frozen_index.project_usage_for_clients(
                            &GroupBy::Model,
                            black_box(selected),
                            effective_date(),
                        )
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

fn bench_frozen_usage_index_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("frozen_usage_index_lifecycle");
    group.throughput(Throughput::Elements(MESSAGE_COUNT as u64));

    let no_switches: [GroupBy; 0] = [];
    let one_switch = [GroupBy::WorkspaceModel];
    let two_switches = [GroupBy::WorkspaceModel, GroupBy::ClientProviderModel];
    let three_switches = [
        GroupBy::ClientModel,
        GroupBy::WorkspaceModel,
        GroupBy::ClientProviderModel,
    ];
    let cases: [(&str, &[GroupBy]); 4] = [
        ("build_plus_initial_model_projection", &no_switches),
        (
            "build_plus_initial_model_plus_1_switch_to_workspace_model",
            &one_switch,
        ),
        (
            "build_plus_initial_model_plus_2_switches_to_workspace_then_client_provider",
            &two_switches,
        ),
        (
            "build_plus_initial_model_plus_3_public_grouping_switches",
            &three_switches,
        ),
    ];
    let messages = production_shaped_messages();

    for (name, switches) in cases {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &switches,
            |b, switches| {
                b.iter_batched(
                    || (),
                    |_| {
                        let frozen_index =
                            build_usage_index(black_box(messages), DateRange::none(), calendar())
                                .unwrap();
                        let mut projections = Vec::with_capacity(switches.len() + 1);
                        projections
                            .push(frozen_index.project_usage(&GroupBy::Model, effective_date()));
                        for group_by in *switches {
                            projections
                                .push(frozen_index.project_usage(group_by, effective_date()));
                        }
                        (frozen_index, projections)
                    },
                    BatchSize::PerIteration,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_generation_accumulator,
    bench_frozen_usage_index_build,
    bench_frozen_usage_index_project_usage,
    bench_frozen_usage_index_project_usage_for_clients,
    bench_frozen_usage_index_lifecycle
);
criterion_main!(benches);
