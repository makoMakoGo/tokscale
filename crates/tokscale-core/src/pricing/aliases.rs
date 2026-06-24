use once_cell::sync::Lazy;
use std::collections::HashMap;

static MODEL_ALIASES: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert("big-pickle", "glm-4.7");
    m.insert("big pickle", "glm-4.7");
    m.insert("bigpickle", "glm-4.7");
    m
});

pub fn resolve_alias(model_id: &str) -> Option<&'static str> {
    let lower = model_id.to_lowercase();
    MODEL_ALIASES.get(lower.as_str()).copied()
}

#[cfg(test)]
mod tests {
    use super::resolve_alias;

    #[test]
    fn resolves_only_personal_price_equivalents() {
        assert_eq!(resolve_alias("big-pickle"), Some("glm-4.7"));
        assert_eq!(resolve_alias("BIG PICKLE"), Some("glm-4.7"));
        assert_eq!(resolve_alias("bigpickle"), Some("glm-4.7"));
        assert_eq!(resolve_alias("k2p6"), None);
        assert_eq!(resolve_alias("model_placeholder_m37"), None);
        assert_eq!(resolve_alias("claude-opus-4.6-thinking"), None);
        assert_eq!(resolve_alias("grok-composer-2.5-fast"), None);
    }
}
