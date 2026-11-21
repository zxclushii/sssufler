use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum AppError {
    #[error("claude не запущен, нажми Enter в терминале")]
    NotRunning,
    #[error("не хватает места на диске, нужно {0} МБ")]
    StorageFull(u64),
    #[error("{0}: файл повредился при загрузке")]
    HashMismatch(String),
}

#[cfg(windows)]
#[derive(Debug, Serialize)]
pub(crate) struct CmdError {
    code: &'static str,
    message: String,
}

#[cfg(windows)]
impl From<anyhow::Error> for CmdError {
    fn from(err: anyhow::Error) -> Self {
        let code = match err.downcast_ref::<AppError>() {
            Some(AppError::NotRunning) => "NotRunning",
            Some(AppError::StorageFull(_)) => "StorageFull",
            Some(AppError::HashMismatch(_)) => "HashMismatch",
            None => "Error",
        };
        let message = format!("{err:#}");
        tracing::warn!(code, %message);
        Self { code, message }
    }
}
