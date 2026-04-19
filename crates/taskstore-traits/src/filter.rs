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

impl FilterOp {
    #[allow(dead_code)]
    pub(crate) fn to_sql(self) -> &'static str {
        match self {
            FilterOp::Eq => "=",
            FilterOp::Ne => "!=",
            FilterOp::Gt => ">",
            FilterOp::Lt => "<",
            FilterOp::Gte => ">=",
            FilterOp::Lte => "<=",
            FilterOp::Contains => "LIKE",
        }
    }
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
    fn test_filter_op_to_sql() {
        assert_eq!(FilterOp::Eq.to_sql(), "=");
        assert_eq!(FilterOp::Ne.to_sql(), "!=");
        assert_eq!(FilterOp::Gt.to_sql(), ">");
        assert_eq!(FilterOp::Lt.to_sql(), "<");
        assert_eq!(FilterOp::Gte.to_sql(), ">=");
        assert_eq!(FilterOp::Lte.to_sql(), "<=");
        assert_eq!(FilterOp::Contains.to_sql(), "LIKE");
    }

    #[test]
    fn test_filter_op_display() {
        assert_eq!(FilterOp::Eq.to_string(), "=");
        assert_eq!(FilterOp::Ne.to_string(), "!=");
    }
}
