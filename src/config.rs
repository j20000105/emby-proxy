use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::Deserialize;
use std::{collections::HashMap, net::SocketAddr, path::Path};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub default_upstream: String,
    pub routes: HashMap<String, String>,
    pub log_directory: String,
    pub log_retention_days: u16,
    pub max_redirects: u8,
    pub connect_timeout_seconds: u64,
    pub read_timeout_seconds: u64,
    pub max_request_body_bytes: usize,
    pub allow_private_upstreams: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:80".parse().unwrap(),
            default_upstream: "https://iris.pvq.cc".into(),
            routes: HashMap::new(),
            log_directory: "logs".into(),
            log_retention_days: 30,
            max_redirects: 10,
            connect_timeout_seconds: 10,
            read_timeout_seconds: 120,
            max_request_body_bytes: 16 * 1024 * 1024,
            allow_private_upstreams: false,
        }
    }
}

pub fn origin(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("invalid upstream URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        bail!("upstream must be an HTTP(S) origin without credentials, path, query or fragment");
    }
    Ok(url)
}

pub fn normalize_host(value: &str) -> Option<String> {
    let authority = value.parse::<axum::http::uri::Authority>().ok()?;
    Some(authority.host().trim_end_matches('.').to_ascii_lowercase())
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let mut config: Self = match path {
            Some(path) => toml::from_str(&std::fs::read_to_string(path)?)?,
            None => Self::default(),
        };
        origin(&config.default_upstream)?;
        let mut routes = HashMap::new();
        for (host, upstream) in &config.routes {
            let host = normalize_host(host).context("invalid route host")?;
            origin(upstream)?;
            if routes.insert(host, upstream.clone()).is_some() {
                bail!("duplicate normalized route host");
            }
        }
        config.routes = routes;
        if !(1..=30).contains(&config.log_retention_days)
            || !(1..=20).contains(&config.max_redirects)
            || config.connect_timeout_seconds == 0
            || config.read_timeout_seconds == 0
            || config.max_request_body_bytes == 0
            || config.max_request_body_bytes > 64 * 1024 * 1024
        {
            bail!("invalid retention, redirect, timeout or request body limit");
        }
        Ok(config)
    }
}
