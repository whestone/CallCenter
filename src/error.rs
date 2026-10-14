use axum::{
    extract::rejection::{FormRejection, JsonRejection, PathRejection, QueryRejection},
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("Nicht gefunden")]
    NotFound,
    #[error("Nicht angemeldet")]
    Unauthorized,
    #[error("Keine Berechtigung")]
    Forbidden,
    #[error("{0}")]
    Unprocessable(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub type AppResult<T> = Result<T, AppError>;

/// Extractoren, deren Ablehnungen (kaputte URL, Formular, JSON) als `AppError`
/// im API-Format statt als roher Text beantwortet werden.
pub mod api_extract {
    use axum::extract::{FromRequest, FromRequestParts};

    use super::AppError;

    #[derive(FromRequestParts)]
    #[from_request(via(axum::extract::Path), rejection(AppError))]
    pub struct Path<T>(pub T);

    #[derive(FromRequestParts)]
    #[from_request(via(axum::extract::Query), rejection(AppError))]
    pub struct Query<T>(pub T);

    #[derive(FromRequest)]
    #[from_request(via(axum::Json), rejection(AppError))]
    pub struct Json<T>(pub T);
}

impl AppError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        AppError::Unprocessable(msg.into())
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Nutzerfreundliche Meldung; interne Fehler werden geloggt statt angezeigt.
    pub fn public_message(&self) -> String {
        match self {
            AppError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                "Interner Fehler".into()
            }
            other => other.to_string(),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::NotFound,
            sqlx::Error::Database(ref d) if d.is_unique_violation() => {
                AppError::invalid("Ein Datensatz mit diesen Werten existiert bereits")
            }
            sqlx::Error::Database(ref d) if d.is_foreign_key_violation() => {
                AppError::invalid("Verweis auf einen nicht existierenden Datensatz")
            }
            other => AppError::Internal(other.into()),
        }
    }
}

impl From<PathRejection> for AppError {
    fn from(_: PathRejection) -> Self {
        AppError::NotFound
    }
}

impl From<FormRejection> for AppError {
    fn from(e: FormRejection) -> Self {
        AppError::invalid(format!("Ungültige Formulareingabe ({})", e.body_text()))
    }
}

impl From<QueryRejection> for AppError {
    fn from(e: QueryRejection) -> Self {
        AppError::invalid(format!("Ungültige Parameter ({})", e.body_text()))
    }
}

impl From<JsonRejection> for AppError {
    fn from(e: JsonRejection) -> Self {
        AppError::invalid(format!("Ungültiges JSON ({})", e.body_text()))
    }
}

impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> Self {
        AppError::Internal(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = match &self {
            AppError::NotFound => "RecordNotFound",
            AppError::Unauthorized => "Unauthorized",
            AppError::Forbidden => "Forbidden",
            AppError::Unprocessable(_) => "RecordInvalid",
            AppError::Internal(_) => "InternalError",
        };
        let unauthorized = matches!(self, AppError::Unauthorized);
        let mut resp = (
            status,
            Json(json!({ "error": code, "description": self.public_message() })),
        )
            .into_response();
        if unauthorized {
            resp.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"callcenter\""),
            );
        }
        resp
    }
}
