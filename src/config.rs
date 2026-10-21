use anyhow::{Context, Result, bail};
use reqwest::Url;
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    pub default_upstream: String,
    pub routes: HashMap<String, String>,
    pub log_directory: String,
    pub log_retention_days: u16,
    pub max_redirects: u8,
    pub redirect_follow_domains: Option<Vec<String>>,
    // Compatibility with v0.3 configurations; cannot be combined with a follow list.
    pub redirect_passthrough_domains: Vec<String>,
    pub connect_timeout_seconds: u64,
    pub read_timeout_seconds: u64,
    pub max_request_body_bytes: usize,
    pub max_buffered_request_bytes: u32,
    pub allow_private_upstreams: bool,
    pub control_socket: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:80".parse().unwrap(),
            // An unset upstream is invalid; parse() requires an explicit origin.
            default_upstream: String::new(),
            routes: HashMap::new(),
            log_directory: "logs".into(),
            log_retention_days: 30,
            max_redirects: 10,
            redirect_follow_domains: None,
            redirect_passthrough_domains: Vec::new(),
            connect_timeout_seconds: 10,
            read_timeout_seconds: 120,
            max_request_body_bytes: 16 * 1024 * 1024,
            max_buffered_request_bytes: 64 * 1024 * 1024,
            allow_private_upstreams: false,
            control_socket: "/run/emby-proxy/control.sock".into(),
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
    if value.contains('@') || value.is_empty() {
        return None;
    }
    let authority = value.parse::<axum::http::uri::Authority>().ok()?;
    let raw_host = authority.host();
    let suffix = value.strip_prefix(raw_host)?;
    if !suffix.is_empty() {
        suffix.strip_prefix(':')?.parse::<u16>().ok()?;
    }
    let host = raw_host
        .strip_suffix('.')
        .unwrap_or(raw_host)
        .to_ascii_lowercase();
    if host.starts_with('[') {
        host.strip_prefix('[')?
            .strip_suffix(']')?
            .parse::<std::net::Ipv6Addr>()
            .ok()?;
    } else if host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || !label.as_bytes()[0].is_ascii_alphanumeric()
                || !label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return None;
    }
    Some(host)
}

impl Config {
    pub fn read(path: &Path) -> Result<String> {
        anyhow::ensure!(
            std::fs::metadata(path)?.is_file(),
            "configuration must be a regular file"
        );
        let file = std::fs::File::open(path)?;
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "configuration must be a regular file"
        );
        let mut contents = String::new();
        file.take(1024 * 1024 + 1).read_to_string(&mut contents)?;
        anyhow::ensure!(contents.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
        Ok(contents)
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        match path {
            Some(path) => Self::parse(&Self::read(path)?),
            None => bail!("missing configuration file; create it and pass --config FILE"),
        }
    }

    pub fn parse(contents: &str) -> Result<Self> {
        anyhow::ensure!(contents.len() <= 1024 * 1024, "configuration exceeds 1 MiB");
        // TOML errors can include source lines containing credentials or URLs.
        let mut config: Self = toml::from_str(contents).map_err(|_| {
            anyhow::anyhow!("invalid configuration: check TOML syntax, fields and value types")
        })?;
        anyhow::ensure!(
            !config.default_upstream.trim().is_empty(),
            "missing required configuration: default_upstream; set it to an HTTP(S) origin"
        );
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
        anyhow::ensure!(
            config.redirect_follow_domains.is_none()
                || config.redirect_passthrough_domains.is_empty(),
            "redirect_follow_domains cannot be combined with nonempty redirect_passthrough_domains"
        );
        if let Some(domains) = &mut config.redirect_follow_domains {
            normalize_redirect_domains(domains)?;
        }
        normalize_redirect_domains(&mut config.redirect_passthrough_domains)?;
        anyhow::ensure!(
            config.control_socket.is_absolute()
                && config.control_socket.file_name().is_some()
                && config.control_socket.as_os_str().len() < 108
                && !config
                    .control_socket
                    .as_os_str()
                    .as_encoded_bytes()
                    .contains(&0),
            "control_socket must be an absolute Unix socket filename shorter than 108 bytes"
        );
        anyhow::ensure!(
            !config.log_directory.is_empty() && !config.log_directory.contains('\0'),
            "invalid log_directory"
        );
        if !(1..=30).contains(&config.log_retention_days)
            || !(1..=20).contains(&config.max_redirects)
            || !(1..=86400).contains(&config.connect_timeout_seconds)
            || !(1..=86400).contains(&config.read_timeout_seconds)
            || config.max_request_body_bytes == 0
            || config.max_request_body_bytes > 64 * 1024 * 1024
            || config.max_buffered_request_bytes < config.max_request_body_bytes as u32
            || config.max_buffered_request_bytes > 512 * 1024 * 1024
        {
            bail!("invalid retention, redirect, timeout or request body limit");
        }
        Ok(config)
    }

