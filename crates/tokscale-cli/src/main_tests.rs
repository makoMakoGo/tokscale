use super::*;
use crate::commands::cache::*;
use crate::commands::clients::*;
use crate::commands::integrations::*;
use crate::commands::render::*;
use crate::commands::shared::*;
use clap::Parser;
use std::path::PathBuf;

#[test]
fn test_parse_variant_arg_accepts_known_values() {
    assert_eq!(
        parse_variant_arg(Some("solo")).unwrap(),
        Some(trae::auth::TraeVariant::Solo)
    );
    assert_eq!(
        parse_variant_arg(Some("ide")).unwrap(),
        Some(trae::auth::TraeVariant::Ide)
    );
}

#[test]
fn test_parse_variant_arg_none_when_omitted() {
    assert_eq!(parse_variant_arg(None).unwrap(), None);
}

#[test]
fn test_parse_variant_arg_rejects_unknown_value() {
    // The earlier `Option`-returning version converted this to `None`
    // and the caller fell through to "all variants" — a typo like
    // `--variant slo` would log out every variant. Now we error out.
    let err = parse_variant_arg(Some("slo")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown variant"), "got: {msg}");
    assert!(msg.contains("slo"), "got: {msg}");
}

#[test]
fn test_parse_variant_arg_rejects_empty_string() {
    assert!(parse_variant_arg(Some("")).is_err());
}

// Tests below call `build_client_filter_with_defaults` directly with
// an explicit `defaults` slice instead of `build_client_filter`, which
// reads from `~/.config/tokscale/settings.json`.

#[test]
fn test_parse_client_id_arg_accepts_catalog_ids_case_insensitive() {
    assert_eq!(parse_client_id_arg("opencode").unwrap(), ClientId::OpenCode);
    assert_eq!(parse_client_id_arg("OPENCODE").unwrap(), ClientId::OpenCode);
    assert_eq!(parse_client_id_arg("grok").unwrap(), ClientId::Grok);
    assert_eq!(parse_client_id_arg("zcode").unwrap(), ClientId::Zcode);
}

#[test]
fn test_parse_client_id_arg_rejects_unknown_ids() {
    let err = parse_client_id_arg("not-a-client").unwrap_err();
    assert!(err.contains("not-a-client"), "unexpected error: {err}");
    assert!(
        err.contains("opencode"),
        "valid ids missing from error: {err}"
    );
}

#[test]
fn test_build_client_filter_no_flags_no_defaults_returns_none() {
    let flags = ClientFlags::default();
    let defaults: Vec<String> = vec![];
    assert_eq!(
        build_client_filter_with_defaults(flags, &defaults).unwrap(),
        None
    );
}

#[test]
fn test_build_client_filter_canonical_clients_preserve_user_order() {
    let flags = ClientFlags {
        clients: vec![ClientId::Claude, ClientId::OpenCode, ClientId::Pi],
    };
    assert_eq!(
        build_client_filter_with_defaults(flags, &[]).unwrap(),
        Some(vec![
            "claude".to_string(),
            "opencode".to_string(),
            "pi".to_string(),
        ])
    );
}

#[test]
fn test_build_client_filter_canonical_dedups_repeats() {
    let flags = ClientFlags {
        clients: vec![ClientId::Claude, ClientId::Claude, ClientId::OpenCode],
    };
    assert_eq!(
        build_client_filter_with_defaults(flags, &[]).unwrap(),
        Some(vec!["claude".to_string(), "opencode".to_string()])
    );
}

#[test]
fn test_resolve_default_tui_filter_set_uses_configured_defaults() {
    // When `defaultClients` is set, the warm-cache resolver must use
    // it verbatim — otherwise the warm cache would store every real
    // client while the next no-flag TUI launch wants only the configured
    // ones, producing a guaranteed cache miss.
    let configured = vec!["opencode".to_string(), "claude".to_string()];
    let set = resolve_default_tui_filter_set_with(&configured).unwrap();
    let mut expected = std::collections::HashSet::new();
    expected.insert(ClientId::OpenCode);
    expected.insert(ClientId::Claude);
    assert_eq!(set, expected);
}

#[test]
fn test_resolve_default_tui_filter_set_falls_back_when_empty() {
    // No defaultClients configured → use the canonical default set.
    let set = resolve_default_tui_filter_set_with(&[]).unwrap();
    assert_eq!(set, ClientId::iter().collect());
}

#[test]
fn test_resolve_default_tui_filter_set_rejects_unknown_ids() {
    let configured = vec!["opencode".to_string(), "not-a-real-client".to_string()];
    let err = resolve_default_tui_filter_set_with(&configured).unwrap_err();
    assert!(
        err.to_string().contains("not-a-real-client"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_resolve_default_tui_filter_set_rejects_all_unknown_ids() {
    let configured = vec!["not-real".to_string(), "also-fake".to_string()];
    let err = resolve_default_tui_filter_set_with(&configured).unwrap_err();
    assert!(
        err.to_string().contains("not-real, also-fake"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_resolve_default_tui_filter_set_rejects_removed_synthetic_id() {
    let configured = vec!["claude".to_string(), "synthetic".to_string()];
    let err = resolve_default_tui_filter_set_with(&configured).unwrap_err();
    assert!(
        err.to_string().contains("synthetic"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_build_client_filter_with_defaults_when_no_flags() {
    // No CLI flags + a defaultClients list → defaults apply.
    let flags = ClientFlags::default();
    let defaults = vec!["opencode".to_string(), "claude".to_string()];
    assert_eq!(
        build_client_filter_with_defaults(flags, &defaults).unwrap(),
        Some(vec!["opencode".to_string(), "claude".to_string()])
    );
}

#[test]
fn test_build_client_filter_maps_legacy_antigravity_cli_default() {
    let flags = ClientFlags::default();
    let defaults = vec![
        "antigravity-cli".to_string(),
        "antigravity".to_string(),
        "codex".to_string(),
    ];
    assert_eq!(
        build_client_filter_with_defaults(flags, &defaults).unwrap(),
        Some(vec!["antigravity".to_string(), "codex".to_string()])
    );
}

#[test]
fn test_build_client_filter_cli_overrides_defaults_completely() {
    // User passes --client → defaults must be ignored entirely
    // (no merge). This is the predictable semantics: "I asked for X,
    // give me X" not "I asked for X but you also added Y from settings".
    let flags = ClientFlags {
        clients: vec![ClientId::Codex],
    };
    let defaults = vec!["opencode".to_string(), "claude".to_string()];
    assert_eq!(
        build_client_filter_with_defaults(flags, &defaults).unwrap(),
        Some(vec!["codex".to_string()])
    );
}

#[test]
fn test_build_client_filter_defaults_reject_unknown_ids() {
    let flags = ClientFlags::default();
    let defaults = vec!["opencode".to_string(), "not-a-client".to_string()];
    let err = build_client_filter_with_defaults(flags, &defaults).unwrap_err();
    assert!(
        err.to_string().contains("not-a-client"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_build_client_filter_defaults_dedup_preserves_order() {
    let flags = ClientFlags::default();
    let defaults = vec![
        "claude".to_string(),
        "opencode".to_string(),
        "claude".to_string(),
    ];
    assert_eq!(
        build_client_filter_with_defaults(flags, &defaults).unwrap(),
        Some(vec!["claude".to_string(), "opencode".to_string()])
    );
}

#[test]
fn test_client_flags_parses_canonical_form() {
    // End-to-end smoke test: ensure clap derives accept the new
    // `--client a,b` and `-c a -c b` shapes through the CLI parser.
    let cli = Cli::try_parse_from(["tokscale", "--client", "opencode,claude"]).expect("parse ok");
    assert_eq!(
        cli.clients.clients,
        vec![ClientId::OpenCode, ClientId::Claude]
    );

    let cli =
        Cli::try_parse_from(["tokscale", "-c", "opencode", "-c", "claude"]).expect("parse ok");
    assert_eq!(
        cli.clients.clients,
        vec![ClientId::OpenCode, ClientId::Claude]
    );
}

#[test]
fn test_wrapped_parses_clients_view_flag() {
    let cli = Cli::try_parse_from(["tokscale", "wrapped"]).expect("parse ok");
    let Some(Commands::Wrapped {
        show_clients,
        agents,
        ..
    }) = cli.command
    else {
        panic!("expected wrapped command");
    };
    assert!(!show_clients);
    assert!(!agents);

    let cli = Cli::try_parse_from(["tokscale", "wrapped", "--clients"]).expect("parse ok");
    let Some(Commands::Wrapped { show_clients, .. }) = cli.command else {
        panic!("expected wrapped command");
    };
    assert!(show_clients);
}

#[test]
fn test_wrapped_client_filter_coexists_with_clients_view_flag() {
    let cli =
        Cli::try_parse_from(["tokscale", "wrapped", "--client", "opencode"]).expect("parse ok");
    let Some(Commands::Wrapped {
        client_flags,
        show_clients,
        ..
    }) = cli.command
    else {
        panic!("expected wrapped command");
    };
    assert_eq!(client_flags.clients, vec![ClientId::OpenCode]);
    assert!(!show_clients);

    let cli = Cli::try_parse_from(["tokscale", "wrapped", "--clients", "--client", "opencode"])
        .expect("parse ok");
    let Some(Commands::Wrapped {
        client_flags,
        show_clients,
        ..
    }) = cli.command
    else {
        panic!("expected wrapped command");
    };
    assert_eq!(client_flags.clients, vec![ClientId::OpenCode]);
    assert!(show_clients);
}

#[test]
fn test_legacy_client_flags_are_removed() {
    assert!(Cli::try_parse_from(["tokscale", "--claude"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "--opencode"]).is_err());
}

#[test]
fn test_client_flag_accepts_uppercase() {
    let cli = Cli::try_parse_from(["tokscale", "--client", "OPENCODE"]).expect("uppercase parses");
    assert_eq!(cli.clients.clients, vec![ClientId::OpenCode]);

    let cli =
        Cli::try_parse_from(["tokscale", "-c", "Codebuff,Antigravity"]).expect("mixed-case parses");
    assert_eq!(
        cli.clients.clients,
        vec![ClientId::Codebuff, ClientId::Antigravity]
    );
}

#[test]
fn test_client_flag_rejects_unknown_and_empty_values() {
    assert!(Cli::try_parse_from(["tokscale", "--client", "unknown"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "--client", ""]).is_err());
}

#[test]
fn test_build_client_filter_with_defaults_uses_defaults_when_no_flags() {
    let flags = ClientFlags::default();
    let defaults = vec!["opencode".to_string(), "claude".to_string()];
    assert_eq!(
        build_client_filter_with_defaults(flags, &defaults).unwrap(),
        Some(vec!["opencode".to_string(), "claude".to_string()])
    );
}

#[test]
fn test_build_client_filter_with_defaults_empty_defaults_returns_none() {
    let flags = ClientFlags::default();
    assert_eq!(build_client_filter_with_defaults(flags, &[]).unwrap(), None);
}

#[test]
fn test_build_date_filter_custom_range() {
    let (since, until) = build_date_filter(
        false,
        false,
        false,
        Some("2024-01-01".to_string()),
        Some("2024-12-31".to_string()),
    );
    assert_eq!(since, Some("2024-01-01".to_string()));
    assert_eq!(until, Some("2024-12-31".to_string()));
}

#[test]
fn test_build_date_filter_no_filters() {
    let (since, until) = build_date_filter(false, false, false, None, None);
    assert_eq!(since, None);
    assert_eq!(until, None);
}

#[test]
fn test_build_date_filter_today_uses_provided_local_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
    let (since, until) = build_date_filter_for_date(true, false, false, None, None, today);
    assert_eq!(since, Some("2026-03-08".to_string()));
    assert_eq!(until, Some("2026-03-08".to_string()));
}

#[test]
fn test_build_date_filter_week_uses_provided_local_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
    let (since, until) = build_date_filter_for_date(false, true, false, None, None, today);
    assert_eq!(since, Some("2026-03-02".to_string()));
    assert_eq!(until, Some("2026-03-08".to_string()));
}

#[test]
fn test_build_date_filter_month_uses_provided_local_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
    let (since, until) = build_date_filter_for_date(false, false, true, None, None, today);
    assert_eq!(since, Some("2026-03-01".to_string()));
    assert_eq!(until, Some("2026-03-08".to_string()));
}

#[test]
fn test_normalize_year_filter_with_year() {
    let year = normalize_year_filter(false, false, false, Some("2024".to_string()));
    assert_eq!(year, Some("2024".to_string()));
}

#[test]
fn test_normalize_year_filter_with_today() {
    let year = normalize_year_filter(true, false, false, Some("2024".to_string()));
    assert_eq!(year, None);
}

#[test]
fn test_normalize_year_filter_with_week() {
    let year = normalize_year_filter(false, true, false, Some("2024".to_string()));
    assert_eq!(year, None);
}

#[test]
fn test_normalize_year_filter_with_month() {
    let year = normalize_year_filter(false, false, true, Some("2024".to_string()));
    assert_eq!(year, None);
}

#[test]
fn test_normalize_year_filter_no_year() {
    let year = normalize_year_filter(false, false, false, None);
    assert_eq!(year, None);
}

#[test]
fn test_format_tokens_with_commas_small() {
    assert_eq!(format_tokens_with_commas(123), "123");
}

#[test]
fn test_format_tokens_with_commas_thousands() {
    assert_eq!(format_tokens_with_commas(1234), "1,234");
}

#[test]
fn test_format_tokens_with_commas_millions() {
    assert_eq!(format_tokens_with_commas(1234567), "1,234,567");
}

#[test]
fn test_format_tokens_with_commas_billions() {
    assert_eq!(format_tokens_with_commas(1234567890), "1,234,567,890");
}

#[test]
fn test_format_tokens_with_commas_zero() {
    assert_eq!(format_tokens_with_commas(0), "0");
}

#[test]
fn test_format_tokens_with_commas_negative() {
    assert_eq!(format_tokens_with_commas(-1234567), "-1,234,567");
}

#[test]
fn test_format_currency_zero() {
    assert_eq!(format_currency(0.0), "$0.00");
}

#[test]
fn test_format_currency_small() {
    assert_eq!(format_currency(12.34), "$12.34");
}

#[test]
fn test_format_currency_large() {
    assert_eq!(format_currency(1234.56), "$1234.56");
}

#[test]
fn test_format_currency_rounds() {
    assert_eq!(format_currency(12.345), "$12.35");
    assert_eq!(format_currency(12.344), "$12.34");
}

#[test]
fn test_get_date_range_label_today() {
    let label = get_date_range_label(true, false, false, &None, &None, &None);
    assert_eq!(label, Some("Today".to_string()));
}

#[test]
fn test_get_date_range_label_week() {
    let label = get_date_range_label(false, true, false, &None, &None, &None);
    assert_eq!(label, Some("Last 7 days".to_string()));
}

#[test]
fn test_get_date_range_label_month_uses_provided_local_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let label = get_date_range_label_for_date(false, false, true, &None, &None, &None, today);
    assert_eq!(label, Some("March 2026".to_string()));
}

#[test]
fn test_get_date_range_label_year() {
    let label = get_date_range_label(false, false, false, &None, &None, &Some("2024".to_string()));
    assert_eq!(label, Some("2024".to_string()));
}

#[test]
fn test_get_date_range_label_custom_since() {
    let label = get_date_range_label(
        false,
        false,
        false,
        &Some("2024-01-01".to_string()),
        &None,
        &None,
    );
    assert_eq!(label, Some("from 2024-01-01".to_string()));
}

#[test]
fn test_get_date_range_label_custom_until() {
    let label = get_date_range_label(
        false,
        false,
        false,
        &None,
        &Some("2024-12-31".to_string()),
        &None,
    );
    assert_eq!(label, Some("to 2024-12-31".to_string()));
}

#[test]
fn test_get_date_range_label_custom_range() {
    let label = get_date_range_label(
        false,
        false,
        false,
        &Some("2024-01-01".to_string()),
        &Some("2024-12-31".to_string()),
        &None,
    );
    assert_eq!(label, Some("from 2024-01-01 to 2024-12-31".to_string()));
}

#[test]
fn test_get_date_range_label_none() {
    let label = get_date_range_label(false, false, false, &None, &None, &None);
    assert_eq!(label, None);
}

#[test]
fn test_light_spinner_frame_0() {
    let frame = LightSpinner::frame(0);
    assert!(frame.contains("■"));
    assert!(frame.contains("⬝"));
}

#[test]
fn test_light_spinner_frame_1() {
    let frame = LightSpinner::frame(1);
    assert!(frame.contains("■"));
    assert!(frame.contains("⬝"));
}

#[test]
fn test_light_spinner_frame_2() {
    let frame = LightSpinner::frame(2);
    assert!(frame.contains("■"));
    assert!(frame.contains("⬝"));
}

#[test]
fn test_light_spinner_scanner_state_forward_start() {
    let (position, forward) = LightSpinner::scanner_state(0);
    assert_eq!(position, 0);
    assert!(forward);
}

#[test]
fn test_light_spinner_scanner_state_forward_mid() {
    let (position, forward) = LightSpinner::scanner_state(4);
    assert_eq!(position, 4);
    assert!(forward);
}

#[test]
fn test_light_spinner_scanner_state_forward_end() {
    let (position, forward) = LightSpinner::scanner_state(7);
    assert_eq!(position, 7);
    assert!(forward);
}

#[test]
fn test_light_spinner_scanner_state_hold_end() {
    let (position, forward) = LightSpinner::scanner_state(8);
    assert_eq!(position, 7);
    assert!(forward);
}

#[test]
fn test_light_spinner_scanner_state_backward_start() {
    let (position, forward) = LightSpinner::scanner_state(17);
    assert_eq!(position, 6);
    assert!(!forward);
}

#[test]
fn test_light_spinner_scanner_state_backward_end() {
    let (position, forward) = LightSpinner::scanner_state(23);
    assert_eq!(position, 0);
    assert!(!forward);
}

#[test]
fn test_light_spinner_scanner_state_hold_start() {
    let (position, forward) = LightSpinner::scanner_state(24);
    assert_eq!(position, 0);
    assert!(!forward);
}

#[test]
fn test_light_spinner_scanner_state_cycle_wrap() {
    // Total cycle = 8 + 9 + 7 + 30 = 54
    let (position1, forward1) = LightSpinner::scanner_state(0);
    let (position2, forward2) = LightSpinner::scanner_state(54);
    assert_eq!(position1, position2);
    assert_eq!(forward1, forward2);
}

#[test]
#[cfg(target_os = "macos")]
#[serial_test::serial]
fn test_load_star_cache_falls_back_to_legacy_macos_path() {
    // Existing macOS users have star-cache.json at the pre-#468 path under
    // `~/Library/Application Support/tokscale/`. After upgrade, the read
    // path moves to `~/.config/tokscale/`, so without the legacy fallback
    // load_star_cache returns None and the user gets re-prompted to star
    // the repo even though they already starred it.
    use std::env;
    let temp = tempfile::TempDir::new().unwrap();
    let prev_home = env::var_os("HOME");
    let prev_override = env::var_os("TOKSCALE_CONFIG_DIR");
    unsafe {
        env::set_var("HOME", temp.path());
        env::remove_var("TOKSCALE_CONFIG_DIR");
    }

    let legacy_dir = temp.path().join("Library/Application Support/tokscale");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(
        legacy_dir.join("star-cache.json"),
        r#"{"username":"junhoyeo","hasStarred":true,"checkedAt":"2025-01-12T03:48:00Z"}"#,
    )
    .unwrap();

    let new_path = temp.path().join(".config/tokscale/star-cache.json");
    assert!(!new_path.exists());

    let cache = load_star_cache("junhoyeo");
    assert!(
        cache.is_some(),
        "legacy macOS star-cache.json must satisfy load_star_cache after upgrade"
    );
    let cache = cache.unwrap();
    assert_eq!(cache.username, "junhoyeo");
    assert!(cache.has_starred);

    unsafe {
        match prev_home {
            Some(v) => env::set_var("HOME", v),
            None => env::remove_var("HOME"),
        }
        match prev_override {
            Some(v) => env::set_var("TOKSCALE_CONFIG_DIR", v),
            None => env::remove_var("TOKSCALE_CONFIG_DIR"),
        }
    }
}

#[test]
#[cfg(target_os = "macos")]
#[serial_test::serial]
fn test_load_star_cache_skips_legacy_fallback_when_config_dir_overridden() {
    // Same hermeticity contract as the Settings test: TOKSCALE_CONFIG_DIR
    // must isolate the test/CI/sandbox profile from the real user's
    // legacy macOS star-cache.json.
    use std::env;
    let temp = tempfile::TempDir::new().unwrap();
    let legacy_root = tempfile::TempDir::new().unwrap();
    let prev_home = env::var_os("HOME");
    let prev_override = env::var_os("TOKSCALE_CONFIG_DIR");
    unsafe {
        env::set_var("HOME", legacy_root.path());
        env::set_var("TOKSCALE_CONFIG_DIR", temp.path());
    }

    let legacy_dir = legacy_root
        .path()
        .join("Library/Application Support/tokscale");
    std::fs::create_dir_all(&legacy_dir).unwrap();
    std::fs::write(
        legacy_dir.join("star-cache.json"),
        r#"{"username":"junhoyeo","hasStarred":true,"checkedAt":"2025-01-12T03:48:00Z"}"#,
    )
    .unwrap();

    assert!(
        load_star_cache("junhoyeo").is_none(),
        "override must not leak the legacy star-cache hit"
    );

    unsafe {
        match prev_home {
            Some(v) => env::set_var("HOME", v),
            None => env::remove_var("HOME"),
        }
        match prev_override {
            Some(v) => env::set_var("TOKSCALE_CONFIG_DIR", v),
            None => env::remove_var("TOKSCALE_CONFIG_DIR"),
        }
    }
}

#[test]
fn resolve_cli_write_overrides_settings_false() {
    let settings = tui::settings::Settings {
        light: tui::settings::LightSettings { write_cache: false },
        ..tui::settings::Settings::default()
    };
    assert!(resolve_should_write_cache(true, false, &settings));
}

#[test]
fn resolve_cli_no_write_overrides_settings_true() {
    let settings = tui::settings::Settings {
        light: tui::settings::LightSettings { write_cache: true },
        ..tui::settings::Settings::default()
    };
    assert!(!resolve_should_write_cache(false, true, &settings));
}

#[test]
fn resolve_settings_true_with_no_cli_flag() {
    let settings = tui::settings::Settings {
        light: tui::settings::LightSettings { write_cache: true },
        ..tui::settings::Settings::default()
    };
    assert!(resolve_should_write_cache(false, false, &settings));
}

#[test]
fn resolve_settings_false_with_no_cli_flag() {
    let settings = tui::settings::Settings {
        light: tui::settings::LightSettings { write_cache: false },
        ..tui::settings::Settings::default()
    };
    assert!(!resolve_should_write_cache(false, false, &settings));
}

#[test]
fn resolve_settings_default_returns_false() {
    assert!(!resolve_should_write_cache(
        false,
        false,
        &tui::settings::Settings::default()
    ));
}

#[test]
fn clap_rejects_write_cache_without_light() {
    assert!(Cli::try_parse_from(["tokscale", "--write-cache"]).is_err());
}

#[test]
fn clap_rejects_no_write_cache_without_light() {
    assert!(Cli::try_parse_from(["tokscale", "--no-write-cache"]).is_err());
}

#[test]
fn clap_rejects_both_write_flags_together() {
    assert!(
        Cli::try_parse_from(["tokscale", "--light", "--write-cache", "--no-write-cache",]).is_err()
    );
}

#[test]
fn clap_accepts_models_light_write_cache_after_subcommand() {
    assert!(Cli::try_parse_from(["tokscale", "models", "--light", "--write-cache"]).is_ok());
}

#[test]
fn clap_accepts_cursor_sync_command() {
    assert!(Cli::try_parse_from(["tokscale", "cursor", "sync"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "cursor", "sync", "--json"]).is_ok());
}

#[test]
fn clap_accepts_warp_status_and_sync_commands() {
    assert!(Cli::try_parse_from(["tokscale", "warp", "status"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "warp", "status", "--json"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "warp", "sync"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "warp", "sync", "--json"]).is_ok());
}

#[test]
fn clap_accepts_usage_without_light_flag() {
    assert!(Cli::try_parse_from(["tokscale", "usage"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "usage", "--json"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "usage", "--light"]).is_err());
}

#[test]
fn usage_rejects_parent_flags() {
    for flag in USAGE_PARENT_FLAGS {
        let args = usage_parent_flag_test_args(flag.display);
        let matches = Cli::command().try_get_matches_from(args).unwrap();
        let error = reject_usage_parent_flags(&matches)
            .expect_err("usage should reject parent CLI flags")
            .to_string();
        assert!(
            error.contains(flag.display),
            "expected error to mention {} but got {error}",
            flag.display
        );
    }

    let matches = Cli::command()
        .try_get_matches_from(["tokscale", "usage", "--json"])
        .unwrap();
    assert!(reject_usage_parent_flags(&matches).is_ok());
}

fn usage_parent_flag_test_args(flag: &'static str) -> Vec<&'static str> {
    match flag {
        "--write-cache" | "--no-write-cache" => vec!["tokscale", "--light", flag, "usage"],
        "--client" => vec!["tokscale", flag, "claude", "usage"],
        "--since" => vec!["tokscale", flag, "2026-01-01", "usage"],
        "--until" => vec!["tokscale", flag, "2026-01-02", "usage"],
        "--year" => vec!["tokscale", flag, "2026", "usage"],
        "--group-by" => vec!["tokscale", flag, "model", "usage"],
        "--theme" => vec!["tokscale", flag, "red", "usage"],
        "--refresh" => vec!["tokscale", flag, "1", "usage"],
        _ => vec!["tokscale", flag, "usage"],
    }
}

#[test]
fn client_id_parses_warp() {
    assert_eq!(ClientId::from_str("warp"), Some(ClientId::Warp));
    assert_eq!(ClientId::Warp.as_str(), "warp");
}

#[test]
fn client_id_parses_grok() {
    assert_eq!(ClientId::from_str("grok"), Some(ClientId::Grok));
    assert_eq!(ClientId::Grok.as_str(), "grok");
}

#[test]
fn clap_rejects_antigravity_cli_as_separate_client() {
    assert!(Cli::try_parse_from(["tokscale", "--client", "antigravity"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "--client", "antigravity-cli"]).is_err());
}

#[test]
fn antigravity_cli_conversations_path_uses_home_when_env_roots_disabled() {
    assert_eq!(
        antigravity_cli_conversations_path("/tmp/home", false),
        PathBuf::from("/tmp/home/.gemini/antigravity-cli/conversations")
    );
}

#[test]
#[serial_test::serial]
fn antigravity_cli_conversations_path_falls_back_for_blank_env() {
    let previous = std::env::var("GEMINI_CLI_HOME").ok();
    unsafe { std::env::set_var("GEMINI_CLI_HOME", "   ") };

    assert_eq!(
        antigravity_cli_conversations_path("/tmp/home", true),
        PathBuf::from("/tmp/home/.gemini/antigravity-cli/conversations")
    );

    match previous {
        Some(value) => unsafe { std::env::set_var("GEMINI_CLI_HOME", value) },
        None => unsafe { std::env::remove_var("GEMINI_CLI_HOME") },
    }
}

#[test]
fn parse_legacy_antigravity_cli_extra_dirs_accepts_only_legacy_key() {
    assert_eq!(
        parse_legacy_antigravity_cli_extra_dirs(
            "antigravity-cli:/tmp/agy-cli,antigravity:/tmp/agy,broken"
        ),
        vec!["/tmp/agy-cli".to_string()]
    );
}

#[test]
fn warp_setup_warning_explains_aggregate_cache_is_not_reported() {
    let warnings = warp_setup_warnings_for_report(&Some(vec!["warp".to_string()]));

    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("not included in local reports"));
    assert!(warnings[0].contains("no token buckets"));
}

#[test]
fn cursor_auto_sync_enabled_for_default_report() {
    assert!(should_auto_sync_cursor_for_local_report(&None, &None));
}

#[test]
fn cursor_auto_sync_enabled_when_cursor_filter_is_explicit() {
    assert!(should_auto_sync_cursor_for_local_report(
        &None,
        &Some(vec!["cursor".to_string()])
    ));
}

#[test]
fn cursor_auto_sync_disabled_when_filter_excludes_cursor() {
    assert!(!should_auto_sync_cursor_for_local_report(
        &None,
        &Some(vec!["codex".to_string()])
    ));
}

#[test]
fn cursor_auto_sync_disabled_for_home_override() {
    assert!(!should_auto_sync_cursor_for_local_report(
        &Some("/tmp/other-home".to_string()),
        &None
    ));
    assert!(!should_auto_sync_cursor_for_local_report(
        &Some("/tmp/other-home".to_string()),
        &Some(vec!["cursor".to_string()])
    ));
}

#[test]
fn cursor_auto_sync_runtime_init_failure_is_best_effort() {
    let result = run_best_effort_cursor_sync_with_runtime_factory(|| {
        Err(std::io::Error::other("runtime unavailable"))
    });

    assert!(!result.synced);
    assert_eq!(result.rows, 0);
    assert!(result
        .error
        .as_deref()
        .is_some_and(|error| error.contains("runtime unavailable")));
}

#[test]
fn light_cache_write_allows_default_home() {
    assert!(can_write_light_cache(&None));
}

#[test]
fn light_cache_write_refuses_when_home_dir_set() {
    assert!(!can_write_light_cache(&Some("/tmp/fake-home".to_string())));
}
