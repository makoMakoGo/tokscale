use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeName {
    Green,
    Halloween,
    Teal,
    Blue,
    Pink,
    Purple,
    Orange,
    Monochrome,
    YlGnBu,
    Graphite,
    Lagoon,
    Dusk,
}

impl ThemeName {
    pub(crate) fn all() -> &'static [ThemeName] {
        &[
            ThemeName::Green,
            ThemeName::Halloween,
            ThemeName::Teal,
            ThemeName::Blue,
            ThemeName::Pink,
            ThemeName::Purple,
            ThemeName::Orange,
            ThemeName::Monochrome,
            ThemeName::YlGnBu,
            ThemeName::Graphite,
            ThemeName::Lagoon,
            ThemeName::Dusk,
        ]
    }

    pub(crate) fn next(self) -> ThemeName {
        let themes = Self::all();
        let idx = themes.iter().position(|&theme| theme == self).unwrap_or(0);
        themes[(idx + 1) % themes.len()]
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            ThemeName::Green => "green",
            ThemeName::Halloween => "halloween",
            ThemeName::Teal => "teal",
            ThemeName::Blue => "blue",
            ThemeName::Pink => "pink",
            ThemeName::Purple => "purple",
            ThemeName::Orange => "orange",
            ThemeName::Monochrome => "monochrome",
            ThemeName::YlGnBu => "ylgnbu",
            ThemeName::Graphite => "graphite",
            ThemeName::Lagoon => "lagoon",
            ThemeName::Dusk => "dusk",
        }
    }

    fn from_canonical(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|theme| theme.as_str() == value)
    }
}

impl std::str::FromStr for ThemeName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_canonical(s).ok_or_else(|| {
            format!(
                "invalid theme `{s}`; expected one of: {}",
                Self::all()
                    .iter()
                    .map(|theme| theme.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }
}

impl Serialize for ThemeName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ThemeName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::from_canonical(&value).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown theme `{value}`; expected one of: {}",
                Self::all()
                    .iter()
                    .map(|theme| theme.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
    }
}
