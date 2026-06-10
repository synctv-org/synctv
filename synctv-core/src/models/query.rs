use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SortDirection {
    Asc,
    #[default]
    Desc,
}

impl SortDirection {
    #[must_use]
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Asc => "ASC",
            Self::Desc => "DESC",
        }
    }
}

impl FromStr for SortDirection {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "asc" => Ok(Self::Asc),
            "desc" => Ok(Self::Desc),
            other => Err(format!("Unknown sort direction: {other}")),
        }
    }
}

impl std::fmt::Display for SortDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SortDirection;

    fn parse_sort_direction(value: &str) -> SortDirection {
        match value.parse::<SortDirection>() {
            Ok(direction) => direction,
            Err(error) => std::panic::panic_any(format!("sort direction should parse: {error}")),
        }
    }

    #[test]
    fn parse_sort_direction_values() {
        assert_eq!(parse_sort_direction("asc"), SortDirection::Asc);
        assert_eq!(parse_sort_direction("desc"), SortDirection::Desc);
        assert!("ascending".parse::<SortDirection>().is_err());
        assert!("descending".parse::<SortDirection>().is_err());
    }
}
