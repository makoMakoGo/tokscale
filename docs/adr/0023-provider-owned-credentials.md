# ADR 0023: Provider-owned credentials

Status: Accepted

The Cursor subsection is superseded by ADR 0024: Cursor support has been
removed and the broader Subscription Usage redesign now owns that product
surface. The Codex credential boundary remains current.

## Context

Tokscale reports local token usage and optional subscription quota data. Some
upstream integrations expanded that role into account management:

- Cursor asked users to paste a browser `WorkosCursorSessionToken`, copied it
  into `~/.config/tokscale/cursor-credentials.json`, managed multiple accounts,
  and made implicit Cursor API requests before local reports and the TUI.
- Codex copied the complete OAuth token set from the provider-owned
  `auth.json` into `~/.config/tokscale/codex-credentials.json`, then exposed
  import, switch, remove, and multi-account status commands.

Neither credential store is required to parse local data. The extra copies
increase the secret-bearing surface, blur ownership, and let an analytics tool
modify account state. File mode `0600` reduces exposure but does not justify a
second plaintext copy of an access token, refresh token, ID token, or browser
session cookie.

## Decision

Tokscale is a credential consumer, not an account manager.

- Authentication remains owned by the provider CLI, provider application, OS
  keychain, or an explicitly named environment variable.
- Tokscale does not copy provider credentials into its configuration or cache
  directories.
- Tokscale does not switch provider accounts, implement provider login/logout,
  or refresh and rewrite provider OAuth credentials.
- A cached quota or usage result may be persisted only when it contains no
  credential or raw authentication response.
- Missing, expired, or rejected credentials are reported explicitly. Users
  repair authentication with the provider's own tool.

### Cursor

The `tokscale cursor` account-management and API-sync namespace is removed.
Local reports may still parse previously supplied Cursor usage CSV files under
`~/.config/tokscale/cursor-cache/usage*.csv`, because those files contain usage
data rather than credentials. Reports and the TUI never use a Tokscale-owned
Cursor credential file and never contact Cursor implicitly.

Legacy `cursor-credentials.json` files are ignored. Tokscale does not silently
delete user files during startup; users may remove the obsolete file after
upgrading.

### Codex and ChatGPT subscription usage

Codex quota lookup reads only the current provider-owned authentication artifact:

1. `$CODEX_HOME/auth.json` when `CODEX_HOME` is explicitly set;
2. `~/.config/codex/auth.json`;
3. `~/.codex/auth.json`;
4. the provider's macOS Keychain entry.

Only the access token and account id required for the usage request are
deserialized. Tokscale does not deserialize the refresh token or ID token,
does not refresh OAuth, and never writes an auth file. The `tokscale codex`
multi-account namespace and `codex-credentials.json` store are removed.

Legacy `codex-credentials.json` files are ignored and may be removed after the
provider-owned authentication has been verified.

### Subscription Usage redesign

ADR 0014 continues to govern when remote subscription requests may occur.
ADR 0024 defines the subsystem boundary. Multiple Z.ai Coding Plan keys and the
broader account/plan presentation model remain in design under issue #146;
that design must reference external secrets rather than store key values in
Tokscale.

## Consequences

Tokscale no longer offers Cursor or Codex account switching. Cursor API export
does not refresh automatically, and an expired Codex access token requires the
user to authenticate with Codex again. In exchange, local report commands no
longer create, retain, refresh, or mutate these providers' secrets, and command
behavior matches Tokscale's analytics role.
