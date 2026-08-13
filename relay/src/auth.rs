//! Bearer-token authentication.
//!
//! One shared secret guards the whole relay. Anything holding it can post a
//! prompt into a running claurst session, which executes tools on the
//! developer's machine, so the token is treated as a command-execution
//! credential rather than a convenience.

use subtle::ConstantTimeEq;

/// Shortest token the relay will start with.
///
/// A weak secret here is a remote shell. 32 characters of a generated token
/// puts brute force out of reach without making the value unmanageable.
pub const MIN_TOKEN_LEN: usize = 32;

/// Why a configured token was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    Missing,
    TooShort { len: usize },
}

impl std::fmt::Display for TokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(
                f,
                "no relay token configured; set RELAY_TOKEN to at least {MIN_TOKEN_LEN} characters"
            ),
            Self::TooShort { len } => write!(
                f,
                "relay token is {len} characters; at least {MIN_TOKEN_LEN} are required, because \
                 this token grants command execution on the connected machine"
            ),
        }
    }
}

impl std::error::Error for TokenError {}

/// Accept a token only if it is long enough to be worth having.
pub fn validate_token(token: &str) -> Result<&str, TokenError> {
    let token = token.trim();
    if token.is_empty() {
        return Err(TokenError::Missing);
    }
    let len = token.chars().count();
    if len < MIN_TOKEN_LEN {
        return Err(TokenError::TooShort { len });
    }
    Ok(token)
}

/// Compare a presented token against the configured one in constant time.
///
/// A short-circuiting `==` leaks the length of the matching prefix through
/// timing, which turns a brute-force search into a per-character one.
pub fn token_matches(configured: &str, presented: &str) -> bool {
    let configured = configured.as_bytes();
    let presented = presented.as_bytes();
    if configured.len() != presented.len() {
        // Still run a comparison so an early return does not itself leak the
        // length, then discard the result.
        let _ = configured.ct_eq(configured);
        return false;
    }
    configured.ct_eq(presented).into()
}

/// Pull the bearer token out of an `Authorization` header value.
pub fn bearer_from_header(value: &str) -> Option<&str> {
    let rest = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

/// Name of the cookie the web page authenticates with.
pub const COOKIE_NAME: &str = "relay_token";

/// Pull the token out of a `Cookie` header value.
///
/// The browser `EventSource` API cannot set request headers, so the SSE
/// endpoint is unreachable with a bearer token alone and has to accept a
/// cookie as well.
pub fn token_from_cookies(header: &str) -> Option<&str> {
    header.split(';').find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name.trim() == COOKIE_NAME)
            .then(|| value.trim())
            .filter(|value| !value.is_empty())
    })
}

/// Cookie attributes for the authenticated session.
///
/// `HttpOnly` keeps page scripts from reading the token back out, and
/// `SameSite=Strict` stops another site from driving the relay through the
/// browser's ambient credentials.
pub fn session_cookie(token: &str) -> String {
    format!("{COOKIE_NAME}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_token_is_rejected() {
        assert_eq!(validate_token(""), Err(TokenError::Missing));
        assert_eq!(validate_token("   "), Err(TokenError::Missing));
    }

    #[test]
    fn a_short_token_is_rejected_with_its_length() {
        assert_eq!(
            validate_token("hunter2"),
            Err(TokenError::TooShort { len: 7 })
        );
    }

    #[test]
    fn a_token_of_exactly_the_minimum_is_accepted() {
        let token = "a".repeat(MIN_TOKEN_LEN);
        assert_eq!(validate_token(&token), Ok(token.as_str()));
    }

    #[test]
    fn the_rejection_message_says_why_the_length_matters() {
        let message = TokenError::TooShort { len: 4 }.to_string();
        assert!(
            message.contains("command execution"),
            "the operator has to understand the stake, got: {message}"
        );
    }

    #[test]
    fn matching_is_exact() {
        let token = "a".repeat(MIN_TOKEN_LEN);
        assert!(token_matches(&token, &token));
        assert!(!token_matches(&token, &token[..MIN_TOKEN_LEN - 1]));
        assert!(!token_matches(&token, &format!("{token}x")));
        assert!(!token_matches(&token, ""));
    }

    #[test]
    fn a_bearer_header_yields_the_token() {
        assert_eq!(bearer_from_header("Bearer abc"), Some("abc"));
        assert_eq!(bearer_from_header("bearer abc"), Some("abc"));
        assert_eq!(bearer_from_header("Bearer  abc  "), Some("abc"));
    }

    #[test]
    fn a_header_without_a_token_yields_nothing() {
        assert_eq!(bearer_from_header("Bearer "), None);
        assert_eq!(bearer_from_header("Basic abc"), None);
        assert_eq!(bearer_from_header(""), None);
    }
}
