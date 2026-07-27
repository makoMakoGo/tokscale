use crate::cli::*;
use crate::commands::render::*;
use crate::commands::shared::*;
use crate::tui::Tab;
use clap::Parser;
use tokenx_engine::ClientId;

#[test]
fn process_runtime_uses_bounded_worker_pool() {
    let runtime = super::build_process_runtime().expect("process runtime must build");

    assert_eq!(runtime.metrics().num_workers(), super::TOKIO_WORKER_THREADS);
    assert_eq!(super::TOKIO_WORKER_THREADS, 2);
}

#[test]
fn tui_exit_maps_to_process_execution_outcome() {
    assert_eq!(
        super::ExecutionOutcome::from(crate::tui::TuiExit::Quit),
        super::ExecutionOutcome::Completed
    );
    assert_eq!(
        super::ExecutionOutcome::from(crate::tui::TuiExit::Interrupted),
        super::ExecutionOutcome::Interrupted
    );
}

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
fn client_scope_without_flags_or_defaults_is_the_full_universe() {
    let flags = ClientFlags::default();
    let (universe, restricted) = resolve_client_universe(flags, &[]).unwrap();
    assert_eq!(universe, tokenx_engine::ClientUniverse::all());
    assert!(!restricted);
}

#[test]
fn client_scope_deduplicates_typed_cli_clients() {
    let flags = ClientFlags {
        clients: vec![ClientId::Claude, ClientId::Claude, ClientId::OpenCode],
    };
    let (universe, restricted) = resolve_client_universe(flags, &[]).unwrap();
    assert!(restricted);
    assert_eq!(universe.iter().count(), 2);
    assert!(universe.contains(ClientId::Claude));
    assert!(universe.contains(ClientId::OpenCode));
}

#[test]
fn client_scope_uses_typed_defaults_when_cli_is_empty() {
    let flags = ClientFlags::default();
    let defaults = [ClientId::OpenCode, ClientId::Claude];
    let (universe, restricted) = resolve_client_universe(flags, &defaults).unwrap();
    assert!(restricted);
    assert!(universe.contains(ClientId::OpenCode));
    assert!(universe.contains(ClientId::Claude));
}

#[test]
fn client_scope_cli_overrides_defaults_completely() {
    let flags = ClientFlags {
        clients: vec![ClientId::Codex],
    };
    let defaults = [ClientId::OpenCode, ClientId::Claude];
    let (universe, restricted) = resolve_client_universe(flags, &defaults).unwrap();
    assert!(restricted);
    assert_eq!(universe.iter().collect::<Vec<_>>(), vec![ClientId::Codex]);
}

#[test]
fn test_client_flags_parses_canonical_form() {
    // End-to-end smoke test: ensure clap derives accept the new
    // `--client a,b` and `-c a -c b` shapes through the CLI parser.
    let cli =
        Cli::try_parse_from(["tokenx", "models", "--client", "opencode,claude"]).expect("parse ok");
    let Some(Commands::Models(args)) = cli.command else {
        panic!("expected models command");
    };
    assert_eq!(
        args.input.clients.clients,
        vec![ClientId::OpenCode, ClientId::Claude]
    );

    let cli =
        Cli::try_parse_from(["tokenx", "tui", "-c", "opencode", "-c", "claude"]).expect("parse ok");
    let Some(Commands::Tui(args)) = cli.command else {
        panic!("expected tui command");
    };
    assert_eq!(
        args.input.clients.clients,
        vec![ClientId::OpenCode, ClientId::Claude]
    );
}

#[test]
fn test_client_flag_accepts_uppercase() {
    let cli = Cli::try_parse_from(["tokenx", "models", "--client", "OPENCODE"])
        .expect("uppercase parses");
    let Some(Commands::Models(args)) = cli.command else {
        panic!("expected models command");
    };
    assert_eq!(args.input.clients.clients, vec![ClientId::OpenCode]);

    let cli = Cli::try_parse_from(["tokenx", "models", "-c", "Codebuff,Antigravity"])
        .expect("mixed-case parses");
    let Some(Commands::Models(args)) = cli.command else {
        panic!("expected models command");
    };
    assert_eq!(
        args.input.clients.clients,
        vec![ClientId::Codebuff, ClientId::Antigravity]
    );
}

