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


def fetch(url, headers, method="GET", limit=65536):
    target = urllib.parse.urlsplit(url)
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
    parser.add_argument("--origin", default="https://iris.pvq.cc")
    args = parser.parse_args()
    fixture = json.load(sys.stdin)
    query = urllib.parse.urlencode({"Static":"true", "DeviceId":fixture["device_id"], "MediaSourceId":fixture["media_source_id"]})
    path = "/Videos/" + urllib.parse.quote(fixture["item_id"], safe="") + "/stream?" + query
    authenticated = {"X-Emby-Token":fixture["token"], "User-Agent":"emby-proxy-verification/1.0"}
    for label, byte_range in [("start", "bytes=0-65535"), ("seek", "bytes=1048576-1114111"), ("tail", "bytes=-65536")]:
        headers = {**authenticated, "Range":byte_range}
        status, response_headers, _ = fetch(args.origin + path, headers)
        if status not in (301,302,303,307,308):
            raise RuntimeError(f"origin did not redirect for {label}: HTTP {status}")
        target = urllib.parse.urljoin(args.origin + path, response_headers["location"])
        for _ in range(10):
            direct_status, direct_headers, direct_body = fetch(target, {"Range":byte_range,"User-Agent":authenticated["User-Agent"]})
            if direct_status not in (301,302,303,307,308): break
            target = urllib.parse.urljoin(target, direct_headers["location"])
        actual_status, actual_headers, actual_body = fetch(args.proxy.rstrip("/") + path, headers)
        if direct_status != 206 or actual_status != 206:
            raise RuntimeError(f"range {label}: direct={direct_status}, proxy={actual_status}")
        assert "location" not in actual_headers
        assert actual_headers["content-range"] == direct_headers["content-range"]
        assert len(actual_body) == 65536 and actual_body == direct_body
        print(json.dumps({"check":label,"status":actual_status,"bytes":len(actual_body),"sha256":hashlib.sha256(actual_body).hexdigest(),"client_redirect":False}), flush=True)
    # HEAD is passed through unchanged; Emby may answer from its own backend
    # instead of the GET-only media redirect handler.
    status, _, body = fetch(args.proxy.rstrip("/") + path, authenticated, "HEAD")
    assert status in (200,206) and not body, f"HEAD failed: {status}"
    print(json.dumps({"check":"HEAD","status":status,"bytes":0}), flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Exception strings from network libraries can contain signed URLs.
        print("Playback verification failed: " + type(error).__name__, file=sys.stderr)
        sys.exit(1)
