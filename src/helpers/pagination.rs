use crate::error::AppError;

/// Default page size for list endpoints.
pub const DEFAULT_LIMIT: u32 = 30;
/// Upper bound accepted for `?limit=`.
pub const MAX_LIMIT: u32 = 100;

/// Validated `?page=&limit=` pair, shared by every list endpoint.
#[derive(Debug, Clone, Copy)]
pub struct Pagination {
    pub page: u32,
    pub limit: u32,
    pub offset: i64,
}

impl Pagination {
    pub fn new(page: Option<u32>, limit: Option<u32>) -> Result<Self, AppError> {
        let page = page.unwrap_or(1);
        let limit = limit.unwrap_or(DEFAULT_LIMIT);

        if page == 0 {
            return Err(AppError::BadRequest("page must be >= 1".into()));
        }
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(AppError::BadRequest(format!(
                "limit must be between 1 and {MAX_LIMIT}"
            )));
        }

        Ok(Self {
            page,
            limit,
            offset: i64::from(page - 1) * i64::from(limit),
        })
    }

    /// Total number of pages, rounded up; `0` for an empty result set.
    pub fn pages(self, total: i64) -> i64 {
        (total + i64::from(self.limit) - 1) / i64::from(self.limit)
    }
}
