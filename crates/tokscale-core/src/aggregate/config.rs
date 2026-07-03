//! Configuration for the aggregation engine: the date filter, the view
//! selector, and the combined [`AggregationConfig`].

/// Date filter applied to every `push`ed message via
/// `UnifiedMessage::date_string()`. Mirrors `retain_messages_in_date_range`
/// (lib.rs) byte-for-byte: a `year` prefix match plus inclusive `since`/`until`
/// string comparisons on the `%Y-%m-%d` date string. A fully-empty range is a
/// no-op that keeps every message.
#[derive(Debug, Clone, Default)]
pub struct DateRange {
    pub since: Option<String>,
    pub until: Option<String>,
    pub year: Option<String>,
}

impl DateRange {
    /// An inactive filter — keeps every message.
    pub fn none() -> Self {
        Self::default()
    }

    /// Build from the raw `ReportOptions` date fields.
    pub fn from_options(opts: &crate::ReportOptions) -> Self {
        Self {
            since: opts.since.clone(),
            until: opts.until.clone(),
            year: opts.year.clone(),
        }
    }

    /// True iff `date` (a `%Y-%m-%d` string from `date_string()`) passes the
    /// filter. Identical predicate to `retain_messages_in_date_range`.
    pub fn contains(&self, date: &str) -> bool {
        if self.year.is_none() && self.since.is_none() && self.until.is_none() {
            return true;
        }
        let year_ok = self
            .year
            .as_ref()
            .is_none_or(|year| date.starts_with(&format!("{year}-")));
        let since_ok = self
            .since
            .as_ref()
            .is_none_or(|since| date >= since.as_str());
        let until_ok = self
            .until
            .as_ref()
            .is_none_or(|until| date <= until.as_str());
        year_ok && since_ok && until_ok
    }
}

/// Bit-set selecting which views the engine accumulates, so a caller pays only
/// for the maps it asked for. A `u8` newtype (not `bitflags`) to honor the
/// campaign's no-new-dependency stance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewSet(u8);

impl ViewSet {
    pub const TUI: ViewSet = ViewSet(0b0000_0001);
    pub const MODEL: ViewSet = ViewSet(0b0000_0010);
    pub const MONTHLY: ViewSet = ViewSet(0b0000_0100);
    pub const HOURLY: ViewSet = ViewSet(0b0000_1000);
    pub const GRAPH: ViewSet = ViewSet(0b0001_0000);
    pub const SESSIONS: ViewSet = ViewSet(0b0010_0000);
    pub const TIME_METRICS: ViewSet = ViewSet(0b0100_0000);
    pub const AGENTS: ViewSet = ViewSet(0b1000_0000);

    /// The empty set.
    pub const fn empty() -> ViewSet {
        ViewSet(0)
    }

    /// True iff every bit set in `other` is also set in `self`.
    pub fn contains(self, other: ViewSet) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for ViewSet {
    type Output = ViewSet;
    fn bitor(self, rhs: ViewSet) -> ViewSet {
        ViewSet(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for ViewSet {
    fn bitor_assign(&mut self, rhs: ViewSet) {
        self.0 |= rhs.0;
    }
}

/// Everything the engine needs up front: the bucket grouping, the date filter
/// applied in `push`, and which views to accumulate.
#[derive(Debug, Clone)]
pub struct AggregationConfig {
    pub group_by: crate::GroupBy,
    pub date_range: DateRange,
    pub views: ViewSet,
}
