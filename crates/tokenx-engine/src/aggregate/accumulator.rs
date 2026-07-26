//! Streaming accumulator for the canonical parts of one generation.

use crate::aggregate::{
    session_usage::SessionUsageBuilder,
    usage_index::{FrozenUsageIndex, UsageIndexBuilder},
    UsageAggregationError,
};

use crate::{AttributedUsageRecord, CalendarContext, DateRange};

pub struct GenerationAccumulator {
    date_range: DateRange,
    calendar: CalendarContext,
    usage: UsageIndexBuilder,
    sessions: SessionUsageBuilder,
    error: Option<UsageAggregationError>,
}

pub(crate) enum RecordAggregationOutcome {
    Retained,
    Filtered,
    Rejected(UsageAggregationError),
    Failed,
}

impl GenerationAccumulator {
    pub fn new(date_range: DateRange, calendar: CalendarContext) -> Self {
        Self {
            date_range,
            calendar,
            usage: UsageIndexBuilder::new(),
            sessions: SessionUsageBuilder::new(),
            error: None,
        }
    }

    /// Fold one finalized client-attributed usage record into the canonical
    /// usage and session indexes.
    pub(crate) fn push(&mut self, msg: &AttributedUsageRecord) -> RecordAggregationOutcome {
        if self.error.is_some() {
            return RecordAggregationOutcome::Failed;
        }
        let calendar_fields = self.calendar.local_date_and_hour(msg.timestamp);
        if !self.date_range.is_unfiltered()
            && !calendar_fields.is_some_and(|(date, _)| self.date_range.contains(date))
        {
            return RecordAggregationOutcome::Filtered;
        }
        let (local_date, local_hour) = calendar_fields.unzip();
        if let Err(error) = self
            .usage
            .check_push_local(msg, local_date, local_hour)
            .and_then(|()| self.sessions.check_push(msg))
        {
            return RecordAggregationOutcome::Rejected(error);
        }
        if let Err(error) = self
            .usage
            .push_local(msg, local_date, local_hour)
            .and_then(|()| self.sessions.push(msg))
        {
            // A commit failure after the side-effect-free checks above means
            // the checked and commit paths disagree. Treat that as an
            // internal invariant failure, not as third-party data damage.
            self.error = Some(error);
            return RecordAggregationOutcome::Failed;
        }
        RecordAggregationOutcome::Retained
    }