#[test]
fn test_client_flag_rejects_unknown_and_empty_values() {
    assert!(Cli::try_parse_from(["tokenx", "models", "--client", "unknown"]).is_err());
    assert!(Cli::try_parse_from(["tokenx", "models", "--client", ""]).is_err());
}

#[test]
fn test_home_arg_rejects_empty_and_blank_values() {
    assert!(Cli::try_parse_from(["tokenx", "models", "--home", ""]).is_err());
    assert!(Cli::try_parse_from(["tokenx", "models", "--home", "   "]).is_err());
}

#[test]
fn tui_theme_argument_is_typed_and_requires_canonical_spelling() {
    let cli = Cli::try_parse_from(["tokenx", "tui", "--theme", "lagoon"]).unwrap();
    let Some(Commands::Tui(args)) = cli.command else {
        panic!("expected TUI command");
    };
    assert_eq!(args.theme, Some(crate::theme::ThemeName::Lagoon));
    assert!(Cli::try_parse_from(["tokenx", "tui", "--theme", "Lagoon"]).is_err());
}

#[test]
fn test_pricing_source_accepts_known_values() {
    let cli = Cli::try_parse_from([
        "tokenx",
        "pricing",
        "lookup",
        "gpt-4o",
        "--pricing-source",
        "openrouter",
    ])
    .expect("Pricing Source parses");
    let Some(Commands::Pricing {
        subcommand: PricingSubcommand::Lookup { pricing_source, .. },
    }) = cli.command
    else {
        panic!("expected pricing command");
    };
    assert_eq!(pricing_source, Some(PricingSource::Openrouter));
}

#[test]
fn test_pricing_source_rejects_unknown_values() {
    assert!(Cli::try_parse_from([
        "tokenx",
        "pricing",
        "lookup",
        "gpt-4o",
        "--pricing-source",
        "unknown",
    ])
    .is_err());
}

#[test]
fn pricing_overrides_is_the_registered_user_facing_subcommand() {
    let cli = Cli::try_parse_from(["tokenx", "pricing", "overrides", "--json"])
        .expect("documented pricing overrides command must parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Pricing {
            subcommand: PricingSubcommand::Overrides { json: true }
        })
    ));
}

#[test]
fn resolved_custom_date_range_is_typed_and_labeled() {
    let since = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
    let until = chrono::NaiveDate::from_ymd_opt(2024, 12, 31).unwrap();
    let resolved = resolve_date_for_date(
        DateRangeFlags {
            since: Some(since),
            until: Some(until),
            ..DateRangeFlags::default()
        },
        chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
    )
    .unwrap();

    assert_eq!(resolved.range.since(), Some(since));
    assert_eq!(resolved.range.until(), Some(until));
    assert_eq!(
        resolved.label.as_deref(),
        Some("from 2024-01-01 to 2024-12-31")
    );
    assert_eq!(resolved.relative, None);
}

#[test]
fn resolved_date_range_without_flags_is_unfiltered() {
    let resolved = resolve_date_for_date(
        DateRangeFlags::default(),
        chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
    )
    .unwrap();

    assert_eq!(resolved.range, tokenx_engine::DateRange::none());
    assert_eq!(resolved.label, None);
    assert_eq!(resolved.relative, None);
}

#[test]
fn resolved_today_uses_provided_local_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
    let resolved = resolve_date_for_date(
        DateRangeFlags {
            today: true,
            ..DateRangeFlags::default()
        },
        today,
    )
    .unwrap();

    assert_eq!(resolved.range.since(), Some(today));
    assert_eq!(resolved.range.until(), Some(today));
    assert_eq!(resolved.label.as_deref(), Some("Today"));
    assert_eq!(resolved.relative, Some(RelativeDateRange::Today));
}

