pub(crate) const DEEPSEEK_V4_PRO_BETA_ALIAS: &str = "model1";
pub(crate) const DEEPSEEK_V4_FLASH_BETA_ALIAS: &str = "model2";

pub(crate) fn is_deepseek_v4_beta_alias(model: &str) -> bool {
    let lower = model.trim().to_lowercase();
    let model_part = lower
        .trim_end_matches('/')
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(&lower);

    matches!(
        model_part,
        DEEPSEEK_V4_PRO_BETA_ALIAS | DEEPSEEK_V4_FLASH_BETA_ALIAS
    )
}
