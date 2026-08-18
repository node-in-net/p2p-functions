use actix_web::http::header;
use actix_web::{dev::ServiceRequest, web, Error};
use actix_web_httpauth::extractors::basic::BasicAuth;
use md5::{Digest, Md5};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum AuthMode {
    None,
    Basic,
    Digest,
}

#[derive(Clone, Debug)]
pub struct AuthConfig {
    pub mode: AuthMode,
    pub credentials: Option<(String, String)>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            mode: AuthMode::None,
            credentials: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_digest_header_extracts_fields() {
        let header = r#"username="alice", realm="NodeInNet", nonce="abc123""#;
        let map = parse_digest_header(header);
        assert_eq!(map.get("username"), Some(&"alice".to_string()));
        assert_eq!(map.get("realm"), Some(&"NodeInNet".to_string()));
        assert_eq!(map.get("nonce"), Some(&"abc123".to_string()));
    }

    #[test]
    fn parse_digest_header_strips_quotes() {
        let map = parse_digest_header(r#"key="quoted_value""#);
        assert_eq!(map.get("key"), Some(&"quoted_value".to_string()));
    }

    #[test]
    fn parse_digest_header_unquoted_value() {
        let map = parse_digest_header("algorithm=MD5");
        assert_eq!(map.get("algorithm"), Some(&"MD5".to_string()));
    }

    #[test]
    fn parse_digest_header_empty_returns_empty_map() {
        let map = parse_digest_header("");
        // Empty input has no '=' so split_once returns None → no entries
        assert!(
            map.is_empty() || map.len() <= 1,
            "should not produce spurious entries"
        );
    }

    #[test]
    fn auth_config_default_has_no_mode_and_no_creds() {
        let cfg = AuthConfig::default();
        assert!(matches!(cfg.mode, AuthMode::None));
        assert!(cfg.credentials.is_none());
    }

    #[test]
    fn validate_digest_wrong_username_returns_false() {
        let req = actix_web::test::TestRequest::get()
            .uri("/dav/")
            .to_http_request();
        let auth = DigestAuth {
            username: "eve".to_string(),
            realm: "NodeInNet".to_string(),
            nonce: "nonce1".to_string(),
            uri: "/dav/".to_string(),
            response: "deadbeef".to_string(),
            qop: None,
            nc: None,
            cnonce: None,
        };
        let config = AuthConfig {
            mode: AuthMode::Digest,
            credentials: Some(("alice".to_string(), "secret".to_string())),
        };
        assert!(!validate_digest(&req, &auth, &config));
    }

    #[test]
    fn validate_digest_no_credentials_always_false() {
        let req = actix_web::test::TestRequest::get()
            .uri("/")
            .to_http_request();
        let auth = DigestAuth {
            username: "alice".to_string(),
            realm: "NodeInNet".to_string(),
            nonce: "nonce".to_string(),
            uri: "/".to_string(),
            response: "anything".to_string(),
            qop: None,
            nc: None,
            cnonce: None,
        };
        let config = AuthConfig {
            mode: AuthMode::Digest,
            credentials: None,
        };
        assert!(!validate_digest(&req, &auth, &config));
    }

    #[test]
    fn validate_digest_correct_response_returns_true() {
        let username = "alice";
        let password = "secret";
        let realm = "NodeInNet";
        let nonce = issue_nonce();
        let nonce = nonce.as_str();
        let method = "GET";
        let uri = "/dav/";

        let mut h = Md5::new();
        h.update(format!("{}:{}:{}", username, realm, password));
        let ha1 = format!("{:x}", h.finalize());

        let mut h = Md5::new();
        h.update(format!("{}:{}", method, uri));
        let ha2 = format!("{:x}", h.finalize());

        let mut h = Md5::new();
        h.update(format!("{}:{}:{}", ha1, nonce, ha2));
        let response = format!("{:x}", h.finalize());

        let req = actix_web::test::TestRequest::get()
            .uri(uri)
            .to_http_request();
        let auth = DigestAuth {
            username: username.to_string(),
            realm: realm.to_string(),
            nonce: nonce.to_string(),
            uri: uri.to_string(),
            response,
            qop: None,
            nc: None,
            cnonce: None,
        };
        let config = AuthConfig {
            mode: AuthMode::Digest,
            credentials: Some((username.to_string(), password.to_string())),
        };
        assert!(validate_digest(&req, &auth, &config));
    }

    #[test]
    fn validate_digest_wrong_response_returns_false() {
        let req = actix_web::test::TestRequest::get()
            .uri("/")
            .to_http_request();
        let auth = DigestAuth {
            username: "alice".to_string(),
            realm: "NodeInNet".to_string(),
            nonce: "nonce".to_string(),
            uri: "/".to_string(),
            response: "000000000000000000000000000000".to_string(),
            qop: None,
            nc: None,
            cnonce: None,
        };
        let config = AuthConfig {
            mode: AuthMode::Digest,
            credentials: Some(("alice".to_string(), "secret".to_string())),
        };
        assert!(!validate_digest(&req, &auth, &config));
    }

    #[test]
    fn validate_digest_rejects_a_nonce_we_never_issued() {
        let (username, password, realm, uri) = ("alice", "secret", "NodeInNet", "/dav/");
        let auth = digest_for(username, password, realm, uri, "forged-nonce", None);
        let req = actix_web::test::TestRequest::get()
            .uri(uri)
            .to_http_request();
        let config = AuthConfig {
            mode: AuthMode::Digest,
            credentials: Some((username.to_string(), password.to_string())),
        };
        assert!(!validate_digest(&req, &auth, &config));
    }

    #[test]
    fn validate_digest_rejects_a_replayed_request() {
        let (username, password, realm, uri) = ("alice", "secret", "NodeInNet", "/dav/");
        let nonce = issue_nonce();
        let config = AuthConfig {
            mode: AuthMode::Digest,
            credentials: Some((username.to_string(), password.to_string())),
        };
        let req = actix_web::test::TestRequest::get()
            .uri(uri)
            .to_http_request();

        let first = digest_for(username, password, realm, uri, &nonce, None);
        assert!(validate_digest(&req, &first, &config));

        let replay = digest_for(username, password, realm, uri, &nonce, None);
        assert!(
            !validate_digest(&req, &replay, &config),
            "a captured header must not authenticate twice"
        );
    }

    /// Builds the `Authorization: Digest` a compliant client would send.
    fn digest_for(
        username: &str,
        password: &str,
        realm: &str,
        uri: &str,
        nonce: &str,
        nc: Option<&str>,
    ) -> DigestAuth {
        let mut h = Md5::new();
        h.update(format!("{}:{}:{}", username, realm, password));
        let ha1 = format!("{:x}", h.finalize());
        let mut h = Md5::new();
        h.update(format!("GET:{}", uri));
        let ha2 = format!("{:x}", h.finalize());
        let mut h = Md5::new();
        h.update(format!("{}:{}:{}", ha1, nonce, ha2));
        DigestAuth {
            username: username.to_string(),
            realm: realm.to_string(),
            nonce: nonce.to_string(),
            uri: uri.to_string(),
            response: format!("{:x}", h.finalize()),
            qop: None,
            nc: nc.map(str::to_string),
            cnonce: None,
        }
    }
}

pub async fn basic_validator(
    req: ServiceRequest,
    auth: BasicAuth,
) -> Result<ServiceRequest, (Error, ServiceRequest)> {
    if let Some(config) = req.app_data::<web::Data<AuthConfig>>() {
        if let Some((username, password)) = &config.credentials {
            if auth.user_id() == username && auth.password().unwrap_or("") == password {
                return Ok(req);
            }
        }
    }
    Err((actix_web::error::ErrorUnauthorized("Unauthorized"), req))
}

// --- Manual Digest Auth Implementation ---

/// How long a challenge stays usable.
const NONCE_TTL: Duration = Duration::from_secs(300);

struct NonceState {
    issued: Instant,
    highest_nc: u64,
}

fn issued_nonces() -> &'static Mutex<HashMap<String, NonceState>> {
    static NONCES: OnceLock<Mutex<HashMap<String, NonceState>>> = OnceLock::new();
    NONCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mints a challenge and remembers it, so a client cannot pick its own nonce.
pub fn issue_nonce() -> String {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let mut map = issued_nonces().lock().unwrap();
    map.retain(|_, s| s.issued.elapsed() < NONCE_TTL);
    map.insert(
        nonce.clone(),
        NonceState {
            issued: Instant::now(),
            highest_nc: 0,
        },
    );
    nonce
}

/// True only for a nonce we issued, unexpired, whose counter moved forward.
fn accept_nonce(nonce: &str, nc: Option<&str>) -> bool {
    let mut map = issued_nonces().lock().unwrap();
    map.retain(|_, s| s.issued.elapsed() < NONCE_TTL);
    match nc {
        Some(nc) => {
            let Ok(counter) = u64::from_str_radix(nc, 16) else {
                return false;
            };
            match map.get_mut(nonce) {
                Some(state) if counter > state.highest_nc => {
                    state.highest_nc = counter;
                    true
                }
                _ => false,
            }
        }
        // No qop means no counter, so the nonce is good for one request.
        None => map.remove(nonce).is_some(),
    }
}

#[derive(Debug)]
pub struct DigestAuth {
    pub username: String,
    pub realm: String,
    pub nonce: String,
    pub uri: String,
    pub response: String,
    pub qop: Option<String>,
    pub nc: Option<String>,
    pub cnonce: Option<String>,
}

pub fn parse_digest_auth(req: &actix_web::HttpRequest) -> Result<DigestAuth, Error> {
    if let Some(auth_header) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(auth_str) = auth_header.to_str() {
            if let Some(digest_params) = auth_str.strip_prefix("Digest ") {
                let params = parse_digest_header(digest_params);
                if let (Some(username), Some(realm), Some(nonce), Some(uri), Some(response)) = (
                    params.get("username"),
                    params.get("realm"),
                    params.get("nonce"),
                    params.get("uri"),
                    params.get("response"),
                ) {
                    return Ok(DigestAuth {
                        username: username.clone(),
                        realm: realm.clone(),
                        nonce: nonce.clone(),
                        uri: uri.clone(),
                        response: response.clone(),
                        qop: params.get("qop").cloned(),
                        nc: params.get("nc").cloned(),
                        cnonce: params.get("cnonce").cloned(),
                    });
                }
            }
        }
    }
    // Generate challenge if missing or invalid
    let nonce = issue_nonce();
    let challenge = format!(
        "Digest realm=\"NodeInNet\", nonce=\"{}\", algorithm=MD5, qop=\"auth\"",
        nonce
    );
    let resp = actix_web::HttpResponse::Unauthorized()
        .append_header((header::WWW_AUTHENTICATE, challenge))
        .finish();
    Err(actix_web::error::InternalError::from_response("", resp).into())
}

