# Pricing semantics

Tokscale pricing estimates what parsed token buckets would cost under the
configured pricing catalog. It is not an invoice reconciler.

## Local report cost

For normal local reports, parsers emit token usage. App or vendor fields such as
`cost`, `credits`, `cost_usd`, `dollar_float`, `spendCents`,
`estimated_cost_usd`, `actual_cost_usd`, and `usage.cost.total` are ignored.

`UnifiedMessage.cost` is derived by applying Tokscale pricing to these token
buckets:

- input tokens
- output tokens
- cache read tokens
- cache write or cache creation tokens
- reasoning tokens

Rows without positive token buckets are not usage rows. Cost-only or
credits-only records are dropped instead of being converted into local token
cost.

See [ADR 0011](adr/0011-token-derived-local-cost.md).

## Pricing source authority

Pricing lookup uses:

1. Custom pricing overrides from `custom-pricing.json`
2. LiteLLM public pricing data
3. OpenRouter public pricing data
4. models.dev public pricing data
5. deterministic direct catalog matching and normalization

Global private aliases are not a substitute for source parsing. Source-specific
model decoding belongs in the parser or source canonicalizer before pricing.

If no pricing match exists, derived cost stays `$0.00`. The unresolved model id
should remain visible so the missing catalog entry can be fixed explicitly.

See [ADR 0013](adr/0013-pricing-source-authority.md).

## Custom pricing overrides

Create `custom-pricing.json` in the Tokscale config directory:

```json
{
  "$schema": "https://tokscale.ai/custom-pricing.schema.json",
  "models": {
    "accounts/fireworks/routers/kimi-k2p6-turbo": {
      "input_cost_per_million_tokens": 2.0,
      "output_cost_per_million_tokens": 8.0,
      "cache_read_input_token_cost_per_million_tokens": 0.3,
      "source": "https://docs.fireworks.ai/serverless/pricing",
      "notes": "Fireworks Kimi K2.6 Turbo preview"
    }
  }
}
```

Per-million-token fields are the recommended user-facing form. At least one of
`input_cost_per_million_tokens` or `output_cost_per_million_tokens` must be
present and positive. Cache-read and cache-creation prices are optional.

Overrides are exact-only and case-insensitive:

- Local reports match the canonical model id emitted by the source parser.
- `tokscale pricing <model>` matches the command argument as a catalog query.
- Gateway paths must be written as full keys when you want that exact route.

Restart the command after editing the file because overrides are loaded at
startup.

## Cache files

Pricing data is cached under `${TOKSCALE_CONFIG_DIR}/cache/`:

- `pricing-litellm.json`
- `pricing-openrouter.json`
- `pricing-models-dev.json`

Deleting these files forces Tokscale to fetch pricing data again on the next
lookup or report that needs pricing.

## Standalone lookup

```bash
tokscale pricing claude-sonnet-4-5 --no-spinner
tokscale pricing grok-code --provider openrouter --no-spinner
tokscale pricing list-overrides --json
```

Standalone lookup does not infer arbitrary source prefixes, route prefixes,
private aliases, or reasoning-tier suffixes. It is a pricing catalog query, not
a parser repair path.

## Subscription usage is separate

Subscription quota commands call provider-specific quota endpoints and show what
the provider reports. Those numbers are not mixed into normal local token
reports.
