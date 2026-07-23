use crate::cli::*;
use crate::commands::render::*;
use crate::commands::shared::*;
use crate::tui::Tab;
use clap::Parser;
use std::path::PathBuf;
use tokscale_core::ClientId;

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
fn removed_clients_are_not_valid_client_ids() {
    for client in ["cursor", "trae", "kilocode"] {
        let error = parse_client_id_arg(client).unwrap_err();
        assert!(error.contains(client), "unexpected error: {error}");
    }
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
fn test_build_client_filter_canonicalizes_persisted_antigravity_cli_default() {
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
fn retired_kilocode_default_is_rejected() {
    let flags = ClientFlags::default();
    let defaults = vec!["kilocode".to_string()];
    let error = build_client_filter_with_defaults(flags, &defaults).unwrap_err();

    assert!(
        error.to_string().contains("kilocode"),
        "unexpected error: {error}"
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
    let cli = Cli::try_parse_from(["tokscale", "models", "--client", "opencode,claude"])
        .expect("parse ok");
    let Some(Commands::Models(args)) = cli.command else {
        panic!("expected models command");
    };
    assert_eq!(
        args.report.input.clients.clients,
        vec![ClientId::OpenCode, ClientId::Claude]
    );

    let cli = Cli::try_parse_from(["tokscale", "tui", "-c", "opencode", "-c", "claude"])
        .expect("parse ok");
    let Some(Commands::Tui(args)) = cli.command else {
        panic!("expected tui command");
    };
    assert_eq!(
        args.input.clients.clients,
        vec![ClientId::OpenCode, ClientId::Claude]
    );
}

#[test]
fn test_legacy_client_flags_are_removed() {
    assert!(Cli::try_parse_from(["tokscale", "--claude"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "--opencode"]).is_err());
}

#[test]
fn test_client_flag_accepts_uppercase() {
    let cli = Cli::try_parse_from(["tokscale", "models", "--client", "OPENCODE"])
        .expect("uppercase parses");
    let Some(Commands::Models(args)) = cli.command else {
        panic!("expected models command");
    };
    assert_eq!(args.report.input.clients.clients, vec![ClientId::OpenCode]);

    let cli = Cli::try_parse_from(["tokscale", "models", "-c", "Codebuff,Antigravity"])
        .expect("mixed-case parses");
    let Some(Commands::Models(args)) = cli.command else {
        panic!("expected models command");
    };
    assert_eq!(
        args.report.input.clients.clients,
        vec![ClientId::Codebuff, ClientId::Antigravity]
    );
}

#[test]
fn test_client_flag_rejects_unknown_and_empty_values() {
    assert!(Cli::try_parse_from(["tokscale", "models", "--client", "unknown"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "models", "--client", ""]).is_err());

    let error = Cli::try_parse_from(["tokscale", "models", "--client", "crush"])
        .expect_err("excluded clients must not remain valid CLI values")
        .to_string();
    assert!(error.contains("invalid client id `crush`"), "{error}");
    assert!(!error.contains("does not support local parsing"), "{error}");
}

#[test]
fn test_home_arg_rejects_empty_and_blank_values() {
    assert!(Cli::try_parse_from(["tokscale", "models", "--home", ""]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "models", "--home", "   "]).is_err());
}

#[test]
fn test_pricing_source_accepts_known_values() {
    let cli = Cli::try_parse_from([
        "tokscale",
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
        "tokscale",
        "pricing",
        "lookup",
        "gpt-4o",
        "--pricing-source",
        "unknown",
    ])
    .is_err());
}

#[test]
fn retired_pricing_source_flag_is_not_an_alias() {
    assert!(Cli::try_parse_from([
        "tokscale",
        "pricing",
        "lookup",
        "gpt-4o",
        "--source",
        "openrouter",
    ])
    .is_err());
}

#[test]
fn pricing_overrides_is_the_registered_user_facing_subcommand() {
    let cli = Cli::try_parse_from(["tokscale", "pricing", "overrides", "--json"])
        .expect("documented pricing overrides command must parse");

    assert!(matches!(
        cli.command,
        Some(Commands::Pricing {
            subcommand: PricingSubcommand::Overrides { json: true }
        })
    ));
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
fn root_rejects_business_options() {
    for args in [
        vec!["tokscale", "--json"],
        vec!["tokscale", "--light"],
        vec!["tokscale", "--client", "codex"],
        vec!["tokscale", "--week"],
        vec!["tokscale", "--json", "models"],
    ] {
        assert!(Cli::try_parse_from(args).is_err());
    }
}

#[test]
fn legacy_v4_invocations_get_one_migration_hint_without_becoming_aliases() {
    let strings = |values: &[&str]| {
        values
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        legacy_invocation_hint(&strings(&["--json", "models"])).as_deref(),
        Some("use `tokscale models --json`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["--light"])).as_deref(),
        Some("use `tokscale models`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["models", "--light"])).as_deref(),
        Some("use `tokscale models`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["--client", "codex"])).as_deref(),
        Some("use `tokscale tui --client codex`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["--client=codex"])).as_deref(),
        Some("use `tokscale tui --client=codex`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["tui", "--json"])).as_deref(),
        Some("use `tokscale models --json`")
    );
    assert_eq!(legacy_invocation_hint(&strings(&["graph", "--json"])), None);
    assert_eq!(legacy_invocation_hint(&strings(&["--json", "graph"])), None);
    assert_eq!(
        legacy_invocation_hint(&strings(&["graph", "--json", "--output", "graph.json"])),
        None
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["--client=codex", "models"])).as_deref(),
        Some("use `tokscale models --client=codex`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["pricing", "list-overrides"])).as_deref(),
        Some("use `tokscale pricing overrides`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["pricing", "list-overrides", "--json"])).as_deref(),
        Some("use `tokscale pricing overrides --json`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["pricing", "gpt-5", "--json"])).as_deref(),
        Some("use `tokscale pricing lookup gpt-5 --json`")
    );
    assert_eq!(
        legacy_invocation_hint(&strings(&["--json", "pricing", "gpt-5"])),
        None,
        "migration hints must never suggest another invalid invocation"
    );
    for unrelated in [
        &["wrapped", "--json"][..],
        &["monthly", "--benchmark"],
        &["pricing", "--json"],
        &["graph", "--group-by", "model"],
    ] {
        assert_eq!(
            legacy_invocation_hint(&strings(unrelated)),
            None,
            "migration hints must not change an explicit command's product"
        );
    }
}

#[test]
fn misplaced_and_equals_form_options_remain_parse_errors() {
    for args in [
        vec!["tokscale", "tui", "--json"],
        vec!["tokscale", "--client=codex"],
        vec!["tokscale", "graph", "--json"],
    ] {
        let error = Cli::try_parse_from(args).expect_err("legacy invocation must be rejected");
        assert_eq!(error.exit_code(), 2);
    }
}

#[test]
fn report_execution_plan_does_not_depend_on_terminal_state() {
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
        let cli = Cli::try_parse_from(["tokscale", "models", "--client", "opencode", "--json"])
            .expect("models command parses");
        let plan = ExecutionPlan::resolve(cli, terminal).expect("models plan resolves");
        let ExecutionPlan::Models(plan) = plan else {
            panic!("models must never resolve to a TUI plan");
        };
        assert!(plan.report.json);
        assert_eq!(
            plan.report.input.clients,
            Some(vec!["opencode".to_string()])
        );
    }
}

#[test]
fn json_report_plan_preserves_explicit_no_spinner() {
    let home = tempfile::TempDir::new().unwrap();
    let home = home.path().to_str().unwrap();
    let resolve = |explicit_no_spinner: bool| {
        let mut argv = vec!["tokscale", "models", "--home", home, "--json"];
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
        plan.report
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
        let cli = Cli::try_parse_from(["tokscale", "tui"]).expect("TUI command parses");
        let error = ExecutionPlan::resolve(cli, terminal).expect_err("non-TTY TUI must fail");
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("interactive terminal"));
    }
}

#[test]
fn tui_tab_accepts_every_tui_tab_name() {
    for (name, expected) in [
        ("overview", Tab::Overview),
        ("usage", Tab::Usage),
        ("models", Tab::Models),
        ("monthly", Tab::Monthly),
        ("weekly", Tab::Weekly),
        ("daily", Tab::Daily),
        ("hourly", Tab::Hourly),
        ("stats", Tab::Stats),
        ("agents", Tab::Agents),
        ("sessions", Tab::Sessions),
    ] {
        let cli = Cli::try_parse_from(["tokscale", "tui", "--tab", name])
            .unwrap_or_else(|error| panic!("{name} tab name must parse: {error}"));
        let Some(Commands::Tui(args)) = cli.command else {
            panic!("expected TUI command");
        };
        assert_eq!(args.tab, Some(expected));
    }

    let error = Cli::try_parse_from(["tokscale", "tui", "--tab", "issues"])
        .expect_err("the removed Issues tab name must not remain as an alias");
    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
}

#[test]
fn tui_execution_plan_rejects_disabled_optional_tab() {
    let home = tempfile::TempDir::new().unwrap();
    let cli = Cli::try_parse_from([
        "tokscale",
        "tui",
        "--home",
        home.path().to_str().unwrap(),
        "--tab",
        "usage",
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
        "tokscale",
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
fn removed_report_and_cache_flags_are_rejected() {
    for flag in ["--light", "--write-cache", "--no-write-cache"] {
        assert!(Cli::try_parse_from(["tokscale", "models", flag]).is_err());
    }
    assert!(Cli::try_parse_from(["tokscale", "wrapped", "--ranking", "agents"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "wrapped", "--disable-pinned"]).is_err());
}

#[test]
fn clap_accepts_input_cache_prune_command() {
    let cli = Cli::try_parse_from(["tokscale", "cache", "prune"]).expect("cache prune parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Cache {
            subcommand: CacheSubcommand::Prune
        })
    ));
}

#[test]
fn clap_accepts_explicit_cache_warm_scope() {
    let cli = Cli::try_parse_from(["tokscale", "cache", "warm", "--client", "codex"])
        .expect("cache warm parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Cache {
            subcommand: CacheSubcommand::Warm { .. }
        })
    ));
}

#[test]
fn cli_rejects_removed_account_management_namespaces() {
    assert!(Cli::try_parse_from(["tokscale", "cursor", "sync"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "cursor", "logout", "--all"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "codex", "accounts"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "codex", "switch", "work"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "trae", "status"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "warp", "status"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "warp", "sync"]).is_err());
}

#[test]
fn clap_accepts_usage_without_light_flag() {
    assert!(Cli::try_parse_from(["tokscale", "usage"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "usage", "--json"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "usage", "--light"]).is_err());
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
fn antigravity_is_a_local_client_without_an_integration_command_namespace() {
    assert!(Cli::try_parse_from(["tokscale", "models", "--client", "antigravity"]).is_ok());
    assert!(Cli::try_parse_from(["tokscale", "models", "--client", "antigravity-cli"]).is_err());
    assert!(Cli::try_parse_from(["tokscale", "antigravity", "status"]).is_err());
}

#[test]
fn antigravity_local_input_uses_home_when_env_roots_are_disabled() {
    let def = ClientId::Antigravity.local_def().unwrap();
    assert_eq!(
        def.resolve_path_with_env_strategy("/tmp/home", false),
        PathBuf::from("/tmp/home/.gemini/antigravity-cli/conversations")
    );
}

#[test]
#[serial_test::serial]
fn antigravity_local_input_falls_back_for_blank_env() {
    let previous = std::env::var("GEMINI_CLI_HOME").ok();
    unsafe { std::env::set_var("GEMINI_CLI_HOME", "   ") };

    let def = ClientId::Antigravity.local_def().unwrap();
    assert_eq!(
        def.resolve_path_with_env_strategy("/tmp/home", true),
        PathBuf::from("/tmp/home/.gemini/antigravity-cli/conversations")
    );

    match previous {
        Some(value) => unsafe { std::env::set_var("GEMINI_CLI_HOME", value) },
        None => unsafe { std::env::remove_var("GEMINI_CLI_HOME") },
    }
}
