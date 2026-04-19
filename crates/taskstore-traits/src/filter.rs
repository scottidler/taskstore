// Query filtering for generic records

use crate::record::IndexValue;

/// Filter for querying records
#[derive(Debug, Clone)]
pub struct Filter {
    /// Field name to filter on
    pub field: String,
    /// Comparison operator
    pub op: FilterOp,
    /// Value to compare against
    pub value: IndexValue,
}

/// Comparison operators for filtering
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterOp {
    Eq,       // ==
    Ne,       // !=
    Gt,       // >
    Lt,       // <
    Gte,      // >=
    Lte,      // <=
    Contains, // LIKE %value%
}

impl std::fmt::Display for FilterOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterOp::Eq => write!(f, "="),
            FilterOp::Ne => write!(f, "!="),
            FilterOp::Gt => write!(f, ">"),
            FilterOp::Lt => write!(f, "<"),
            FilterOp::Gte => write!(f, ">="),
            FilterOp::Lte => write!(f, "<="),
            FilterOp::Contains => write!(f, "LIKE"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_creation() {
        let filter = Filter {
            field: "status".to_string(),
            op: FilterOp::Eq,
            value: IndexValue::String("active".to_string()),
        };

        assert_eq!(filter.field, "status");
        assert_eq!(filter.op, FilterOp::Eq);
    }

    #[test]
    fn test_filter_op_display() {
        assert_eq!(FilterOp::Eq.to_string(), "=");
        assert_eq!(FilterOp::Ne.to_string(), "!=");
    }
}