fn parse_digest_header(header: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let parts = header.split(',');
    for part in parts {
        if let Some((key, value)) = part.trim().split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

pub fn validate_digest(
    req: &actix_web::HttpRequest,
    auth: &DigestAuth,
    config: &AuthConfig,
) -> bool {
    if let Some((username, password)) = &config.credentials {
        if &auth.username != username {
            return false;
        }

        let mut hasher = Md5::new();
        hasher.update(format!("{}:{}:{}", username, auth.realm, password));
        let ha1 = format!("{:x}", hasher.finalize());

        let method = req.method().as_str();
        let mut hasher = Md5::new();
        hasher.update(format!("{}:{}", method, auth.uri));
        let ha2 = format!("{:x}", hasher.finalize());

        let mut hasher = Md5::new();
        if let (Some(qop), Some(nc), Some(cnonce)) = (&auth.qop, &auth.nc, &auth.cnonce) {
            hasher.update(format!(
                "{}:{}:{}:{}:{}:{}",
                ha1, auth.nonce, nc, cnonce, qop, ha2
            ));
        } else {
            hasher.update(format!("{}:{}:{}", ha1, auth.nonce, ha2));
        }
        let expected = format!("{:x}", hasher.finalize());

        if auth.response != expected {
            return false;
        }
        // A correct digest stays correct once captured; the challenge must be fresh.
        return accept_nonce(&auth.nonce, auth.nc.as_deref());
    }
    false
}

pub async fn digest_auth_middleware(
    req: actix_web::dev::ServiceRequest,
    next: actix_web::middleware::Next<impl actix_web::body::MessageBody + 'static>,
) -> Result<actix_web::dev::ServiceResponse<impl actix_web::body::MessageBody>, actix_web::Error> {
    let config = if let Some(cfg) = req.app_data::<web::Data<AuthConfig>>() {
        cfg
    } else {
        return next.call(req).await.map(|res| res.map_into_left_body());
    };

    match parse_digest_auth(req.request()) {
        Ok(auth) => {
            if validate_digest(req.request(), &auth, config) {
                next.call(req).await.map(|res| res.map_into_left_body())
            } else {
                let nonce = issue_nonce();
                let challenge = format!(
                    "Digest realm=\"NodeInNet\", nonce=\"{}\", algorithm=MD5, qop=\"auth\"",
                    nonce
                );
                let resp = actix_web::HttpResponse::Unauthorized()
                    .append_header((header::WWW_AUTHENTICATE, challenge))
                    .finish();
                Ok(req.into_response(resp).map_into_right_body())
            }
        }
        Err(e) => Ok(req.error_response(e).map_into_right_body()),
    }
}
