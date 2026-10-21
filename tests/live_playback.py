#!/usr/bin/env python3
"""Compare a real origin redirect and proxied byte ranges, without printing secrets.

Read fixture JSON from stdin: token, device_id, item_id, media_source_id.
No login/session mutations, filesystem scans, or full-media downloads.
"""
import argparse
import hashlib
import http.client
import json
import sys
import urllib.parse

REDIRECTS = (301, 302, 303, 307, 308)


def matches_domain(url, domains):
    host = (urllib.parse.urlsplit(url).hostname or "").lower().removesuffix(".")
    return any(host == domain or host.endswith("." + domain) for domain in domains)


def follow(url, headers, should_passthrough, method="GET"):
    passthrough_status = None
    for _ in range(11):
        status, response_headers, body = fetch(url, headers, method)
        if status not in REDIRECTS:
            return status, response_headers, body, passthrough_status
        url = urllib.parse.urljoin(url, response_headers["location"])
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme not in ("http", "https") or parsed.username is not None or parsed.password is not None:
            raise RuntimeError("invalid playback redirect")
        if passthrough_status is None and should_passthrough(url):
            passthrough_status = status
    raise RuntimeError("playback redirect limit exceeded")


def fetch(url, headers, method="GET", limit=65536):
    target = urllib.parse.urlsplit(url)
    if target.scheme not in ("http", "https") or not target.hostname or target.username is not None or target.password is not None:
        raise RuntimeError("invalid playback URL")
    cls = http.client.HTTPSConnection if target.scheme == "https" else http.client.HTTPConnection
    conn = cls(target.hostname, target.port, timeout=90)
    conn.request(method, urllib.parse.urlunsplit(("", "", target.path or "/", target.query, "")), headers=headers)
    response = conn.getresponse()
    body = response.read(limit + 1) if method != "HEAD" else b""
    result = response.status, {k.lower(): v for k,v in response.getheaders()}, body
    conn.close()
    if len(body) > limit:
        raise RuntimeError("upstream ignored bounded byte range")
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--proxy", required=True)
    parser.add_argument("--origin", required=True)
    parser.add_argument("--passthrough-domain", action="append", default=[])
    parser.add_argument("--follow-domains", nargs="*", default=None)
    args = parser.parse_args()
    args.passthrough_domain = [domain.lower().removesuffix(".") for domain in args.passthrough_domain]
    if args.follow_domains is not None:
        if args.passthrough_domain:
            parser.error("follow domains and passthrough domains cannot be combined")
        args.follow_domains = [domain.lower().removesuffix(".") for domain in args.follow_domains]
        should_passthrough = lambda url: not matches_domain(url, args.follow_domains)
    else:
        should_passthrough = lambda url: matches_domain(url, args.passthrough_domain)
    fixture = json.load(sys.stdin)
    query = urllib.parse.urlencode({"Static":"true", "DeviceId":fixture["device_id"], "MediaSourceId":fixture["media_source_id"]})
    path = "/Videos/" + urllib.parse.quote(fixture["item_id"], safe="") + "/stream?" + query
    authenticated = {"X-Emby-Token":fixture["token"], "User-Agent":"emby-proxy-verification/1.0"}
    for label, byte_range in [("start", "bytes=0-65535"), ("seek", "bytes=1048576-1114111"), ("tail", "bytes=-65536")]:
        headers = {**authenticated, "Range":byte_range}
        status, response_headers, _ = fetch(args.origin + path, headers)
        if status not in REDIRECTS:
            raise RuntimeError(f"origin did not redirect for {label}: HTTP {status}")
        target = urllib.parse.urljoin(args.origin + path, response_headers["location"])
        safe_headers = {"Range":byte_range,"User-Agent":authenticated["User-Agent"]}
        direct_status, direct_headers, direct_body, expected_redirect = follow(target, safe_headers, should_passthrough)
        if should_passthrough(target):
            expected_redirect = status
        actual_status, actual_headers, actual_body = fetch(args.proxy.rstrip("/") + path, headers)
        client_redirect = actual_status in REDIRECTS
        if expected_redirect is not None:
            assert actual_status == expected_redirect
            actual_target = urllib.parse.urljoin(args.proxy.rstrip("/") + path, actual_headers["location"])
            assert should_passthrough(actual_target)
            actual_status, actual_headers, actual_body, _ = follow(actual_target, safe_headers, should_passthrough)
        else:
            assert "location" not in actual_headers
        if direct_status != 206 or actual_status != 206:
            raise RuntimeError(f"range {label}: direct={direct_status}, proxy={actual_status}")
        assert actual_headers["content-range"] == direct_headers["content-range"]
        assert len(actual_body) == 65536 and actual_body == direct_body
        print(json.dumps({"check":label,"status":actual_status,"bytes":len(actual_body),"sha256":hashlib.sha256(actual_body).hexdigest(),"client_redirect":client_redirect}), flush=True)
    # HEAD is passed through unchanged; Emby may answer from its own backend
    # instead of the GET-only media redirect handler.
    status, headers, body = fetch(args.proxy.rstrip("/") + path, authenticated, "HEAD")
    client_status = status
    if status in REDIRECTS:
        target = urllib.parse.urljoin(args.proxy.rstrip("/") + path, headers["location"])
        assert should_passthrough(target)
        status, _, body, _ = follow(target, {"User-Agent":authenticated["User-Agent"]}, should_passthrough, "HEAD")
    assert status in (200,206) and not body, f"HEAD failed: {status}"
    print(json.dumps({"check":"HEAD","status":client_status,"bytes":0}), flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Exception strings from network libraries can contain signed URLs.
        print("Playback verification failed: " + type(error).__name__, file=sys.stderr)
        sys.exit(1)