    pub fn passthrough_redirect(&self, target: &Url) -> bool {
        match &self.redirect_follow_domains {
            Some(domains) => !matches_redirect_domain(target, domains),
            None => matches_redirect_domain(target, &self.redirect_passthrough_domains),
        }
    }

    pub fn validate_reload(&self, next: &Self) -> Result<()> {
        for (same, name) in [
            (self.listen == next.listen, "listen"),
            (self.log_directory == next.log_directory, "log_directory"),
            (
                self.log_retention_days == next.log_retention_days,
                "log_retention_days",
            ),
            (
                self.max_buffered_request_bytes == next.max_buffered_request_bytes,
                "max_buffered_request_bytes",
            ),
            (self.control_socket == next.control_socket, "control_socket"),
        ] {
            anyhow::ensure!(
                same,
                "{name} requires a service restart; running configuration unchanged"
            );
        }
        Ok(())
    }
}

fn normalize_redirect_domains(domains: &mut Vec<String>) -> Result<()> {
    for domain in domains.iter_mut() {
        // This is a DNS suffix policy, not a URL, authority or IP allowlist.
        anyhow::ensure!(!domain.contains(':'), "invalid redirect domain");
        let normalized = normalize_host(domain).context("invalid redirect domain")?;
        anyhow::ensure!(
            matches!(url::Host::parse(&normalized), Ok(url::Host::Domain(_))),
            "redirect entries must be DNS domains"
        );
        *domain = normalized;
    }
    domains.sort_unstable();
    domains.dedup();
    Ok(())
}

