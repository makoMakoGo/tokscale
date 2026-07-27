//! Achievement ladders for the Overview snapshot: five permanent ladders,
//! each with five tiers plus a roast title for below the first tier.

use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

use crate::tui::data::CacheRate;
use crate::tui::themes::Theme;

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
const CLIENTS: TierSet = TierSet {
    roast: "光杆司令",
    tiers: [
        (1, "牧马人", "1"),
        (5, "驯马师", "5"),
        (10, "弼马温", "10"),
        (15, "御马监", "15"),
        (20, "齐天大圣", "20"),
    ],
};

const TITLE_WIDTH: usize = 10;

pub(super) struct Achievement {
    title: &'static str,
    ladder: [&'static str; 5],
    /// Tier index 0..=4, or -1 when below the first tier.
    current: i8,
}

fn rank(set: &TierSet, value: u64) -> Achievement {
    rank_when(set, |threshold| value >= threshold)
}

fn rank_cache(set: &TierSet, rate: CacheRate) -> Achievement {
    rank_when(set, |threshold| rate.reaches(threshold))
}

fn rank_when(set: &TierSet, reached: impl Fn(u64) -> bool) -> Achievement {
    let mut current: i8 = -1;
    for (index, (threshold, _, _)) in set.tiers.iter().enumerate() {
        if reached(*threshold) {
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
    current_streak: u32,
    total_tokens: u64,
    cache_rate: CacheRate,
    models: usize,
    clients: usize,
) -> Vec<Achievement> {
    vec![
        rank(&STREAK, current_streak as u64),
        rank(&TOKENS, total_tokens),
        rank_cache(&CACHE, cache_rate),
        rank(&MODELS, models as u64),
        rank(&CLIENTS, clients as u64),
    ]
}

pub(super) fn lines(theme: &Theme, achievements: &[Achievement]) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(achievements.len() + 2);
    lines.push(Line::from(Span::styled(
        "Achievements",
        Style::default()
            .fg(theme.text.primary)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::default());
    lines.extend(
        achievements
            .iter()
            .map(|achievement| ladder_line(theme, achievement)),
    );
    lines
}

fn ladder_line(theme: &Theme, achievement: &Achievement) -> Line<'static> {
    let locked = achievement.current < 0;
    let title_style = Style::default()
        .fg(theme.text.primary)
        .add_modifier(Modifier::BOLD);
    let title_pad = TITLE_WIDTH.saturating_sub(text_width(achievement.title));

    let mut spans = vec![
        Span::styled(achievement.title.to_string(), title_style),
        Span::raw(" ".repeat(title_pad + 1)),
    ];
    for (index, display) in achievement.ladder.iter().enumerate() {
        let tier = index as i8;
        if tier == achievement.current {
            spans.push(Span::styled(
                format!("[{display}]"),
                Style::default()
                    .fg(theme.status.success)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if !locked && tier < achievement.current {
            spans.push(Span::styled(
                display.to_string(),
                Style::default().fg(theme.status.success),
            ));
        } else {
            spans.push(Span::styled(
                display.to_string(),
                Style::default().fg(theme.text.secondary),
            ));
        }
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// Display width limited to what the ladders need (CJK counts double).
fn text_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::themes::ThemeName;

    fn theme() -> Theme {
        Theme::from_name(ThemeName::Blue)
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
        assert_eq!(rank(&CLIENTS, 15).title, "御马监");
        assert_eq!(rank(&CLIENTS, 20).title, "齐天大圣");
    }

    #[test]
    fn build_uses_the_authoritative_current_streak() {
        let achievements = build(30, 0, CacheRate::default(), 0, 0);

        assert_eq!(achievements[0].title, "废寝忘食");
        assert_eq!(achievements[0].current, 2);
    }

    #[test]
    fn cache_tier_uses_the_same_tenth_percent_as_the_display() {
        let rounded_to_fifty = build(0, 0, CacheRate::from_tokens(4_996, 10_000), 0, 0);
        let still_below_fifty = build(0, 0, CacheRate::from_tokens(4_994, 10_000), 0, 0);

        assert_eq!(rounded_to_fifty[2].title, "省吃俭用");
        assert_eq!(rounded_to_fifty[2].current, 0);
        assert_eq!(still_below_fifty[2].title, "败家子");
        assert_eq!(still_below_fifty[2].current, -1);
    }

    #[test]
    fn every_below_threshold_achievement_keeps_its_roast_title() {
        let achievements = build(0, 0, CacheRate::default(), 0, 0);

        assert_eq!(
            achievements
                .iter()
                .map(|achievement| (achievement.title, achievement.current))
                .collect::<Vec<_>>(),
            vec![
                ("三天打鱼", -1),
                ("养生局", -1),
                ("败家子", -1),
                ("从一而终", -1),
                ("光杆司令", -1),
            ]
        );
    }

    #[test]
    fn colors_encode_ladder_progress_without_highlighting_locked_titles() {
        let theme = theme();
        let locked = rank(&STREAK, 0);
        let unlocked = rank(&STREAK, 15);
        let locked_line = ladder_line(&theme, &locked);
        let unlocked_line = ladder_line(&theme, &unlocked);

        assert_eq!(locked_line.spans[0].style, unlocked_line.spans[0].style);
        assert_eq!(locked_line.spans[0].style.fg, Some(theme.text.primary));
        assert!(locked_line.spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));

        for tier_span in locked_line.spans.iter().skip(2).step_by(2) {
            assert_eq!(tier_span.style.fg, Some(theme.text.secondary));
            assert!(!tier_span.style.add_modifier.contains(Modifier::BOLD));
        }

        assert_eq!(unlocked_line.spans[2].style.fg, Some(theme.status.success));
        assert!(!unlocked_line.spans[2]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(unlocked_line.spans[4].content.as_ref(), "[15]");
        assert_eq!(unlocked_line.spans[4].style.fg, Some(theme.status.success));
        assert!(unlocked_line.spans[4]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        for tier_span in unlocked_line.spans.iter().skip(6).step_by(2) {
            assert_eq!(tier_span.style.fg, Some(theme.text.secondary));
        }
    }
}
