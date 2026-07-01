use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion,
};
use tokscale_core::{normalize_model_for_grouping, normalize_provider_for_grouping};

const RAW_MODEL_CASES: &[(&str, &str)] = &[
    ("gpt_custom_tier", "custom:openai/gpt-5.3-codex-high"),
    (
        "claude_anthropic_date",
        "anthropic/claude-sonnet-4-20250514-thinking",
    ),
    ("kimi_free_tier", "moonshotai/kimi-k2.5-free-high"),
    (
        "longcat_quantized",
        "meituan/longcat-flash-3b-all-quant-int8",
    ),
    ("qwen_openrouter_free", "openrouter/qwen/qwen3-coder:free"),
    ("gpt4o_mini_date", "openai/gpt-4o-mini-2024-07-18"),
    ("nemotron_free", "nemotron-3-ultra-free"),
];

const CANONICAL_MODEL_CASES: &[(&str, &str)] = &[
    ("gpt_codex", "gpt-5.3-codex"),
    ("claude_sonnet", "claude-sonnet-4"),
    ("kimi", "kimi-k2.5"),
    ("longcat", "longcat-flash-3b"),
    ("qwen", "qwen3-coder"),
    ("gpt4o_mini", "gpt-4o-mini"),
    ("nemotron", "nemotron-3-ultra"),
];

const PROVIDER_CASES: &[(&str, &str)] = &[
    ("anthropic_provider_path", "anthropic/claude-sonnet-4"),
    ("kimi_provider_path", "moonshotai/kimi-k2.5"),
    ("github_copilot_provider", "github_copilot/gpt-5.3-codex"),
    ("zai_provider_alias", "z-ai/glm-4.6"),
    ("openai_codex_provider", "openai_codex/gpt-5.3"),
    (
        "opencode_openrouter_path",
        "opencode/openrouter/qwen3-coder",
    ),
];

fn canonical_model_id_cleanup(c: &mut Criterion) {
    let mut group = c.benchmark_group("canonical_model_id_cleanup");

    group.bench_function("raw_source_labels_mixed_batch", |b| {
        b.iter(|| {
            let mut total_len = 0usize;
            for (_, model) in black_box(RAW_MODEL_CASES) {
                total_len += normalize_model_for_grouping(black_box(model)).len();
            }
            black_box(total_len)
        });
    });

    group.bench_function("already_canonical_mixed_batch", |b| {
        b.iter(|| {
            let mut total_len = 0usize;
            for (_, model) in black_box(CANONICAL_MODEL_CASES) {
                total_len += normalize_model_for_grouping(black_box(model)).len();
            }
            black_box(total_len)
        });
    });

    for (case_id, model) in RAW_MODEL_CASES {
        group.bench_with_input(BenchmarkId::from_parameter(*case_id), model, |b, model| {
            b.iter(|| black_box(normalize_model_for_grouping(black_box(*model)).len()));
        });
    }

    group.finish();
}

fn normalize_providers(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_provider_for_grouping");

    group.bench_function("mixed_batch", |b| {
        b.iter(|| {
            let mut total_len = 0usize;
            for (_, provider) in black_box(PROVIDER_CASES) {
                total_len += normalize_provider_for_grouping(provider).len();
            }
            black_box(total_len)
        });
    });

    for (case_id, provider) in PROVIDER_CASES {
        group.bench_with_input(
            BenchmarkId::from_parameter(*case_id),
            provider,
            |b, provider| {
                b.iter(|| black_box(normalize_provider_for_grouping(black_box(*provider)).len()));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, canonical_model_id_cleanup, normalize_providers);
criterion_main!(benches);
