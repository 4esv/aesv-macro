//! Turning on mail sync for the stack's iCloud Mail account.

use crate::outbound::icloud_link::{ICloudLinkError, ICloudLinks};
use axum::{
    Router,
    extract::{FromRef, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::get,
};
use macro_authorization::{
    MacroAuthorizationExtractor, MacroAuthorizationService, MacroAuthorizationState, UserOrInternal,
};
use model::response::ErrorResponse;
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

#[cfg(test)]
mod test;

/// The routes, generic over the state and the authorization service so a test
/// can serve them without a JWT validator.
pub fn router<S, Svc>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    Svc: MacroAuthorizationService,
    Arc<ICloudLinks>: FromRef<S>,
    MacroAuthorizationState<Svc>: FromRef<S>,
{
    Router::new().route("/", get(availability::<Svc>).post(link::<Svc>))
}

/// Whether the caller can link the stack's iCloud Mail account.
#[derive(Debug, serde::Serialize, serde::Deserialize, ToSchema)]
pub struct ICloudAvailability {
    /// True when the stack has an iCloud login and its link row names the caller.
    pub available: bool,
}

/// The linked iCloud inbox.
#[derive(Debug, serde::Serialize, serde::Deserialize, ToSchema)]
pub struct ICloudLinkResponse {
    /// The `email_links` id of the iCloud inbox.
    pub link_id: Uuid,
}

/// Whether the caller can link the stack's iCloud Mail account.
#[utoipa::path(
    get,
    tag = "Init",
    path = "/email/icloud",
    operation_id = "icloud_availability",
    responses(
        (status = 200, body = ICloudAvailability),
        (status = 401, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    )
)]
pub(crate) async fn availability<Svc: MacroAuthorizationService>(
    State(links): State<Arc<ICloudLinks>>,
    authorization: MacroAuthorizationExtractor<Svc, UserOrInternal>,
) -> Response {
    let user = &authorization.authorization.user;
    match links.available(user.macro_user_id.as_ref()).await {
        Ok(available) => Json(ICloudAvailability { available }).into_response(),
        Err(error) => respond(ICloudLinkError::Database(error)),
    }
}

/// Turn on mail sync for the caller's iCloud link. Only the user the stack's
/// connect script named may link it.
#[utoipa::path(
    post,
    tag = "Init",
    path = "/email/icloud",
    operation_id = "link_icloud",
    responses(
        (status = 200, body = ICloudLinkResponse),
        (status = 401, body = ErrorResponse),
        (status = 404, body = ErrorResponse),
        (status = 409, body = ErrorResponse),
        (status = 502, body = ErrorResponse),
        (status = 503, body = ErrorResponse),
        (status = 500, body = ErrorResponse),
    )
)]
#[tracing::instrument(skip(links, authorization), fields(user_id = authorization.authorization.user.user_context.user_id))]
pub(crate) async fn link<Svc: MacroAuthorizationService>(
    State(links): State<Arc<ICloudLinks>>,
    authorization: MacroAuthorizationExtractor<Svc, UserOrInternal>,
) -> Response {
    let user = &authorization.authorization.user;
    match links.link(user.macro_user_id.as_ref()).await {
        Ok(link_id) => Json(ICloudLinkResponse { link_id }).into_response(),
        Err(error) => respond(error),
    }
}

fn respond(error: ICloudLinkError) -> Response {
    let status = match &error {
        ICloudLinkError::NotConfigured => StatusCode::NOT_FOUND,
        ICloudLinkError::Taken => StatusCode::CONFLICT,
        ICloudLinkError::Refused => StatusCode::BAD_GATEWAY,
        ICloudLinkError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        ICloudLinkError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status.is_server_error() {
        tracing::error!(%error, "iCloud link failed");
    }
    (
        status,
        Json(ErrorResponse {
            message: error.to_string().into(),
        }),
    )
        .into_response()
}