    pub(crate) fn into_usage_index(self) -> Result<FrozenUsageIndex, UsageAggregationError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.usage.finish()),
        }
    }

    pub(crate) fn into_generation_parts(
        self,
    ) -> Result<(FrozenUsageIndex, Vec<crate::SessionUsage>), UsageAggregationError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok((self.usage.finish(), self.sessions.finish())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientId, TokenBreakdown};

    #[test]
    fn cross_record_token_overflow_rejects_only_the_offending_record() {
        let mut accumulator = GenerationAccumulator::new(
            DateRange::none(),
            CalendarContext::explicit("UTC").unwrap(),
        );
        for timestamp in 1..=2 {
            assert!(matches!(
                accumulator.push(&AttributedUsageRecord::new(
                    ClientId::Codex,
                    "gpt-overflow",
                    "openai",
                    "session",
                    timestamp,
                    TokenBreakdown {
                        input: i64::MAX,
                        ..TokenBreakdown::default()
                    },
                    0.0,
                )),
                RecordAggregationOutcome::Retained
            ));
        }
        let rejected = accumulator.push(&AttributedUsageRecord::new(
            ClientId::Codex,
            "must-not-appear",
            "openai",
            "session",
            3,
            TokenBreakdown {
                input: 2,
                ..TokenBreakdown::default()
            },
            0.0,
        ));
        assert!(matches!(
            rejected,
            RecordAggregationOutcome::Rejected(ref error)
                if error.field() == "global token totals"
        ));
        assert!(matches!(
            accumulator.push(&AttributedUsageRecord::new(
                ClientId::Codex,
                "gpt-overflow",
                "openai",
                "session",
                4,
                TokenBreakdown {
                    input: 1,
                    ..TokenBreakdown::default()
                },
                0.0,
            )),
            RecordAggregationOutcome::Retained
        ));

        let (usage_index, sessions) = accumulator.into_generation_parts().unwrap();
        let projection = usage_index
            .project_usage(
                &crate::GroupBy::Model,
                chrono::NaiveDate::from_ymd_opt(2026, 7, 26).unwrap(),
            )
            .unwrap();
        assert_eq!(projection.total_tokens, u64::MAX);
        assert_eq!(projection.models.len(), 1);
        assert_eq!(projection.models[0].model_id.as_ref(), "gpt-overflow");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].tokens.input, u64::MAX);
        assert_eq!(sessions[0].message_count, 3);
    }

    #[test]
    fn cross_client_model_overflow_is_rejected_before_grouped_projection() {
        let mut accumulator = GenerationAccumulator::new(
            DateRange::none(),
            CalendarContext::explicit("UTC").unwrap(),
        );
        for (client, session, timestamp) in
            [(ClientId::Codex, "codex", 1), (ClientId::Amp, "amp", 2)]
        {
            assert!(matches!(
                accumulator.push(&AttributedUsageRecord::new(
                    client,
                    "shared-model",
                    "openai",
                    session,
                    timestamp,
                    TokenBreakdown {
                        input: i64::MAX,
                        ..TokenBreakdown::default()
                    },
                    0.0,
                )),
                RecordAggregationOutcome::Retained
            ));
        }
        let rejected = accumulator.push(&AttributedUsageRecord::new(
            ClientId::Amp,
            "shared-model",
            "openai",
            "amp",
            3,
            TokenBreakdown {
                input: 2,
                ..TokenBreakdown::default()
            },
            0.0,
        ));
        assert!(matches!(
            rejected,
            RecordAggregationOutcome::Rejected(ref error)
                if error.field() == "global token totals"
        ));
        assert!(matches!(
            accumulator.push(&AttributedUsageRecord::new(
                ClientId::Amp,
                "shared-model",
                "openai",
                "amp",
                4,
                TokenBreakdown {
                    input: 1,
                    ..TokenBreakdown::default()
                },
                0.0,
            )),
            RecordAggregationOutcome::Retained
        ));

        let (usage_index, sessions) = accumulator.into_generation_parts().unwrap();
        let projection = usage_index.project_models(&crate::GroupBy::Model).unwrap();
        assert_eq!(projection.total_tokens, u64::MAX);
        assert_eq!(projection.models.len(), 1);
        assert_eq!(projection.models[0].tokens.input, u64::MAX);
        assert_eq!(sessions.len(), 2);
        let amp = sessions
            .iter()
            .find(|session| session.client == ClientId::Amp)
            .unwrap();
        assert_eq!(amp.tokens.input, i64::MAX as u64 + 1);
        assert_eq!(amp.message_count, 2);
    }

    #[test]
    fn cross_record_cost_overflow_rejects_only_the_offending_record() {
        let mut accumulator = GenerationAccumulator::new(
            DateRange::none(),
            CalendarContext::explicit("UTC").unwrap(),
        );
        assert!(matches!(
            accumulator.push(&AttributedUsageRecord::new(
                ClientId::Codex,
                "gpt-overflow",
                "openai",
                "session",
                1,
                TokenBreakdown {
                    input: 1,
                    ..TokenBreakdown::default()
                },
                f64::MAX,
            )),
            RecordAggregationOutcome::Retained
        ));
        let rejected = accumulator.push(&AttributedUsageRecord::new(
            ClientId::Codex,
            "must-not-appear",
            "openai",
            "session",
            2,
            TokenBreakdown {
                input: 1,
                ..TokenBreakdown::default()
            },
            f64::MAX,
        ));
        assert!(matches!(
            rejected,
            RecordAggregationOutcome::Rejected(ref error) if error.field() == "global cost"
        ));

        let (usage_index, sessions) = accumulator.into_generation_parts().unwrap();
        let projection = usage_index.project_models(&crate::GroupBy::Model).unwrap();
        assert_eq!(projection.total_tokens, 1);
        assert_eq!(projection.models.len(), 1);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].message_count, 1);
    }

    #[test]
    fn non_finite_record_cost_is_rejected_instead_of_normalized_to_zero() {
        let mut accumulator = GenerationAccumulator::new(
            DateRange::none(),
            CalendarContext::explicit("UTC").unwrap(),
        );
        let rejected = accumulator.push(&AttributedUsageRecord::new(
            ClientId::Codex,
            "gpt-overflow",
            "openai",
            "session",
            1,
            TokenBreakdown {
                input: 1,
                ..TokenBreakdown::default()
            },
            f64::INFINITY,
        ));

        assert!(matches!(
            rejected,
            RecordAggregationOutcome::Rejected(ref error) if error.field() == "record cost"
        ));
        let (usage_index, sessions) = accumulator.into_generation_parts().unwrap();
        assert_eq!(
            usage_index
                .project_models(&crate::GroupBy::Model)
                .unwrap()
                .total_tokens,
            0
        );
        assert!(sessions.is_empty());
    }
}
