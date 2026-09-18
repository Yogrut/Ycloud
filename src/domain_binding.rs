//! Persisted HTTPS domain binding, applied immediately after a successful save.
use std::net::IpAddr;

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use crate::{
    config::Config,
    error::{AppError, AppResult},
    state::AppState,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DomainBinding {
    pub public_url: String,
    pub trusted_proxy_ips: Vec<IpAddr>,
}

impl DomainBinding {
    pub fn validate(&self) -> AppResult<()> {
        let invalid = || {
            AppError::BadRequest(
                "请输入 HTTPS 域名地址（可带端口），不包含路径、账号或查询参数".into(),
            )
        };
        if self.public_url.len() > 300 {
            return Err(invalid());
        }
        let uri: axum::http::Uri = self.public_url.parse().map_err(|_| invalid())?;
        let authority = uri.authority().ok_or_else(invalid)?;
        let host = authority.host();
        if uri.scheme_str() != Some("https")
            || !matches!(uri.path(), "" | "/")
            || uri.query().is_some()
            || authority.as_str().contains('@')
            || host.parse::<IpAddr>().is_ok()
            || host.len() > 253
            || !host.contains('.')
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || (authority.as_str().contains(':') && authority.port_u16().is_none_or(|p| p == 0))
        {
            return Err(invalid());
        }
        if self.trusted_proxy_ips.is_empty()
            || self.trusted_proxy_ips.len() > 16
            || self
                .trusted_proxy_ips
                .iter()
                .any(|ip| ip.is_unspecified() || ip.is_multicast())
        {
            return Err(AppError::BadRequest(
                "请填写 1–16 个可信反向代理的准确 IP 地址，不能使用通配地址".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn normalize(mut self) -> AppResult<Self> {
        self.public_url = self
            .public_url
            .trim()
            .trim_end_matches('/')
            .to_ascii_lowercase();
        self.validate()?;
        // Browsers omit the default HTTPS port from Host and Origin.
        if self.public_url.ends_with(":443") {
            self.public_url.truncate(self.public_url.len() - 4);
        }
        self.trusted_proxy_ips.sort();
        self.trusted_proxy_ips.dedup();
        Ok(self)
    }

    pub fn apply(&self, base: &Config) -> Config {
        let mut config = base.clone();
        config.public_base_url = Some(self.public_url.clone());
        config.public_host = Some(
            self.public_url
                .trim_start_matches("https://")
                .trim_end_matches('/')
                .into(),
        );
        config.trusted_proxy_ips = self.trusted_proxy_ips.iter().copied().collect();
        config.allow_lan_http = false;
        config.secure_cookies = true;
        config
    }
}

#[derive(Serialize)]
pub struct BindingView {
    pub binding: Option<DomainBinding>,
    pub source: &'static str,
}

pub async fn view(state: &AppState) -> BindingView {
    let persisted = state.config_file.read().await.domain_binding.clone();
    let source = if persisted.is_some() {
        "settings"
    } else if state.config.is_public_mode() {
        "environment"
    } else {
        "none"
    };
    BindingView {
        binding: persisted.or_else(|| {
            state
                .config
                .public_base_url
                .as_ref()
                .map(|url| DomainBinding {
                    public_url: url.clone(),
                    trusted_proxy_ips: state.config.trusted_proxy_ips.iter().copied().collect(),
                })
        }),
        source,
    }
}

/// Read the committed policy from memory; no disk IO and no old-address fallback.
pub async fn policy(state: &AppState) -> Config {
    let binding = state.config_file.read().await.domain_binding.clone();
    binding.map_or_else(
        || state.config.clone(),
        |binding| binding.apply(&state.config),
    )
}

pub async fn get_binding(State(state): State<AppState>) -> Json<BindingView> {
    Json(view(&state).await)
}

pub async fn save_binding(
    State(state): State<AppState>,
    Json(binding): Json<DomainBinding>,
) -> AppResult<Json<BindingView>> {
    let binding = binding.normalize()?;
    state
        .update_config(move |config| {
            config.domain_binding = Some(binding);
            Ok(())
        })
        .await?;
    Ok(Json(view(&state).await))
}

pub async fn remove_binding(State(state): State<AppState>) -> AppResult<Json<BindingView>> {
    state
        .update_config(|config| {
            config.domain_binding = None;
            Ok(())
        })
        .await?;
    Ok(Json(view(&state).await))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{self, ConfigFile};
    use axum::{
        body::Body,
        extract::ConnectInfo,
        http::{header, Request, StatusCode},
    };
    use std::{path::PathBuf, sync::Arc};
    use tokio::sync::RwLock;
    use tower::ServiceExt;
    use uuid::Uuid;

    fn binding() -> DomainBinding {
        DomainBinding {
            public_url: "https://cloud.example.com".into(),
            trusted_proxy_ips: vec!["127.0.0.1".parse().unwrap()],
        }
    }

    async fn fixture() -> (AppState, PathBuf) {
        let root = std::env::temp_dir().join(format!("ycloud-domain-{}", Uuid::new_v4()));
        let runtime = crate::test_support::runtime_config(&root);
        let persisted = ConfigFile {
            admin_password_hash: config::hash_password("domain-test-password"),
            global_web_password_hash: None,
            ..ConfigFile::default()
        };
        config::save_config(&runtime.config_path, &persisted)
            .await
            .unwrap();
        (
            AppState::new(runtime, Arc::new(RwLock::new(persisted)))
                .await
                .unwrap(),
            root,
        )
    }

    fn request(
        method: &str,
        path: &str,
        public: bool,
        token: Option<&str>,
        body: serde_json::Value,
    ) -> Request<Body> {
        let host = if public {
            "cloud.example.com"
        } else {
            "127.0.0.1:18473"
        };
        let origin = if public {
            "https://cloud.example.com"
        } else {
            "http://127.0.0.1:18473"
        };
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, host)
            .header(header::ORIGIN, origin)
            .header(header::CONTENT_TYPE, "application/json")
            .extension(ConnectInfo(
                "127.0.0.1:50000".parse::<std::net::SocketAddr>().unwrap(),
            ));
        if public {
            builder = builder
                .header("x-forwarded-for", "192.168.1.10")
                .header("x-forwarded-proto", "https");
        }
        if let Some(token) = token {
            builder = builder.header(header::COOKIE, format!("session={token}"));
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    #[test]
    fn normalizes_domain_and_supports_custom_https_port() {
        let mut value = binding();
        value.public_url = " HTTPS://CLOUD.EXAMPLE.COM:443/ ".into();
        value.trusted_proxy_ips.push(value.trusted_proxy_ips[0]);
        assert_eq!(value.normalize().unwrap(), binding());
        value = binding();
        value.public_url = "https://cloud.example.com:8443".into();
        assert_eq!(
            value.normalize().unwrap().public_url,
            "https://cloud.example.com:8443"
        );
    }

    #[test]
    fn requires_https_domain_and_explicit_proxy_configuration() {
        let mut value = binding();
        value.public_url = "http://cloud.example.com".into();
        assert!(value.normalize().is_err());
        value = binding();
        value.trusted_proxy_ips.clear();
        assert!(value.normalize().is_err());
    }

    #[tokio::test]
    async fn failed_save_keeps_the_previous_policy() {
        let (mut state, root) = fixture().await;
        let original_path = state.config.config_path.clone();
        let blocked_parent = root.join("not-a-directory");
        tokio::fs::write(&blocked_parent, b"fixture").await.unwrap();
        state.config.config_path = blocked_parent.join("config.json");
        assert!(save_binding(State(state.clone()), Json(binding()))
            .await
            .is_err());
        assert!(state.config_file.read().await.domain_binding.is_none());
        assert!(!policy(&state).await.is_public_mode());
        assert!(config::load_config(&original_path)
            .await
            .unwrap()
            .domain_binding
            .is_none());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn saves_immediately_persists_across_restart_and_restores_deployment_on_removal() {
        let (state, root) = fixture().await;
        let router = crate::app::build_router(state.clone());
        let token = state.sessions.create().await;
        let saved = router
            .clone()
            .oneshot(request(
                "PUT",
                "/api/admin/domain-binding",
                false,
                Some(&token),
                serde_json::to_value(binding()).unwrap(),
            ))
            .await
            .unwrap();
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(
            state.config_file.read().await.domain_binding,
            Some(binding())
        );
        assert_eq!(
            policy(&state).await.public_base_url.as_deref(),
            Some("https://cloud.example.com")
        );
        let login = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/login",
                true,
                None,
                serde_json::json!({"username":"admin","password":"domain-test-password"}),
            ))
            .await
            .unwrap();
        assert_eq!(login.status(), StatusCode::OK);
        let cookie = login.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(cookie.contains("; Secure"));
        assert!(cookie.contains("HttpOnly"));
        let new_token = cookie
            .split(';')
            .next()
            .unwrap()
            .trim_start_matches("session=");
        let persisted = config::load_config(&state.config.config_path)
            .await
            .unwrap();
        assert_eq!(persisted.domain_binding, Some(binding()));
        let restarted = AppState::new(state.config.clone(), Arc::new(RwLock::new(persisted)))
            .await
            .unwrap();
        assert_eq!(
            policy(&restarted).await.public_base_url.as_deref(),
            Some("https://cloud.example.com")
        );
        let old_page = router
            .clone()
            .oneshot(request(
                "GET",
                "/api/health",
                false,
                None,
                serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(old_page.status(), StatusCode::FORBIDDEN);
        let new_page = router
            .clone()
            .oneshot(request(
                "GET",
                "/api/admin/info",
                true,
                Some(new_token),
                serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(new_page.status(), StatusCode::OK);
        let removed = router
            .clone()
            .oneshot(request(
                "DELETE",
                "/api/admin/domain-binding",
                true,
                Some(new_token),
                serde_json::Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(removed.status(), StatusCode::OK);
        assert!(config::load_config(&state.config.config_path)
            .await
            .unwrap()
            .domain_binding
            .is_none());
        assert!(!policy(&state).await.is_public_mode());
        tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
