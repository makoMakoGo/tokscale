use tokenx_engine::inferred_provider_from_model;

/// Stable model-family identity derived exclusively from a canonical model id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(usize)]
pub(crate) enum ModelFamily {
    Gpt,
    Claude,
    Gemini,
    Xai,
    Glm,
    Deepseek,
    Qwen,
    Kimi,
    Minimax,
    Mimo,
    Mistral,
    Unknown,
}

impl ModelFamily {
    // Unknown is the terminal family so the closed-set array size has one
    // source of truth.
    pub(crate) const COUNT: usize = Self::Unknown as usize + 1;
    pub(crate) const ALL: [Self; Self::COUNT] = [
        Self::Gpt,
        Self::Claude,
        Self::Gemini,
        Self::Xai,
        Self::Glm,
        Self::Deepseek,
        Self::Qwen,
        Self::Kimi,
        Self::Minimax,
        Self::Mimo,
        Self::Mistral,
        Self::Unknown,
    ];

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn from_model_id(model_id: &str) -> Self {
        match inferred_provider_from_model(model_id) {
            Some("openai") => Self::Gpt,
            Some("anthropic") => Self::Claude,
            Some("google") => Self::Gemini,
            Some("xai") => Self::Xai,
            Some("zai") => Self::Glm,
            Some("deepseek") => Self::Deepseek,
            Some("qwen") => Self::Qwen,
            Some("kimi") => Self::Kimi,
            Some("minimax") => Self::Minimax,
            Some("xiaomi") => Self::Mimo,
            Some("mistral") => Self::Mistral,
            _ => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_covers_every_colored_family() {
        for (model_id, family) in [
            ("gpt-5.5", ModelFamily::Gpt),
            ("codex-mini-latest", ModelFamily::Gpt),
            ("o3", ModelFamily::Gpt),
            ("claude-opus-4-7", ModelFamily::Claude),
            ("amazon-bedrock/claude-opus-4.6", ModelFamily::Claude),
            ("gemini-2.5-pro", ModelFamily::Gemini),
            ("grok-code-fast-1", ModelFamily::Xai),
            ("composer-2.5", ModelFamily::Xai),
            ("glm-4.6", ModelFamily::Glm),
            ("deepseek-v3.2", ModelFamily::Deepseek),
            ("qwen3-coder-plus", ModelFamily::Qwen),
            ("openrouter/qwen3-coder-plus", ModelFamily::Qwen),
            ("qwq-32b", ModelFamily::Qwen),
            ("qvq-max", ModelFamily::Qwen),
            ("kimi-k2", ModelFamily::Kimi),
            ("k3-thinking", ModelFamily::Kimi),
            ("minimax-m3", ModelFamily::Minimax),
            ("mimo-v2.5-pro", ModelFamily::Mimo),
            ("mistral-large-3", ModelFamily::Mistral),
            ("llama-4-scout", ModelFamily::Unknown),
        ] {
            assert_eq!(ModelFamily::from_model_id(model_id), family, "{model_id}");
        }
    }

    #[test]
    fn family_indices_cover_the_identity_palette_exactly_once() {
        let mut seen = [false; ModelFamily::COUNT];
        for family in ModelFamily::ALL {
            let index = family.index();
            assert!(index < ModelFamily::COUNT);
            assert!(!seen[index], "duplicate model-family index {index}");
            seen[index] = true;
        }
        assert!(seen.into_iter().all(|present| present));
    }
}
