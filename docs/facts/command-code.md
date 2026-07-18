# Command Code local-session facts

Verified on 2026-07-18 against the globally installed npm package
`command-code@0.52.1`, its bundled runtime, and the local
`~/.commandcode` corpus. Prompt contents and credentials were not inspected.

## Local storage shape

```text
~/.commandcode/
├── config.json
├── projects/
│   └── <project-slug>/
│       ├── <session-id>.jsonl
│       ├── <session-id>.meta.json
│       └── <session-id>.checkpoints.jsonl
└── file-history/
    └── <session-id>/
```

The verified corpus contains 7 project directories, 11 main session files,
11 matching checkpoint files, and 8 session metadata files.

Each main session JSONL record has this top-level shape:

```text
id
timestamp
sessionId
parentId
role
content
gitBranch
metadata
```

All 311 verified main-session records are valid JSON. The main transcript
schema does not persist a model, provider, or token usage.

Session metadata is stored separately in `<session-id>.meta.json`. Of the 8
verified metadata files, 1 contains `model`; none contains `provider`.

Each checkpoint record has this shape:

```text
type: "file-history-snapshot"
messageId
snapshot:
  messageId
  trackedFileBackups
  timestamp
isSnapshotUpdate
```

The 11 checkpoint files contain 35 valid records. They are file-rewind
sidecars and do not contain model or usage data.

The verified global `config.json` has the keys `firstMessageSent`, `installed`,
`model`, and `provider`. At verification time its identity fields were:

```text
model: Qwen/Qwen3.7-Max-Free
provider: command-code
```

## Current Tokscale model projection

For every discovered main transcript, `COMMANDCODE_ADAPTER` attaches
`~/.commandcode/config.json` as a required cache dependency. The parser then
calls `model_from_config` and assigns the current `config.json.model` to every
estimated assistant usage record in that historical session.

Tokscale does not currently read `<session-id>.meta.json.model`.

Because `config.json` participates in the source fingerprint, changing its
model invalidates the cached projection and reassigns the same unchanged
historical transcript:

```text
Today:
  config.model = claude
  historical session model reported by Tokscale = claude

Tomorrow:
  config.model = gpt-5
  the same historical session model reported by Tokscale = gpt-5
```

The transcript and estimated token counts do not need to change for this model
reassignment to occur.
