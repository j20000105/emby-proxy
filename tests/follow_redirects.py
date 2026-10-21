#!/usr/bin/env python3
"""Exercise the follow allowlist against real local HTTP origins and a proxy."""
import http.client
import json
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse

import protocol


def main():
    binary = pathlib.Path(sys.argv[1]).resolve()
    origin, media = protocol.start_backend(), protocol.start_backend()
    protocol.MEDIA = media.server_port
    with socket.socket() as reserve:
        reserve.bind(("127.0.0.1", 0))
        port = reserve.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="emby-follow-") as directory:
        directory = pathlib.Path(directory)
        config = directory / "proxy.toml"
        config.write_text(f'''listen = "127.0.0.1:{port}"
default_upstream = "http://localhost:{origin.server_port}"
log_directory = "{directory}/logs"
control_socket = "{directory}/control.sock"
redirect_follow_domains = ["localhost"]
allow_private_upstreams = true
read_timeout_seconds = 2
''')
        process = subprocess.Popen([str(binary), "--config", str(config)], stdout=subprocess.PIPE, stderr=subprocess.PIPE)

        def request(path, method="GET", headers=None, body=None):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=6)
            connection.request(method, path, body, headers or {})
            response = connection.getresponse()
            result = response.status, {k.lower(): v for k, v in response.getheaders()}, response.read()
            connection.close()
            return result

        def redirect(target, code=302, **kwargs):
            return request("/passthrough?" + urllib.parse.urlencode({"target": target, "status": code}), **kwargs)

        try:
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError("proxy exited")
                try:
                    assert request("/echo")[0] == 200
                    break
                except ConnectionRefusedError:
                    time.sleep(.03)
            else:
                raise AssertionError("proxy did not start")

            # Localhost represents example.com: only it is fetched. A different
            # hostname is unlisted even when it resolves to the same address.
            for code in (301, 302, 303, 307, 308):
                for method in ("GET", "HEAD"):
                    before = len(protocol.RECEIVED)
                    status, headers, body = redirect(f"http://localhost:{media.server_port}/media?sig=a%2Fb+z", code,
                                                     method=method, headers={"Range": "bytes=123-456", "Authorization": protocol.SECRET, "Cookie": protocol.SECRET})
                    assert status == 206 and "location" not in headers
                    assert headers["content-range"] == f"bytes 123-456/{len(protocol.DATA)}"
                    assert body == (b"" if method == "HEAD" else protocol.DATA[123:457])
                    assert len(protocol.RECEIVED) == before + 2
                    assert protocol.SECRET not in str(protocol.RECEIVED[-1])

                    before = len(protocol.RECEIVED)
                    status, headers, body = redirect(protocol.SIGNED_LOCATION, code, method=method)
                    assert status == code and headers["location"] == protocol.SIGNED_LOCATION
                    assert body == (b"" if method == "HEAD" else b"redirect body")
                    assert len(protocol.RECEIVED) == before + 1

            for target in [
                "https://edge.cdn.example.net/media?sig=a%2Fb+z",
                "https://storage.alias.example.net/media?sig=a%2Fb+z",
                "https://notlocalhost/media", "https://localhost.evil.example/media",
                f"http://127.0.0.1:{media.server_port}/media?localhost",
            ]:
                before = len(protocol.RECEIVED)
                status, headers, _ = redirect(target)
                assert status == 302 and headers["location"] == target
                assert len(protocol.RECEIVED) == before + 1

            # Same-origin relative references are allowed and retain signed queries.
            assert redirect("/media?sig=a%2Fb+z", headers={"Range": "bytes=0-31"})[2] == protocol.DATA[:32]
            assert protocol.RECEIVED[-1][1] == "/media?sig=a%2Fb+z"
            target = f"http://localhost:{media.server_port}/passthrough"
            status, headers, _ = redirect(target)
            assert status == 302 and headers["location"] == protocol.SIGNED_LOCATION and "set-cookie" not in headers
            assert request("/passthrough-limit/10")[0] == 302
            assert request("/passthrough-limit/11")[0] == 502
            assert request("/loop")[0] == 502
            for target in ["ftp://edge.cdn.example.net/file", "https://user:password@edge.cdn.example.net/file"]:
                assert redirect(target)[0] == 502
            for code in (301, 302, 303, 307, 308):
                result = redirect("/echo", code, method="POST", body=b"payload")
                echoed = json.loads(result[2])
                assert echoed["method"] == ("POST" if code in (307, 308) else "GET")
                assert echoed["body"] == ("payload" if code in (307, 308) else "")
                assert redirect(f"http://localhost:{media.server_port}/echo", code, method="POST", body=b"payload")[0] == (502 if code in (307, 308) else 200)
                assert redirect(protocol.SIGNED_LOCATION, code, method="POST", body=b"payload")[0] == code

            # Test the rollout verifier in both directions of the new policy.
            fixture = json.dumps({"token": protocol.SECRET, "device_id": "fixture", "item_id": "fixture", "media_source_id": "fixture"})
            verification = [sys.executable, str(pathlib.Path(__file__).with_name("live_playback.py")),
                            "--origin", f"http://localhost:{origin.server_port}", "--proxy", f"http://127.0.0.1:{port}"]
            for domains in (["localhost"], []):
                config.write_text(config.read_text().replace('redirect_follow_domains = ["localhost"]', 'redirect_follow_domains = []') if not domains else config.read_text())
                subprocess.run([str(binary), "--config", str(config), "reload"], check=True, capture_output=True, timeout=10)
                checked = subprocess.run([*verification, "--follow-domains", *domains], input=fixture, capture_output=True, text=True, timeout=20)
                assert checked.returncode == 0, checked.stderr
                checks = [json.loads(line) for line in checked.stdout.splitlines()]
                assert len(checks) == 4 and all(check["client_redirect"] == (not domains) for check in checks[:3])
                assert checks[3]["status"] == (200 if domains else 302)
            rejected = subprocess.run([*verification, "--follow-domains", "localhost"], input=fixture, capture_output=True, text=True, timeout=20)
            assert rejected.returncode != 0
        finally:
            process.terminate()
            try:
                process.communicate(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
            origin.shutdown()
            media.shutdown()
        logs = "\n".join(p.read_text() for p in (directory / "logs").glob("emby-proxy.log.*"))
        assert protocol.SECRET not in logs and "signed%2Bvalue" not in logs
        print("PASS: follow-only domains, DNS-name boundaries, signed external redirects, ranges/HEAD, bounded chains, credentials, replay and playback verifier")


if __name__ == "__main__":
    main()
