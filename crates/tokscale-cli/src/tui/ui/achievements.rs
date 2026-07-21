//! Achievement ladders for the Overview snapshot: five permanent ladders,
//! each with five tiers plus a roast title for below the first tier.

use ratatui::prelude::*;

use crate::tui::app::App;
use crate::tui::data::DailyUsage;

struct TierSet {
    roast: &'static str,
    tiers: [(u64, &'static str, &'static str); 5],
}

const STREAK: TierSet = TierSet {
    roast: "三天打鱼",
    tiers: [
        (7, "浅尝辄止", "7"),
        (15, "渐入佳境", "15"),
        (30, "废寝忘食", "30"),
        (90, "不眠不休", "90"),
        (180, "人机合一", "180"),
    ],
};
const TOKENS: TierSet = TierSet {
    roast: "养生局",
    tiers: [
        (100_000_000, "开胃小菜", "0.1B"),
        (1_000_000_000, "细嚼慢咽", "1B"),
        (10_000_000_000, "大胃袋", "10B"),
        (100_000_000_000, "饕餮", "100B"),
        (1_000_000_000_000, "黑洞", "1T"),
    ],
};
const CACHE: TierSet = TierSet {
    roast: "败家子",
    tiers: [
        (50, "省吃俭用", "50%"),
        (80, "精打细算", "80%"),
        (90, "持家有道", "90%"),
        (95, "薅羊毛大师", "95%"),
        (99, "infra 之神", "99%"),
    ],
};
const MODELS: TierSet = TierSet {
    roast: "从一而终",
    tiers: [
        (10, "浅尝一口", "10"),
        (20, "品石师", "20"),
        (30, "赤石大王", "30"),
        (100, "神农尝百草", "100"),
        (200, "满汉全席", "200"),
    ],
};
const HARNESSES: TierSet = TierSet {
    roast: "光杆司令",
    tiers: [
        (1, "牧马人", "1"),
        (5, "驯马师", "5"),
        (10, "弼马温", "10"),
        (15, "御马监", "15"),
        (20, "齐天大圣", "20"),
    ],
};

const TITLE_WIDTH: usize = 12;

pub(super) struct Achievement {
    title: &'static str,
    ladder: [&'static str; 5],
    /// Tier index 0..=4, or -1 when below the first tier (roast title).
    current: i8,
}

fn rank(set: &TierSet, value: u64) -> Achievement {
    let mut current: i8 = -1;
    for (index, (threshold, _, _)) in set.tiers.iter().enumerate() {
        if value >= *threshold {
            current = index as i8;
        }
    }
    let title = if current < 0 {
        set.roast
    } else {
        set.tiers[current as usize].1
    };
    Achievement {
        title,
        ladder: set.tiers.map(|(_, _, display)| display),
        current,
    }
}

pub(super) fn build(
    app: &App,
    total_tokens: u64,
    cache_read: u64,
    models: usize,
    harnesses: usize,
) -> Vec<Achievement> {
    let cache_pct = if total_tokens > 0 {
        cache_read.saturating_mul(100) / total_tokens
    } else {
        0
    };
    vec![
        rank(&STREAK, streak_days(&app.data.daily) as u64),
        rank(&TOKENS, total_tokens),
        rank(&CACHE, cache_pct),
        rank(&MODELS, models as u64),
        rank(&HARNESSES, harnesses as u64),
    ]
}

pub(super) fn lines(app: &App, achievements: &[Achievement]) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(achievements.len() + 1);
    lines.push(Line::from(Span::styled(
        "Achievements",
        Style::default()
            .fg(app.theme.foreground)
            .add_modifier(Modifier::BOLD),
    )));
    lines.extend(
        achievements
            .iter()
            .map(|achievement| ladder_line(app, achievement)),
    );
    lines
}

/// Consecutive days with activity, counting back from the most recent
/// active day (today not being scanned yet must not break the streak).
pub(super) fn streak_days(daily: &[DailyUsage]) -> u32 {
    let mut dates: Vec<_> = daily
        .iter()
        .filter(|day| day.tokens.total() > 0)
        .map(|day| day.date)
        .collect();
    dates.sort_unstable();
    dates.dedup();

    let mut streak = 0;
    let mut cursor = match dates.last() {
        Some(date) => *date,
        None => return 0,
    };
    while dates.contains(&cursor) {
        streak += 1;
        match cursor.pred_opt() {
            Some(previous) => cursor = previous,
            None => break,
        }
    }
    streak
}

fn ladder_line(app: &App, achievement: &Achievement) -> Line<'static> {
    let roasting = achievement.current < 0;
    let title_style = if roasting {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
            .fg(app.theme.foreground)
            .add_modifier(Modifier::BOLD)
    };
    let title_pad = TITLE_WIDTH.saturating_sub(text_width(achievement.title));

    let mut spans = vec![
        Span::styled(achievement.title.to_string(), title_style),
        Span::raw(" ".repeat(title_pad + 2)),
    ];
    for (index, display) in achievement.ladder.iter().enumerate() {
        let tier = index as i8;
        if tier == achievement.current {
            spans.push(Span::styled(
                format!("[{display}]"),
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if !roasting && tier < achievement.current {
            spans.push(Span::styled(
                display.to_string(),
                Style::default().fg(app.theme.accent),
            ));
        } else {
            spans.push(Span::styled(
                display.to_string(),
                Style::default().fg(app.theme.muted),
            ));
        }
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// Display width limited to what the ladders need (CJK counts double).
fn text_width(text: &str) -> usize {
    text.chars()
        .map(|ch| if (ch as u32) > 0x2E80 { 2 } else { 1 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::data::TokenBreakdown;
    use chrono::NaiveDate;

    fn day(date: &str, tokens: u64) -> DailyUsage {
        DailyUsage {
            date: NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap(),
            tokens: TokenBreakdown {
                input: tokens,
                ..Default::default()
            },
            cost: 0.0,
            source_breakdown: Default::default(),
            message_count: 0,
            turn_count: 0,
        }
    }

    #[test]
    fn streak_counts_consecutive_days_back_from_the_last_active_day() {
        let daily = vec![
            day("2026-05-01", 1),
            day("2026-05-18", 2),
            day("2026-05-19", 3),
            day("2026-05-20", 4),
        ];
        assert_eq!(streak_days(&daily), 3);
        assert_eq!(streak_days(&[]), 0);
        assert_eq!(streak_days(&[day("2026-05-20", 0)]), 0);
    }

    #[test]
    fn rank_picks_the_highest_reached_tier_or_roast() {
        assert_eq!(rank(&STREAK, 0).title, "三天打鱼");
        assert_eq!(rank(&STREAK, 0).current, -1);
        assert_eq!(rank(&STREAK, 7).title, "浅尝辄止");
        assert_eq!(rank(&STREAK, 179).title, "不眠不休");
        assert_eq!(rank(&STREAK, 180).title, "人机合一");
        assert_eq!(rank(&TOKENS, 12_000_000_000).title, "大胃袋");
        assert_eq!(rank(&MODELS, 67).title, "赤石大王");
        assert_eq!(rank(&CACHE, 91).title, "持家有道");
        assert_eq!(rank(&HARNESSES, 15).title, "御马监");
        assert_eq!(rank(&HARNESSES, 20).title, "齐天大圣");
    }
}
