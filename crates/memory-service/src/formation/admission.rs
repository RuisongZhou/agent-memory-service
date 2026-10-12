//! Scope validation shared by explicit writes and both memory algorithms.
use crate::types::*;

pub(crate) fn required_scope(req: &AddMemoriesReq) -> ApiResult<(&str, &str)> {
    let project = req
        .scope
        .project
        .as_deref()
        .filter(|s| !s.trim().is_empty() && *s != "*")
        .ok_or_else(|| ApiError::InvalidArgument("scope.project is required".into()))?;
    let session = req
        .scope
        .session
        .as_deref()
        .filter(|s| !s.trim().is_empty() && *s != "*")
        .ok_or_else(|| ApiError::InvalidArgument("scope.session is required".into()))?;
    Ok((project, session))
}
