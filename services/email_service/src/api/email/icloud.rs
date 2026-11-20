//! Linking the stack's iCloud Mail account.

use crate::api::ApiContext;
use crate::api::context::AuthorizationService;
use crate::outbound::icloud_link::{ICloudLinkError, ICloudLinks};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::get,
};
use macro_authorization::{MacroAuthorizationExtractor, UserOrInternal};
use model::response::ErrorResponse;
use utoipa::ToSchema;
use uuid::Uuid;

pub fn router() -> Router<ApiContext> {
    Router::new().route("/", get(availability).post(link))
}

/// Whether the stack offers an iCloud Mail account to link.
#[derive(Debug, serde::Serialize, serde::Deserialize, ToSchema)]
pub struct ICloudAvailability {
    /// True when the stack has an Apple ID and app-specific password.
    pub available: bool,
}

/// The linked iCloud inbox.
#[derive(Debug, serde::Serialize, serde::Deserialize, ToSchema)]
pub struct ICloudLinkResponse {
    /// The `email_links` id of the iCloud inbox.
    pub link_id: Uuid,
}

/// Whether an iCloud Mail account can be linked on this stack.
#[utoipa::path(
    get,
    tag = "Init",
    path = "/email/icloud",
    operation_id = "icloud_availability",
    responses((status = 200, body = ICloudAvailability), (status = 401, body = ErrorResponse))
)]
pub(crate) async fn availability(
    _authorization: MacroAuthorizationExtractor<AuthorizationService, UserOrInternal>,
) -> Json<ICloudAvailability> {
    Json(ICloudAvailability {
        available: ICloudLinks::available(),
    })
}

/// Link the stack's iCloud Mail account to the caller and start its sync.
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
#[tracing::instrument(skip(ctx, authorization), fields(user_id = authorization.authorization.user.user_context.user_id))]
pub(crate) async fn link(
    State(ctx): State<ApiContext>,
    authorization: MacroAuthorizationExtractor<AuthorizationService, UserOrInternal>,
) -> Response {
    let user = &authorization.authorization.user;
    let result = ctx
        .icloud_links
        .link(
            user.macro_user_id.as_ref(),
            &user.user_context.fusion_user_id,
        )
        .await;
    match result {
        Ok(link_id) => Json(ICloudLinkResponse { link_id }).into_response(),
        Err(error) => {
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
    }
}
