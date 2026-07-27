# Droid local-session facts

Last verified: 2026-07-18

This document records observed Factory Droid storage and runtime semantics, not
Tokenx policy. The corresponding ingestion decision lives in
[ADR 0001](../adr/0001-no-silent-fallback.md).
Verification used the locally installed Droid `0.174.0` Linux executable and a
read-only snapshot of the local `~/.factory` corpus. No credentials, prompts,
message content, or tool payloads were inspected or copied.

## `provider` is an API protocol selector

Droid custom models are configured with independent model, endpoint, and
protocol fields. A sanitized real entry had this shape:

```json
{
  "model": "glm-5.2",
  "id": "custom:glm-5.2",
  "baseUrl": "https://api.z.ai/api/anthropic",
  "displayName": "GLM-5.2 [Z.AI Coding Plan]",
  "provider": "anthropic"
}
```

The omitted `apiKey` was not inspected. This tuple cannot mean that Anthropic
owns the model: the model and display name identify GLM/Z.AI, while the URL
selects Z.AI's Anthropic-compatible endpoint.

The bundled Droid source confirms that interpretation:

- the custom-model schema stores `model`, `baseUrl`, `apiKey`, and `provider`
  separately;
- its supported custom provider values include `anthropic`, `openai`,
  `generic-chat-completion-api`, and `bedrock-converse`;
- its own BYOK diagnostics describe `provider` as the type that must match the
  configured endpoint;
- custom-model resolution copies this field into the runtime `modelProvider`;
- `modelProvider == "anthropic"` selects the Anthropic message conversion and
  request client while still using the custom model's own `baseUrl`; and
- `modelProvider == "openai"` or `generic-chat-completion-api` selects the
  corresponding OpenAI-style request path.

Accordingly, Droid's `provider`/`modelProvider` names an API adapter or wire
protocol family. It is not reliable model-vendor ownership metadata.

## How `providerLock` is persisted

When a session begins sending with a resolved model, Droid passes that runtime
`modelProvider` to `setLockedModelProviderOnce`. The method writes the value to
the session settings as `providerLock` with a `providerLockTimestamp`.

For a custom model, the chain is therefore:

```text
customModels[].provider = "anthropic"
        |
        v
runtime modelProvider = "anthropic"
        |
        +-- select Anthropic-compatible request path + custom baseUrl
        |
        `-- session providerLock = "anthropic"
```

`providerLock` preserves the request-protocol family across a session. It does
not snapshot the custom model definition, base URL, configured display name, or
the upstream company that owns the model.

Droid also has a distinct `apiProviderLock`. That field selects concrete
backend routing for built-in/proxied models, with values such as `fireworks`,
`baseten`, `openai`, and `bedrock_anthropic`. Droid clears that lock for custom
models. Neither field turns a protocol label into model ownership.

## Local session shape

Current session usage is stored under project directories as a settings file
paired with a transcript:

```text
~/.factory/sessions/<project-key>/
+-- <session-id>.settings.json
`-- <session-id>.jsonl
```

The settings file carries the aggregate token counters and model identity:

```json
{
  "model": "custom:glm-5.1",
  "providerLock": "anthropic",
  "providerLockTimestamp": "2026-05-18T13:23:16.279Z",
  "tokenUsage": {
    "inputTokens": 3340968,
    "outputTokens": 22620,
    "cacheReadTokens": 3626624,
    "cacheCreationTokens": 0,
    "thinkingTokens": 0
  }
}
```

The verified local corpus contained 286 settings files: 244 with
`providerLock = "openai"`, two with `providerLock = "anthropic"`, and 40 with
no lock. Both Anthropic-locked sessions used `model = "custom:glm-5.1"` and had
positive token usage. Their paired transcripts contained no model, provider,
endpoint, API, or URL field; those transcripts therefore cannot enrich or
contradict the settings identity.

The current custom-model config contains `custom:glm-5.2`, not the historical
`custom:glm-5.1` entry. Since Droid does not copy the custom model definition
into the session, current config must not be used as proof of a historical
session's endpoint. The persisted facts remain the raw custom model ID,
protocol lock, timestamp, and token totals.

## Tokenx interpretation boundary

Tokenx may retain `providerLock` as a routing label while parsing, but
must not present it as authoritative model ownership. Final report
canonicalization can combine the normalized model ID with that label; for the
verified `custom:glm-5.1` sessions, this yields model `glm-5.1`, provider `zai`,
and keeps all recorded tokens. Missing or unfamiliar protocol labels must not
make an otherwise valid model/token record ineligible.

## Evidence index

The installed npm package resolves to
`@factory/cli-linux-x64` and contains an unstripped Bun ELF executable. Its
`.bun` section was extracted read-only and inspected for the bundled schemas and
request dispatch. The inspected executable's SHA-256 was
`be125705bc08ed5ef6b59257b5ca484739b76685cb50eaca05354f413eb8e74f`.
Relevant embedded source modules include:

- `packages/common/src/settings/schema.ts` and settings parsing code —
  custom-model fields and provider validation;
- `src/utils/modelResolution.ts` — custom model to runtime `modelProvider`;
- `src/services/SessionService.ts` — `providerLock` persistence;
- `src/hooks/useLLMStreaming.ts` — protocol-specific request dispatch;
- `src/llm-proxy/getModelProviderInfo.ts` and
  `src/utils/providerLocking.ts` — model-provider and API-provider routing.

These names and minified implementation details are version-specific and must
be reverified when Droid changes its storage or request contracts.
