use codspeed_criterion_compat::{black_box, criterion_group, criterion_main, Criterion};
use tokscale_core::{normalize_model_for_grouping, normalize_provider_for_grouping};

fn normalize_models(c: &mut Criterion) {
    let models = [
        "custom:openai/gpt-5.3-codex-high",
        "anthropic/claude-sonnet-4-20250514-thinking",
        "moonshotai/kimi-k2.5-free-high",
        "meituan/longcat-flash-3b-all-quant-int8",
        "openrouter/qwen/qwen3-coder:free",
        "composer-2.5-fast",
    ];

    c.bench_function("normalize_model_for_grouping", |b| {
        b.iter(|| {
            let mut total_len = 0usize;
            for model in black_box(models) {
                total_len += normalize_model_for_grouping(model).len();
            }
            black_box(total_len)
        });
    });
}

fn normalize_providers(c: &mut Criterion) {
    let providers = [
        "anthropic/claude-sonnet-4",
        "moonshotai/kimi-k2.5",
        "github_copilot/gpt-5.3-codex",
        "z-ai/glm-4.6",
        "openai_codex/gpt-5.3",
        "opencode/openrouter/qwen3-coder",
    ];

    c.bench_function("normalize_provider_for_grouping", |b| {
        b.iter(|| {
            let mut total_len = 0usize;
            for provider in black_box(providers) {
                total_len += normalize_provider_for_grouping(provider).len();
            }
            black_box(total_len)
        });
    });
}

criterion_group!(benches, normalize_models, normalize_providers);
criterion_main!(benches);
