use crate::TokenBreakdown;

const DENOMINATOR: u64 = 26_981_413_964;
const NUMERATORS: [u64; 5] = [
    2_182_896_619,
    112_659_190,
    24_546_162_069,
    104_142_575,
    35_553_511,
];
const TOTAL_OVERFLOW_MESSAGE: &str = "total-only token imputation totals exceed i64::MAX";

pub(crate) fn impute_total_only_token_breakdown(total: i64) -> TokenBreakdown {
    if total <= 0 {
        return TokenBreakdown::default();
    }

    debug_assert_eq!(NUMERATORS.iter().sum::<u64>(), DENOMINATOR);

    let total_u = total as u64;
    let mut values = [0_u64; 5];
    let mut remainders = [0_u64; 5];
    let mut allocated = 0_u64;

    for (idx, numerator) in NUMERATORS.iter().copied().enumerate() {
        let product = u128::from(total_u) * u128::from(numerator);
        values[idx] = (product / u128::from(DENOMINATOR)) as u64;
        remainders[idx] = (product % u128::from(DENOMINATOR)) as u64;
        allocated += values[idx];
    }

    let remaining = (total_u - allocated) as usize;
    let mut order = [0_usize, 1, 2, 3, 4];
    order.sort_by(|left, right| {
        remainders[*right]
            .cmp(&remainders[*left])
            .then_with(|| left.cmp(right))
    });
    for idx in order.into_iter().take(remaining) {
        values[idx] += 1;
    }

    TokenBreakdown {
        input: values[0] as i64,
        output: values[1] as i64,
        cache_read: values[2] as i64,
        cache_write: values[3] as i64,
        reasoning: values[4] as i64,
    }
}

