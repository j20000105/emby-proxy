#!/usr/bin/env python3
"""End-to-end proxy regression tests using isolated loopback upstreams."""
import argparse
import base64
import datetime
import gzip
import hashlib
import http.client
import http.server
import json
import pathlib
import select
import socket
import subprocess
import tempfile
import threading
import time
import urllib.parse

DATA = bytes(range(256)) * 8192
RECEIVED = []
SECRET = "test-secret-never-log-this"
SIGNED_LOCATION = "https://A.B.CDN.EXAMPLE.NET.:8443/media?sig=signed%2Bvalue&x=a%2fb&x=1+2&empty=#part"


class Backend(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def reply(self, status, body=b"", headers=None):
        self.send_response(status)
        for key, value in (headers or {}).items():
            self.send_header(key, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET()

    def do_POST(self):
        self.do_GET()

    def do_GET(self):
        body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
        path = urllib.parse.urlsplit(self.path).path
        RECEIVED.append((self.server.server_port, self.path, dict(self.headers), body))
        if path == "/passthrough":
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            return self.reply(int(query.get("status", ["302"])[0]), b"redirect body", {
                "Location": query.get("target", [SIGNED_LOCATION])[0],
                "Content-Type": "text/plain", "Cache-Control": "private, no-store",
                "Set-Cookie": "fixture-cookie=present", "Connection": "X-Remove",
                "X-Remove": "secret", "Content-Location": "/do-not-expose",
            })
        if path == "/passthrough-cross":
            return self.reply(302, headers={"Location": f"http://127.0.0.1:{MEDIA}/passthrough"})
        if path == "/passthrough-local":
            return self.reply(302, headers={"Location": f"http://localhost:{self.server.server_port}/echo?sig=a%2Fb+z"})
        if path.startswith("/passthrough-limit/"):
            remaining = int(path.rsplit("/", 1)[1])
            return self.reply(302, headers={"Location": f"/passthrough-limit/{remaining - 1}" if remaining else SIGNED_LOCATION})
        if path == "/redirect":
            return self.reply(302, headers={"Location": f"http://127.0.0.1:{MEDIA}/media?sig=signed%2Bvalue&x=a%2Fb"})
        if path == "/Videos/fixture/stream":
            return self.reply(302, headers={"Location": f"http://localhost:{MEDIA}/media?sig=signed%2Bvalue&x=a%2Fb"})
        if path == "/chain":
            return self.reply(307, headers={"Location": "/redirect"})
        if path == "/loop":
            return self.reply(302, headers={"Location": "/loop"})
        if path.startswith("/endless/"):
            return self.reply(302, headers={"Location": "/endless/" + str(int(path.rsplit("/",1)[1])+1)})
        if path == "/invalid":
            return self.reply(302, headers={"Location": "file:///etc/passwd"})
        if path == "/post-redirect":
            return self.reply(302, headers={"Location": "/echo"})
        if path == "/post-preserve":
            return self.reply(307, headers={"Location": "/echo"})
        if path == "/post-cross":
            return self.reply(307, headers={"Location": f"http://127.0.0.1:{MEDIA}/echo"})
        if path == "/echo":
            return self.reply(200, json.dumps({"method": self.command, "body": body.decode(), "port": self.server.server_port, "path": self.path, "headers": dict(self.headers)}).encode())
        if path == "/gzip":
            return self.reply(200, gzip.compress(b"compressed bytes"), {"Content-Encoding": "gzip"})
        if path == "/hop":
            return self.reply(200, b"ok", {"Connection": "X-Remove", "X-Remove": "secret", "X-Keep": "yes"})
        if path in ("/slow", "/passthrough-slow"):
            self.send_response(302 if path == "/passthrough-slow" else 200)
            if path == "/passthrough-slow":
                self.send_header("Location", SIGNED_LOCATION)
            self.send_header("Content-Length", "65536")
            self.end_headers()
            try:
                self.wfile.write(b"a" * 4096)
                self.wfile.flush()
                time.sleep(1)
                self.wfile.write(b"b" * (65536 - 4096))
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        if path == "/ws":
            accept = base64.b64encode(hashlib.sha1((self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
            self.send_response(101)
            self.send_header("Connection", "Upgrade")
            self.send_header("Upgrade", "websocket")
            self.send_header("Sec-WebSocket-Accept", accept)
            self.end_headers()
            frame = self.rfile.read(2)
            mask = self.rfile.read(4)
            raw = self.rfile.read(frame[1] & 127)
            payload = bytes(x ^ mask[i % 4] for i, x in enumerate(raw))
            self.wfile.write(bytes([0x81, len(payload)]) + payload)
            self.wfile.flush()
            self.close_connection = True
            return
        if path == "/media":
            first, last = 0, len(DATA) - 1
            range_header = self.headers.get("Range")
            if range_header:
                start, end = range_header.removeprefix("bytes=").split("-")
                first = int(start) if start else len(DATA) - int(end)
                last = min(int(end), len(DATA)-1) if start and end else len(DATA)-1
                if first >= len(DATA):
                    return self.reply(416, headers={"Content-Range": f"bytes */{len(DATA)}"})
            headers = {"Content-Type": "video/mp4", "Accept-Ranges": "bytes", "ETag": '"fixture"'}
            if range_header:
                headers["Content-Range"] = f"bytes {first}-{last}/{len(DATA)}"
            return self.reply(206 if range_header else 200, DATA[first:last+1], headers)
        self.reply(404)


def start_backend():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Backend)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def reserve_upload(port, probe):
    # Either concurrent POST can legitimately win the reservation. Retry if
    # the unfinished upload was rejected before it established the reservation.
    for _ in range(10):
        upload = socket.create_connection(("127.0.0.1", port), timeout=5)
        upload.sendall(b"POST /echo HTTP/1.1\r\nHost: test\r\nContent-Length: 1024\r\n\r\nx")
        for _ in range(20):
            if select.select([upload], [], [], .025)[0]:
                break
            if probe() == 503:
                return upload
        upload.close()
    raise AssertionError("concurrent request-body budget not enforced")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("binary", type=pathlib.Path)
    args = parser.parse_args()
    origin, media = start_backend(), start_backend()
    global MEDIA
    MEDIA = media.server_port
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="emby-proxy-test-") as directory:
        directory = pathlib.Path(directory)
        logs = directory / "logs"
        logs.mkdir()
        old = logs / ("emby-proxy.log." + (datetime.datetime.now(datetime.timezone.utc).date() - datetime.timedelta(days=60)).isoformat())
        old.write_text("old")
        unrelated = logs / "keep-me"
        unrelated.write_text("preserve")
        config = directory / "proxy.toml"
        config.write_text(f'''listen = "127.0.0.1:{port}"
default_upstream = "http://127.0.0.1:{origin.server_port}"
log_directory = "{logs}"
control_socket = "{directory}/control.sock"
max_request_body_bytes = 1024
max_buffered_request_bytes = 1024
read_timeout_seconds = 2
redirect_passthrough_domains = ["cdn.example.net", "localhost"]
allow_private_upstreams = true
[routes]
"different.example" = "http://127.0.0.1:{MEDIA}"
"relative.example" = "http://localhost:{origin.server_port}"
''')
        process = subprocess.Popen([str(args.binary.resolve()), "--config", str(config)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        def request(path, method="GET", headers=None, body=None):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=8)
            connection.request(method, path, body, headers or {})
            response = connection.getresponse()
            result = response.status, dict((k.lower(), v) for k,v in response.getheaders()), response.read()
            connection.close()
            return result
        try:
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError(process.stderr.read().decode())
                try:
                    request("/echo")
                    break
                except ConnectionRefusedError:
                    time.sleep(.05)
            else:
                raise AssertionError("proxy never started")
            assert not old.exists() and unrelated.exists(), "retention removed wrong files"
            for host in ["proxy.example.com", "unknown.example", "127.0.0.1"]:
                assert json.loads(request("/echo", headers={"Host": host})[2])["port"] == origin.server_port
            assert json.loads(request("/echo", headers={"Host": "DIFFERENT.EXAMPLE:80"})[2])["port"] == MEDIA
            status, headers, body = request("/chain?api_key=" + SECRET, headers={"Range": "bytes=123-456", "Authorization": SECRET, "X-Emby-Token": SECRET, "Cookie": SECRET, "X-Custom-Secret": SECRET})
            assert status == 206 and body == DATA[123:457]
            assert headers["content-range"] == f"bytes 123-456/{len(DATA)}" and "location" not in headers
            final = RECEIVED[-1]
            assert final[1] == "/media?sig=signed%2Bvalue&x=a%2Fb"
            assert SECRET not in str(final), "cross-origin credential leak"
            assert request("/redirect", headers={"Range": "bytes=-64"})[2] == DATA[-64:]
            status, headers, body = request("/redirect", "HEAD", {"Range": "bytes=0-31"})
            assert status == 206 and headers["content-length"] == "32" and body == b""
            assert request("/redirect", headers={"Range": "bytes=99999999-"})[0] == 416
            for code in (301, 302, 303, 307, 308):
                for method in ("GET", "HEAD", "POST"):
                    before = len(RECEIVED)
                    status, headers, body = request(f"/passthrough?status={code}", method, body=b"payload" if method == "POST" else None)
                    assert status == code and headers["location"] == SIGNED_LOCATION
                    assert body == (b"" if method == "HEAD" else b"redirect body")
                    assert headers["content-length"] == "13" and headers["cache-control"] == "private, no-store"
                    assert "set-cookie" in headers and "x-remove" not in headers and "content-location" not in headers
                    assert len(RECEIVED) == before + 1, "passthrough target was fetched"
            for target in ("https://cdn.example.net/a?x=a%2Fb+z", "//cdn.example.net/media?sig=a%2Fb+z"):
                status, headers, _ = request("/passthrough?" + urllib.parse.urlencode({"target": target}))
                assert status == 302 and headers["location"] == ("http:" + target if target.startswith("//") else target)
            for target in ("https://user:password@cdn.example.net/a", "ftp://cdn.example.net/a", "file:///etc/passwd"):
                assert request("/passthrough?" + urllib.parse.urlencode({"target": target}))[0] == 502
            status, headers, body = request("/passthrough-cross", headers={"Authorization": SECRET, "Cookie": SECRET, "Range": "bytes=0-31"})
            assert status == 302 and headers["location"] == SIGNED_LOCATION and body == b"redirect body"
            assert "set-cookie" not in headers and SECRET not in str(RECEIVED[-1])
            assert request("/passthrough-limit/10")[0] == 302, "no extra hop is needed to pass a redirect through"
            assert request("/passthrough-limit/11")[0] == 502
            assert request("/passthrough", headers={"Connection": "Upgrade", "Upgrade": "websocket"})[0] == 502
            for target, resolved in [
                ("/media?sig=a%2Fb+z", f"http://localhost:{origin.server_port}/media?sig=a%2Fb+z"),
                ("?sig=a%2Fb+z", f"http://localhost:{origin.server_port}/passthrough?sig=a%2Fb+z"),
                ("http:media?sig=a%2Fb+z", f"http://localhost:{origin.server_port}/media?sig=a%2Fb+z"),
            ]:
                status, headers, _ = request("/passthrough?" + urllib.parse.urlencode({"target": target}), headers={"Host": "relative.example"})
                assert status == 302 and headers["location"] == resolved
            target = f"http://127.0.0.1:{MEDIA}/media?name=cdn.example.net"
            assert request("/passthrough?" + urllib.parse.urlencode({"target": target}), headers={"Range": "bytes=0-31"})[0] == 206
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            started = time.monotonic()
            conn.request("GET", "/passthrough-slow")
            response = conn.getresponse()
            assert response.status == 302 and response.getheader("Location") == SIGNED_LOCATION
            assert response.read(4096) == b"a" * 4096 and time.monotonic() - started < .8, "redirect body was buffered"
            conn.close()
            for path in ["/loop", "/invalid", "/post-cross"]:
                assert request(path, "POST", body=b"payload")[0] == 502
            before = len(RECEIVED)
            assert request("/endless/0")[0] == 502 and len(RECEIVED) - before == 11
            assert json.loads(request("/post-redirect", "POST", body=b"payload")[2])["method"] == "GET"
            echoed = json.loads(request("/post-preserve", "POST", body=b"payload")[2])
            assert echoed["method"] == "POST" and echoed["body"] == "payload"
            assert request("/echo", "POST", body=b"x" * 1025)[0] == 413
            with reserve_upload(port, lambda: request("/echo", "POST", body=b"x")[0]) as upload:
                assert request("/echo")[0] == 200, "empty API request blocked by uploads"
                upload.sendall(b"x" * 1023)
                response = http.client.HTTPResponse(upload)
                response.begin()
                assert response.status == 200
                response.read()
            status, headers, body = request("/gzip")
            assert headers["content-encoding"] == "gzip" and gzip.decompress(body) == b"compressed bytes"
            assert "x-remove" not in request("/hop")[1]
            echoed = json.loads(request("/echo", headers={"Connection": "X-Remove", "X-Remove": "gone", "X-Forwarded-For": "attacker", "CF-Connecting-IP": "attacker"})[2])
            assert "x-remove" not in {k.lower(): v for k,v in echoed["headers"].items()} and "attacker" not in str(echoed)
            assert request("http://attacker.invalid/echo")[0] == 400
            assert request("*", "OPTIONS")[0] == 400
            assert request("/ws", headers={"Upgrade":"websocket"})[0] == 400
            with socket.create_connection(("127.0.0.1", port), timeout=5) as raw:
                raw.sendall(b"GET /echo HTTP/1.0\r\n\r\n")
                response = http.client.HTTPResponse(raw)
                response.begin()
                assert response.status == 200 and json.loads(response.read())["port"] == origin.server_port
            before = len(RECEIVED)
            assert request("//attacker.invalid/echo")[0] == 404
            assert len(RECEIVED) == before + 1 and RECEIVED[-1][0] == origin.server_port
            conn = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            started = time.monotonic()
            conn.request("GET", "/slow")
            response = conn.getresponse()
            assert response.read(4096) == b"a" * 4096 and time.monotonic() - started < .8, "response was buffered"
            conn.close()
            with socket.create_connection(("127.0.0.1", port), timeout=5) as ws:
                ws.sendall(b"GET /ws HTTP/1.1\r\nHost: proxy.example.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
                header = b""
                while b"\r\n\r\n" not in header: header += ws.recv(1)
                assert b"101 Switching Protocols" in header
                ws.sendall(bytes([0x81, 0x84]) + b"abcd" + bytes(x ^ b"abcd"[i % 4] for i,x in enumerate(b"ping")))
                received = b""
                while len(received) < 6: received += ws.recv(6-len(received))
                assert received == b"\x81\x04ping"
            # Exercise the deployment playback verifier itself with a real
            # redirecting origin, ranged media and the running proxy. It must
            # accept configured redirects and reject unexpected redirects.
            verification = ["python3", str(pathlib.Path(__file__).with_name("live_playback.py")),
                            "--proxy", f"http://127.0.0.1:{port}",
                            "--origin", f"http://127.0.0.1:{origin.server_port}"]
            fixture = json.dumps({"token": SECRET, "device_id": "fixture-device", "item_id": "fixture", "media_source_id": "fixture-source"})
            checked = subprocess.run([*verification, "--passthrough-domain", "localhost"], input=fixture,
                                     capture_output=True, text=True, timeout=20)
            assert checked.returncode == 0, checked.stderr
            checks = [json.loads(line) for line in checked.stdout.splitlines()]
            assert len(checks) == 4 and all(check["client_redirect"] for check in checks[:3])
            assert checks[3]["status"] == 302 and checks[3]["bytes"] == 0
            rejected = subprocess.run(verification, input=fixture, capture_output=True, text=True, timeout=20)
            assert rejected.returncode != 0
        finally:
            process.terminate()
            try: process.communicate(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
            origin.shutdown()
            media.shutdown()
        text = "\n".join(p.read_text() for p in logs.glob("emby-proxy.log.*"))
        assert SECRET not in text and "signed%2Bvalue" not in text
        events = [json.loads(line) for line in text.splitlines()]
        assert any(e["fields"].get("redirects") == 2 for e in events)
        assert any(e["fields"].get("route") == "websocket" for e in events)
        assert any(e["fields"].get("bytes_streamed") == 334 and e["fields"].get("complete") is True for e in events), "stream accounting missing"
        print("PASS: routing, redirect chains and configured passthrough, byte ranges, HEAD, request replay, credential isolation, framing, streaming, WebSockets, retention and log redaction")


if __name__ == "__main__":
    main()
