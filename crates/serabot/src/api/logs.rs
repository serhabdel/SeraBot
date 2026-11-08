//! The Logs viewer endpoint (0.28.0, phase C4): `GET /v1/logs?level=&q=`
//! reads the redacting in-memory ring buffer installed by
//! [`crate::logging`]. Owner only — a member gets an explicit 403, and the
//! buffer itself never holds unredacted secrets.

use super::*;

/// `GET /v1/logs`: ring-buffer snapshot, admin (owner) only, newest first.
pub(super) async fn list_logs(
    Extension(user): Extension<ActingUser>,
    Query(query): Query<crate::logging::LogsQuery>,
) -> Result<Json<Value>> {
    // `logs_handler` gates on the admin role itself.
    crate::logging::logs_handler(Extension(user), Query(query)).await
}