pub(crate) fn impute_total_only_token_breakdowns(totals: &[i64]) -> Vec<TokenBreakdown> {
    if totals.is_empty() {
        return Vec::new();
    }

    let total_sum = totals
        .iter()
        .copied()
        .filter(|total| *total > 0)
        .try_fold(0_i64, |acc, total| acc.checked_add(total))
        .expect(TOTAL_OVERFLOW_MESSAGE);
    let target = impute_total_only_token_breakdown(total_sum);
    let target_values = [
        target.input as u64,
        target.output as u64,
        target.cache_read as u64,
        target.cache_write as u64,
        target.reasoning as u64,
    ];

    let mut rows = Vec::with_capacity(totals.len());
    let mut column_values = [0_u64; 5];

    for total in totals.iter().copied() {
        if total <= 0 {
            rows.push(([0_u64; 5], [0_u64; 5], 0_usize));
            continue;
        }

        let total_u = total as u64;
        let mut values = [0_u64; 5];
        let mut remainders = [0_u64; 5];
        let mut allocated = 0_u64;
        for (bucket_idx, numerator) in NUMERATORS.iter().copied().enumerate() {
            let product = u128::from(total_u) * u128::from(numerator);
            values[bucket_idx] = (product / u128::from(DENOMINATOR)) as u64;
            remainders[bucket_idx] = (product % u128::from(DENOMINATOR)) as u64;
            allocated += values[bucket_idx];
            column_values[bucket_idx] += values[bucket_idx];
        }

        rows.push((values, remainders, (total_u - allocated) as usize));
    }

    let mut bucket_remaining = [0_u64; 5];
    for (bucket_idx, target) in target_values.iter().copied().enumerate() {
        bucket_remaining[bucket_idx] = target
            .checked_sub(column_values[bucket_idx])
            .expect("total-only token imputation column floor exceeded batch target");
    }

    let mut row_order: Vec<usize> = (0..rows.len()).collect();
    row_order.sort_by(|left, right| {
        rows[*right]
            .2
            .cmp(&rows[*left].2)
            .then_with(|| left.cmp(right))
    });

    for row_idx in row_order {
        let needed = rows[row_idx].2;
        if needed == 0 {
            continue;
        }

        let mut buckets = [0_usize, 1, 2, 3, 4];
        buckets.sort_by(|left, right| {
            bucket_remaining[*right]
                .cmp(&bucket_remaining[*left])
                .then_with(|| rows[row_idx].1[*right].cmp(&rows[row_idx].1[*left]))
                .then_with(|| left.cmp(right))
        });

        for bucket_idx in buckets {
            if rows[row_idx].2 == 0 {
                break;
            }
            if bucket_remaining[bucket_idx] == 0 {
                continue;
            }
            rows[row_idx].0[bucket_idx] += 1;
            rows[row_idx].2 -= 1;
            bucket_remaining[bucket_idx] -= 1;
        }
    }

    assert!(
        rows.iter().all(|row| row.2 == 0),
        "token imputation failed to distribute all row remainders"
    );
    assert!(
        bucket_remaining.iter().all(|remaining| *remaining == 0),
        "token imputation left residual bucket capacity"
    );

    rows.into_iter()
        .map(|(values, _, _)| TokenBreakdown {
            input: values[0] as i64,
            output: values[1] as i64,
            cache_read: values[2] as i64,
            cache_write: values[3] as i64,
            reasoning: values[4] as i64,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imputes_warp_sample_with_fixed_local_history_ratios() {
        let tokens = impute_total_only_token_breakdown(37_204_845);

        assert_eq!(
            tokens,
            TokenBreakdown {
                input: 3_010_010,
                output: 155_346,
                cache_read: 33_846_861,
                cache_write: 143_603,
                reasoning: 49_025,
            }
        );
        assert_eq!(tokens.total(), 37_204_845);
    }

    #[test]
    fn imputed_total_always_matches_reported_total() {
        for total in [1, 2, 3, 10, 999, 123_456_789] {
            assert_eq!(impute_total_only_token_breakdown(total).total(), total);
        }
    }

    #[test]
    fn batch_imputation_preserves_row_and_batch_bucket_totals() {
        let totals = [20_802_120, 13_108_837, 2_019_058, 1_182_533, 64_058, 28_239];
        let rows = impute_total_only_token_breakdowns(&totals);

        assert_eq!(rows.len(), totals.len());
        for (tokens, total) in rows.iter().zip(totals) {
            assert_eq!(tokens.total(), total);
        }

        let mut aggregate = TokenBreakdown::default();
        for tokens in rows {
            aggregate = aggregate.checked_add(&tokens).unwrap();
        }

        assert_eq!(
            aggregate,
            impute_total_only_token_breakdown(totals.into_iter().sum())
        );
    }

    #[test]
    fn batch_imputation_handles_current_warp_row_remainders() {
        let totals = [
            767_365, 53_345, 2_384_452, 2_535, 594_164, 172_321, 793_529, 59_372, 76_971, 300_645,
            64_058, 6_656, 1_729_435, 282_135, 309_976, 2_232, 467_448, 2_019_058, 116_403,
            13_765_082, 14_064, 13_108_837, 112_010, 2_752,
        ];
        let rows = impute_total_only_token_breakdowns(&totals);

        for (tokens, total) in rows.iter().zip(totals) {
            assert_eq!(tokens.total(), total);
        }

        let mut aggregate = TokenBreakdown::default();
        for tokens in rows {
            aggregate = aggregate.checked_add(&tokens).unwrap();
        }

        assert_eq!(
            aggregate,
            TokenBreakdown {
                input: 3_010_010,
                output: 155_346,
                cache_read: 33_846_861,
                cache_write: 143_603,
                reasoning: 49_025,
            }
        );
    }

    #[test]
    fn non_positive_totals_do_not_emit_tokens() {
        assert_eq!(
            impute_total_only_token_breakdown(0),
            TokenBreakdown::default()
        );
        assert_eq!(
            impute_total_only_token_breakdown(-1),
            TokenBreakdown::default()
        );
    }

    #[test]
    #[should_panic(expected = "total-only token imputation totals exceed i64::MAX")]
    fn batch_imputation_panics_on_total_sum_overflow() {
        let _ = impute_total_only_token_breakdowns(&[i64::MAX, 1]);
    }
}
