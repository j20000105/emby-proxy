use reqwest::{
    Url,
    dns::{Addrs, Name, Resolve, Resolving},
};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

// Check the addresses actually returned to the connector, not a separate DNS
// lookup. A second lookup would make redirect checks vulnerable to rebinding.
pub struct PublicResolver;

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            if addresses.is_empty() || addresses.iter().any(|a| !public_ip(a.ip())) {
                return Err(io::Error::other("upstream address is not public").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0
        || a >= 240
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 88 && c == 99)
        || (a == 198 && (b == 18 || b == 19)))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // Only global unicast; exclude special-use, documentation, 6to4/Teredo
    // and IPv4-mapped addresses (the latter must use the IPv4 policy).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return public_v4(v4);
    }
    (s[0] & 0xe000) == 0x2000
        && s[0] != 0x2002
        && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
        && !(s[0] == 0x3fff && s[1] < 0x1000)
}

pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => public_v6(v6),
    }
}

pub fn allowed_url(url: &Url, allow_private: bool) -> bool {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host().is_none()
    {
        return false;
    }
    match url.host() {
        Some(url::Host::Ipv4(ip)) => allow_private || public_v4(ip),
        Some(url::Host::Ipv6(ip)) => allow_private || public_v6(ip),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disallows_special_use_and_mapped_internal_addresses() {
        for ip in [
            "0.1.2.3",
            "10.1.2.3",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "198.18.0.1",
            "224.0.0.1",
            "240.1.2.3",
            "::1",
            "::ffff:127.0.0.1",
            "fe80::1",
            "fc00::1",
            "2002:7f00:1::",
            "2001:db8::1",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(public_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn resolver_rejects_localhost() {
        assert!(
            PublicResolver
                .resolve("localhost".parse().unwrap())
                .await
                .is_err()
        );
    }

    #[test]
    fn url_policy_blocks_literal_and_alternate_loopback_notation() {
        for text in [
            "http://127.0.0.1",
            "http://2130706433",
            "http://0x7f000001",
            "http://[::ffff:127.0.0.1]",
            "file:///etc/passwd",
            "https://user:secret@example.com",
        ] {
            assert!(!allowed_url(&Url::parse(text).unwrap(), false), "{text}");
        }
    }
}
