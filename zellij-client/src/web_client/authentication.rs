use crate::web_client::utils::parse_cookies;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, SET_COOKIE};
use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use axum_extra::extract::cookie::{Cookie, SameSite};
use zellij_utils::web_authentication_tokens::{
    hash_token, is_session_token_read_only, refresh_session_token, validate_session_token,
};

#[derive(Clone)]
pub struct SessionTokenHash(pub String);

#[derive(Clone, Copy)]
pub struct IsReadOnly(pub bool);

/// The session token a request presented, from whichever of the two places carries it.
///
/// The cookie is the browser's channel and is tried first, so nothing about a browser session
/// changes. `Authorization: Bearer <session token>` is the same credential in a header, for a
/// caller that has no cookie jar - a script, or anything driving the server over HTTP. It is the
/// SAME token: the header is not a second way to authenticate, only a second way to present what
/// `/command/login` already issued, and it is validated by the same call.
fn presented_session_token(request: &Request) -> Option<String> {
    if let Some(token) = parse_cookies(request).get("session_token") {
        return Some(token.clone());
    }
    let header = request.headers().get(AUTHORIZATION)?.to_str().ok()?;
    // the scheme is matched case-insensitively, as RFC 7235 requires
    let (scheme, token) = header.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

pub async fn auth_middleware(request: Request, next: Next) -> Result<Response, StatusCode> {
    let session_token = match presented_session_token(&request) {
        Some(token) => token,
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    match validate_session_token(&session_token) {
        Ok(true) => {
            // a short-lived token lives five minutes from when it was last used rather than from
            // when it was issued, so a session someone is actually using is not logged out
            // mid-use. An expired token matches nothing here and stays expired
            let _ = refresh_session_token(&session_token);

            // Check if this is a read-only token
            let is_read_only = is_session_token_read_only(&session_token).unwrap_or(true);

            // Compute session token hash for client ownership verification
            let session_token_hash = hash_token(&session_token);

            // Store in request extensions for downstream handlers
            let mut request = request;
            request.extensions_mut().insert(IsReadOnly(is_read_only));
            request
                .extensions_mut()
                .insert(SessionTokenHash(session_token_hash));

            let response = next.run(request).await;
            Ok(response)
        },
        Ok(false) | Err(_) => {
            // revoke session_token as if it exists it's no longer valid
            let mut response = Response::builder()
                .status(StatusCode::UNAUTHORIZED)
                .body(Body::empty())
                .unwrap();

            // Clear both secure and non-secure versions
            // in case the user was on http before and is now on https
            // or vice versa
            let clear_cookies = [
                Cookie::build(("session_token", ""))
                    .http_only(true)
                    .secure(false)
                    .same_site(SameSite::Strict)
                    .path("/")
                    .max_age(time::Duration::seconds(0))
                    .build(),
                Cookie::build(("session_token", ""))
                    .http_only(true)
                    .secure(true)
                    .same_site(SameSite::Strict)
                    .path("/")
                    .max_age(time::Duration::seconds(0))
                    .build(),
            ];

            for cookie in clear_cookies {
                response
                    .headers_mut()
                    .append(SET_COOKIE, cookie.to_string().parse().unwrap());
            }

            Ok(response)
        },
    }
}