fn matches_redirect_domain(target: &Url, domains: &[String]) -> bool {
    let Some(host) = target.host_str().and_then(normalize_host) else {
        return false;
    };
    domains.iter().any(|domain| {
        host == *domain
            || host
                .strip_suffix(domain)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_with_upstream(contents: &str) -> Result<Config> {
        Config::parse(&format!(
            "default_upstream='https://origin.example.com'\n{contents}"
        ))
    }

    #[test]
    fn requires_explicit_upstream_and_configuration_file() {
        for contents in [
            "",
            "# configuration pending",
            "default_upstream=''",
            "default_upstream='   '",
            "[routes]\n'example.com'='https://origin.example.com'",
        ] {
            let error = Config::parse(contents).unwrap_err().to_string();
            assert!(
                error.contains("missing required configuration: default_upstream"),
                "{error}"
            );
        }
        assert!(
            Config::load(None)
                .unwrap_err()
                .to_string()
                .contains("missing configuration file")
        );
        assert!(Config::parse(include_str!("../examples/proxy.toml")).is_err());
        assert_eq!(
            parse_with_upstream("").unwrap().default_upstream,
            "https://origin.example.com"
        );
    }

    #[test]
    fn routes_require_unambiguous_hosts_and_origins() {
        assert_eq!(
            normalize_host("EXAMPLE.COM.:80").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_host("[2001:db8::1]:443").as_deref(),
            Some("[2001:db8::1]")
        );
        for host in [
            "",
            ".",
            "user@example.com",
            "example.com:bad",
            "example.com:99999",
            "*.example.com",
            "a..example",
            "-bad.example",
        ] {
            assert!(normalize_host(host).is_none(), "{host}");
        }
        assert!(parse_with_upstream("[routes]\n'EXAMPLE.com'='https://one.example'\n'example.COM:80'='https://two.example'").is_err());
        assert!(Config::parse("default_upstream='https://user:password@example.com'").is_err());
        assert!(parse_with_upstream("[routes]\n'example.com'='https://example.com/path'").is_err());
    }

    #[test]
    fn rejects_unknown_keys_invalid_runtime_paths_and_resource_limits() {
        for text in [
            "typo=true",
            "control_socket='relative.sock'",
            "control_socket='/'",
            "log_directory=''",
            "read_timeout_seconds=18446744073709551615",
            "max_buffered_request_bytes=1",
            "log_retention_days=31",
        ] {
            assert!(parse_with_upstream(text).is_err(), "{text}");
        }
    }

    #[test]
    fn redirect_domains_match_only_complete_dns_labels() {
        let config = parse_with_upstream(
            "redirect_passthrough_domains=['CDN.EXAMPLE.NET.', 'cdn.example.net']",
        )
        .unwrap();
        assert_eq!(config.redirect_passthrough_domains, ["cdn.example.net"]);
        for host in [
            "cdn.example.net",
            "a.cdn.example.net",
            "A.B.CDN.EXAMPLE.NET.",
            "a.cdn.example.net:8443",
        ] {
            let url = Url::parse(&format!("https://{host}/video?sig=a%2Fb")).unwrap();
            assert!(config.passthrough_redirect(&url), "{host}");
            assert!(!parse_with_upstream("").unwrap().passthrough_redirect(&url));
        }
        for host in [
            "notcdn.example.net",
            "cdn.example.net.evil.example",
            "cdn.example.net.evil",
            "other.example",
        ] {
            assert!(
                !config.passthrough_redirect(
                    &Url::parse(&format!("https://{host}/cdn.example.net")).unwrap()
                ),
                "{host}"
            );
        }
        for domain in [
            "",
            "https://cdn.example.net",
            "*.cdn.example.net",
            ".cdn.example.net",
            "cdn.example.net:443",
            "cdn.example.net/path",
            "cdn.example.net?x=1",
            "user@cdn.example.net",
            "a..cdn.example.net",
            "127.0.0.1",
            "2130706433",
            "0x7f000001",
            "[::1]",
        ] {
            assert!(
                parse_with_upstream(&format!("redirect_passthrough_domains=['{domain}']")).is_err(),
                "{domain}"
            );
        }
        parse_with_upstream("")
            .unwrap()
            .validate_reload(&config)
            .unwrap();
    }

    #[test]
    fn follow_list_proxies_only_listed_domains_and_preserves_legacy_configs() {
        let config =
            parse_with_upstream("redirect_follow_domains=['EXAMPLE.COM.', 'example.com']").unwrap();
        assert_eq!(
            config.redirect_follow_domains.as_deref().unwrap(),
            ["example.com"]
        );
        for host in [
            "example.com",
            "media.example.com",
            "A.B.EXAMPLE.COM.",
            "example.com:8443",
        ] {
            assert!(
                !config
                    .passthrough_redirect(&Url::parse(&format!("https://{host}/media")).unwrap()),
                "{host}"
            );
        }
        for host in [
            "notexample.com",
            "example.com.evil.example",
            "edge.cdn.example.net",
            "storage.alias.example.net",
            "127.0.0.1",
        ] {
            assert!(
                config.passthrough_redirect(
                    &Url::parse(&format!("https://{host}/example.com")).unwrap()
                ),
                "{host}"
            );
        }
        for text in [
            "redirect_follow_domains=['https://example.com']",
            "redirect_follow_domains=['*.example.com']",
            "redirect_follow_domains=['example.com:443']",
            "redirect_follow_domains=['127.0.0.1']",
            "redirect_follow_domains=['example.com']\nredirect_passthrough_domains=['cdn.example.net']",
        ] {
            assert!(parse_with_upstream(text).is_err(), "{text}");
        }
        let target = Url::parse("https://example.com/media").unwrap();
        assert!(
            parse_with_upstream("redirect_follow_domains=[]")
                .unwrap()
                .passthrough_redirect(&target)
        );
        assert!(
            !parse_with_upstream("")
                .unwrap()
                .passthrough_redirect(&target)
        );
        assert!(
            !parse_with_upstream("redirect_passthrough_domains=['cdn.example.net']")
                .unwrap()
                .passthrough_redirect(&target)
        );
        parse_with_upstream("")
            .unwrap()
            .validate_reload(&config)
            .unwrap();
    }
}
