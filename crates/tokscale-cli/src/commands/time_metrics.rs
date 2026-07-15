use crate::commands::render::LightSpinner;
use crate::commands::shared::{use_env_roots, ReportEnvelope};
use crate::tui;
use anyhow::Result;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_time_metrics_report(
    json: bool,
    home_dir: Option<String>,
    clients: Option<Vec<String>>,
    since: Option<String>,
    until: Option<String>,
    year: Option<String>,
    benchmark: bool,
    no_spinner: bool,
) -> Result<()> {
    use tokio::runtime::Runtime;
    use tokscale_core::{get_time_metrics_report, GroupBy, ReportOptions};

    let spinner = if no_spinner {
        None
    } else {
        Some(LightSpinner::start("Computing time metrics..."))
    };
    let use_env_roots = use_env_roots(&home_dir);
    let scanner_settings = tui::settings::load_scanner_settings_for_home(&home_dir)?;
    let rt = Runtime::new()?;
    let report = rt
        .block_on(async {
            get_time_metrics_report(ReportOptions {
                home_dir: home_dir.clone(),
                use_env_roots,
                clients,
                since,
                until,
                year,
                group_by: GroupBy::default(),
                scanner_settings,
            })
            .await
        })
        .map_err(|e| anyhow::anyhow!(e))?;

    if let Some(spinner) = spinner {
        spinner.stop();
    }
    super::shared::emit_health_summary(&report.health);

    let m = &report.metrics;

    if json {
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct TimeMetricsData<'a> {
            metrics: &'a tokscale_core::TimeMetrics,
        }

        let data = TimeMetricsData {
            metrics: &report.metrics,
        };
        let output = ReportEnvelope::new(
            data,
            report.health.clone(),
            report.processing_time_ms as u64,
        );
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Session Time Metrics");
        println!("====================");
        println!(
            "Total active time:       {}",
            format_duration_ms(m.total_active_time_ms)
        );
        println!(
            "Total wall-clock time:   {}",
            format_duration_ms(m.total_wall_time_ms)
        );
        println!(
            "Longest continuous use:  {}",
            format_duration_ms(m.longest_continuous_ms)
        );
        println!("Max concurrent sessions: {}", m.max_concurrent_sessions);
        println!("Total sessions:          {}", m.session_count);
    }

    if benchmark {
        eprintln!("Processing time: {}ms", report.processing_time_ms);
    }

    Ok(())
}

pub(crate) fn format_duration_ms(ms: i64) -> String {
    if ms <= 0 {
        return "0s".to_string();
    }
    let total_secs = ms / 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, secs)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, secs)
    } else {
        format!("{}s", secs)
    }
}