#[test]
fn resolved_week_uses_provided_local_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
    let resolved = resolve_date_for_date(
        DateRangeFlags {
            week: true,
            ..DateRangeFlags::default()
        },
        today,
    )
    .unwrap();

    assert_eq!(
        resolved.range.since(),
        Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 2).unwrap())
    );
    assert_eq!(resolved.range.until(), Some(today));
    assert_eq!(resolved.label.as_deref(), Some("Last 7 days"));
    assert_eq!(resolved.relative, Some(RelativeDateRange::LastSevenDays));
}

#[test]
fn resolved_month_uses_provided_local_date() {
    let today = chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
    let resolved = resolve_date_for_date(
        DateRangeFlags {
            month: true,
            ..DateRangeFlags::default()
        },
        today,
    )
    .unwrap();

    assert_eq!(
        resolved.range.since(),
        Some(chrono::NaiveDate::from_ymd_opt(2026, 3, 1).unwrap())
    );
    assert_eq!(resolved.range.until(), Some(today));
    assert_eq!(resolved.label.as_deref(), Some("March 2026"));
    assert_eq!(resolved.relative, Some(RelativeDateRange::CurrentMonth));
}

#[test]
fn relative_month_re_resolves_when_local_midnight_enters_a_new_month() {
    let next_month = chrono::NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
    let range = RelativeDateRange::CurrentMonth.resolve(next_month);

    assert_eq!(range.since(), Some(next_month));
    assert_eq!(range.until(), Some(next_month));
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
fn resolved_year_is_typed_and_labeled() {
    let resolved = resolve_date_for_date(
        DateRangeFlags {
            year: Some(2024),
            ..DateRangeFlags::default()
        },
        chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
    )
    .unwrap();

    assert_eq!(resolved.range.year(), Some(2024));
    assert_eq!(resolved.label.as_deref(), Some("2024"));
}

#[test]
fn resolved_reversed_custom_range_preserves_error_message() {
    let error = resolve_date_for_date(
        DateRangeFlags {
            since: Some(chrono::NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()),
            until: Some(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            ..DateRangeFlags::default()
        },
        chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
    )
    .unwrap_err();

    assert_eq!(
        error.to_string(),
        "--since (2024-12-31) must not be later than --until (2024-01-01)"
    );
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
fn root_rejects_business_options() {
    for args in [
        vec!["tokenx", "--json"],
        vec!["tokenx", "--client", "codex"],
        vec!["tokenx", "--week"],
        vec!["tokenx", "--json", "models"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn misplaced_and_equals_form_options_remain_parse_errors() {
    for args in [
        vec!["tokenx", "tui", "--json"],
        vec!["tokenx", "--client=codex"],
        vec!["tokenx", "not-a-command", "--json"],
    ] {
        let error = Cli::try_parse_from(args).expect_err("invalid invocation must be rejected");
        assert_eq!(error.exit_code(), 2);
    }
}

#[test]
fn models_execution_plan_does_not_depend_on_terminal_state() {
    for terminal in [
        TerminalState {
            stdin: true,
            stdout: true,
        },
        TerminalState {
            stdin: false,
            stdout: false,
        },
    ] {
        let cli = Cli::try_parse_from(["tokenx", "models", "--client", "opencode", "--json"])
            .expect("models command parses");
        let plan = ExecutionPlan::resolve(cli, terminal).expect("models plan resolves");
        let ExecutionPlan::Models(plan) = plan else {
            panic!("models must never resolve to a TUI plan");
        };
        assert!(plan.json);
        assert!(
            !plan.startup.input.home.as_os_str().is_empty(),
            "startup must resolve the optional CLI home into one required path"
        );
        assert!(plan.startup.input.restricted);
        assert_eq!(
            plan.startup.input.universe.iter().collect::<Vec<_>>(),
            vec![ClientId::OpenCode]
        );
    }
}

#[test]
#[serial_test::serial]
fn one_startup_snapshot_resolves_all_settings_driven_policy() {
    struct ConfigDirGuard(Option<std::ffi::OsString>);
    impl Drop for ConfigDirGuard {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var("TOKENX_CONFIG_DIR", value),
                    None => std::env::remove_var("TOKENX_CONFIG_DIR"),
                }
            }
        }
    }

    let input_home = tempfile::TempDir::new().unwrap();
    let product_root = tempfile::TempDir::new().unwrap();
    let settings_path = product_root.path().join("settings.json");
    let previous_config_dir = std::env::var_os("TOKENX_CONFIG_DIR");
    unsafe {
        std::env::set_var("TOKENX_CONFIG_DIR", product_root.path());
    }
    let _guard = ConfigDirGuard(previous_config_dir);

    let decoy_settings = input_home.path().join(".tokenx").join("settings.json");
    std::fs::create_dir_all(decoy_settings.parent().unwrap()).unwrap();
    std::fs::write(
        decoy_settings,
        r#"{"colorPalette":"halloween","defaultClients":["amp"]}"#,
    )
    .unwrap();
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(
        &settings_path,
        serde_json::to_vec(&serde_json::json!({
            "colorPalette": "lagoon",
            "autoRefreshEnabled": true,
            "autoRefreshMs": 40000,
            "defaultClients": ["claude", "codex"],
            "scanner": {
                "extraScanPaths": {
                    "codex": ["/tmp/tokenx-extra-codex"]
                }
            },
            "subscription": {
                "enabled": true,
                "providers": ["codex"]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        product_root.path().join("custom-pricing.json"),
        b"{not-json",
    )
    .unwrap();

    let cli = Cli::try_parse_from([
        "tokenx",
        "tui",
        "--home",
        input_home.path().to_str().unwrap(),
        "--tab",
        "subscription",
    ])
    .unwrap();
    let plan = ExecutionPlan::resolve(
        cli,
        TerminalState {
            stdin: true,
            stdout: true,
        },
    )
    .unwrap();
    let ExecutionPlan::Tui(plan) = plan else {
        panic!("expected TUI plan");
    };

    assert_eq!(
        plan.startup.input.home,
        input_home.path().canonicalize().unwrap()
    );
    assert_eq!(
        plan.startup.input.universe.iter().collect::<Vec<_>>(),
        vec![ClientId::Claude, ClientId::Codex]
    );
    assert!(plan.startup.input.restricted);
    assert_eq!(
        plan.startup.settings.color_palette,
        crate::theme::ThemeName::Lagoon
    );
    assert!(plan.startup.settings.auto_refresh_enabled);
    assert_eq!(plan.startup.settings.auto_refresh_ms, 40_000);
    assert_eq!(
        plan.startup
            .settings
            .scanner
            .extra_scan_paths
            .get(&ClientId::Codex)
            .unwrap(),
        &[std::path::PathBuf::from("/tmp/tokenx-extra-codex")]
    );
    assert!(plan.startup.settings.subscription.enabled);
    assert_eq!(plan.startup.settings.subscription.providers.len(), 1);
    assert_eq!(
        plan.startup.paths.settings_file(),
        settings_path,
        "input discovery home must not redirect Tokenx product state"
    );
    let replacement_product_root = tempfile::TempDir::new().unwrap();
    unsafe {
        std::env::set_var("TOKENX_CONFIG_DIR", replacement_product_root.path());
    }
    assert_eq!(
        plan.startup.paths.root(),
        product_root.path(),
        "a resolved startup snapshot must not observe later environment changes"
    );
    assert_eq!(
        plan.startup.paths.generation_cache_file(),
        product_root.path().join("cache/generation.bin")
    );
    assert_eq!(
        tokenx_engine::pricing::PricingStatus::from_diagnostics(plan.startup.pricing.diagnostics()),
        tokenx_engine::pricing::PricingStatus::Unavailable,
        "invalid optional pricing metadata must not prevent startup resolution"
    );
    assert!(plan
        .startup
        .pricing
        .diagnostics()
        .iter()
        .any(|diagnostic| diagnostic.message().contains("failed to parse JSON")));
}

#[test]
fn json_models_plan_preserves_explicit_no_spinner() {
    let home = tempfile::TempDir::new().unwrap();
    let home = home.path().to_str().unwrap();
    let resolve = |explicit_no_spinner: bool| {
        let mut argv = vec!["tokenx", "models", "--home", home, "--json"];
        if explicit_no_spinner {
            argv.push("--no-spinner");
        }
        let cli = Cli::try_parse_from(argv).expect("models command parses");
        let plan = ExecutionPlan::resolve(
            cli,
            TerminalState {
                stdin: false,
                stdout: false,
            },
        )
        .expect("models plan resolves");
        let ExecutionPlan::Models(plan) = plan else {
            panic!("expected models plan");
        };
        plan
    };

    let implicit = resolve(false);
    let explicit = resolve(true);
    assert!(implicit.json);
    assert!(explicit.json);
    assert!(!implicit.no_spinner);
    assert!(explicit.no_spinner);
}

#[test]
fn effective_spinner_policy_keeps_json_quiet_without_erasing_explicit_intent() {
    assert!(!super::effective_no_spinner(false, false));
    assert!(super::effective_no_spinner(false, true));
    assert!(super::effective_no_spinner(true, false));
    assert!(super::effective_no_spinner(true, true));
}

#[test]
fn tui_execution_plan_requires_both_interactive_streams() {
    for terminal in [
        TerminalState {
            stdin: false,
            stdout: true,
        },
        TerminalState {
            stdin: true,
            stdout: false,
        },
    ] {
        let cli = Cli::try_parse_from(["tokenx", "tui"]).expect("TUI command parses");
        let error = ExecutionPlan::resolve(cli, terminal).expect_err("non-TTY TUI must fail");
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("interactive terminal"));
    }
}

#[test]
fn tui_tab_accepts_every_tui_tab_name() {
    for (name, expected) in [
        ("overview", Tab::Overview),
        ("subscription", Tab::Subscription),
        ("models", Tab::Models),
        ("monthly", Tab::Monthly),
        ("weekly", Tab::Weekly),
        ("daily", Tab::Daily),
        ("hourly", Tab::Hourly),
        ("stats", Tab::Stats),
        ("agents", Tab::Agents),
        ("sessions", Tab::Sessions),
    ] {
        let cli = Cli::try_parse_from(["tokenx", "tui", "--tab", name])
            .unwrap_or_else(|error| panic!("{name} tab name must parse: {error}"));
        let Some(Commands::Tui(args)) = cli.command else {
            panic!("expected TUI command");
        };
        assert_eq!(args.tab, Some(expected));
    }
}

#[test]
fn tui_execution_plan_rejects_disabled_optional_tab() {
    let home = tempfile::TempDir::new().unwrap();
    let cli = Cli::try_parse_from([
        "tokenx",
        "tui",
        "--home",
        home.path().to_str().unwrap(),
        "--tab",
        "subscription",
    ])
    .expect("TUI command parses");
    let error = ExecutionPlan::resolve(
        cli,
        TerminalState {
            stdin: true,
            stdout: true,
        },
    )
    .expect_err("disabled explicit tab must fail before entering the TUI");
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("disabled in settings.json"));
}

#[test]
fn resolve_rejects_reversed_custom_date_range() {
    let cli = Cli::try_parse_from([
        "tokenx",
        "models",
        "--since",
        "2026-07-15",
        "--until",
        "2026-07-14",
    ])
    .expect("individually valid dates parse");
    let error = ExecutionPlan::resolve(
        cli,
        TerminalState {
            stdin: false,
            stdout: false,
        },
    )
    .expect_err("reversed range must fail");
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("must not be later"));
}

#[test]
fn clap_accepts_input_cache_prune_command() {
    let cli = Cli::try_parse_from(["tokenx", "cache", "prune"]).expect("cache prune parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Cache {
            subcommand: CacheSubcommand::Prune
        })
    ));
}

#[test]
fn clap_accepts_explicit_cache_warm_scope() {
    let cli = Cli::try_parse_from(["tokenx", "cache", "warm", "--client", "codex"])
        .expect("cache warm parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Cache {
            subcommand: CacheSubcommand::Warm { .. }
        })
    ));
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
