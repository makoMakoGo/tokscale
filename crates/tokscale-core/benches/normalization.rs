use codspeed_criterion_compat::{
    black_box, criterion_group, criterion_main, BenchmarkId, Criterion,
};
use tokscale_core::{normalize_model_for_grouping, normalize_provider_for_grouping};

const MODEL_CASES: &[(&str, &str)] = &[
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
    ("unknown_fast_suffix", "composer-2.5-fast"),
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

fn normalize_models(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_model_for_grouping");

    group.bench_function("mixed_batch", |b| {
        b.iter(|| {
            let mut total_len = 0usize;
            for (_, model) in black_box(MODEL_CASES) {
                total_len += normalize_model_for_grouping(*model).len();
            }
            black_box(total_len)
        });
    });

    for (case_id, model) in MODEL_CASES {
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
                total_len += normalize_provider_for_grouping(*provider).len();
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

criterion_group!(benches, normalize_models, normalize_providers);
criterion_main!(benches);
