#!/usr/bin/env python3
"""Exercise config editing and acknowledged, atomic reload against a live proxy."""
import concurrent.futures
import http.client
import json
import os
import pathlib
import socket
import stat
import subprocess
import sys
import tempfile
import time

from protocol import reserve_upload, start_backend


def main():
    binary = pathlib.Path(sys.argv[1]).resolve()
    origin, alternate = start_backend(), start_backend()
    origins = [f"http://127.0.0.1:{server.server_port}" for server in (origin, alternate)]
    with tempfile.TemporaryDirectory(prefix="emby-management-") as temporary:
        directory = pathlib.Path(temporary)
        config = directory / "proxy.toml"
        control = directory / "control.sock"
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        config.write_text(f'''# Keep this operator comment.
listen = "127.0.0.1:{port}"
default_upstream = "{origins[0]}"
log_directory = "{directory}/logs"
control_socket = "{control}"
allow_private_upstreams = true
max_request_body_bytes = 1024
max_buffered_request_bytes = 1024
''')
        config.chmod(0o640)

        def cli(*args, success=True, path=config):
            result = subprocess.run([str(binary), "--config", str(path), *args],
                                    capture_output=True, text=True, timeout=15)
            assert (result.returncode == 0) == success, (args, result.stdout, result.stderr)
            return result.stdout if success else result.stderr

        def request(host="unknown.example", method="GET", body=None, path="/echo"):
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            connection.request(method, path, body=body, headers={"Host": host})
            response = connection.getresponse()
            data = response.read()
            connection.close()
            return response.status, data

        def routed(host="unknown.example"):
            status, body = request(host)
            assert status == 200
            return json.loads(body)["port"]

        def raw_reload(path=config, command="reload"):
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(5)
                connection.connect(str(control))
                connection.sendall(json.dumps({"command": command, "config_path": str(path)}).encode())
                connection.shutdown(socket.SHUT_WR)
                return json.loads(connection.makefile("rb").read())

        # File management must work with no daemon running, and preserve comments.
        cli("route", "add", "EXAMPLE.COM.:80", origins[1])
        config.write_text(config.read_text().replace(f'= "{origins[1]}"', f'= "{origins[1]}" # upstream comment'))
        assert cli("route", "get", "example.com").strip() == origins[1]
        assert json.loads(cli("route", "list"))["routes"] == {"example.com": origins[1]}
        for arguments in [("route", "add", "example.com", origins[0]),
                          ("route", "update", "missing.example", origins[0]),
                          ("route", "delete", "missing.example"),
                          ("route", "add", "bad.example", "https://user:password@example.com")]:
            before = config.read_bytes()
            cli(*arguments, success=False)
            assert config.read_bytes() == before
        assert stat.S_IMODE(config.stat().st_mode) == 0o640
        assert config.read_text().startswith("# Keep this operator comment.")
        # A relative filename must edit and fsync the current directory correctly.
        subprocess.run([str(binary), "--config", "proxy.toml", "default", origins[0]],
                       cwd=directory, check=True, capture_output=True, timeout=15)
        symlink = directory / "symlink.toml"
        symlink.symlink_to(config)
        cli("default", origins[1], path=symlink, success=False)
        # Concurrent writers must retain every independent edit.
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
            list(executor.map(lambda i: cli("route", "add", f"worker-{i}.example", origins[1]), range(12)))
        routes = json.loads(cli("route", "list"))["routes"]
        assert len(routes) == 13
        for i in range(12):
            cli("route", "delete", f"worker-{i}.example")

        # Recover only an actual stale socket; do not overwrite arbitrary files.
        with socket.socket(socket.AF_UNIX) as stale:
            stale.bind(str(control))
        process = subprocess.Popen([str(binary), "--config", str(config)],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError(process.stderr.read().decode())
                try:
                    routed()
                    break
                except ConnectionRefusedError:
                    time.sleep(.03)
            else:
                raise AssertionError("proxy did not start")
            assert stat.S_IMODE(control.stat().st_mode) == 0o600
            assert routed("example.com") == alternate.server_port
            cli("route", "update", "example.com", origins[0])
            assert "# upstream comment" in config.read_text()
            assert routed("example.com") == alternate.server_port, "edit applied without reload"
            cli("reload")
            assert routed("example.com") == origin.server_port
            cli("route", "delete", "example.com")
            cli("default", origins[1])
            assert routed() == origin.server_port
            cli("reload")
            assert routed() == alternate.server_port and routed("example.com") == alternate.server_port
            assert process.poll() is None, "reload restarted the process"

            # The redirect policy changes only on reload, and invalid updates
            # cannot replace the working policy or routes.
            assert request(path="/passthrough-local")[0] == 200
            original = config.read_text()
            enabled = 'redirect_passthrough_domains = ["LOCALHOST."]\n' + original
            config.write_text(enabled)
            assert request(path="/passthrough-local")[0] == 200
            cli("reload")
            assert request(path="/passthrough-local")[0] == 302
            config.write_text(enabled.replace('"LOCALHOST."', '"*.localhost"'))
            cli("reload", success=False)
            assert raw_reload()["ok"] is False
            assert request(path="/passthrough-local")[0] == 302
            config.write_text('redirect_passthrough_domains = []\n' + original)
            cli("reload")
            assert request(path="/passthrough-local")[0] == 200
            assert process.poll() is None

            followed = 'redirect_follow_domains = ["LOCALHOST."]\n' + original
            config.write_text(followed)
            cli("reload")
            assert request(path="/passthrough-local")[0] == 200
            config.write_text(followed.replace('["LOCALHOST."]', '["example.com"]'))
            assert request(path="/passthrough-local")[0] == 200, "follow-list edit applied before reload"
            cli("reload")
            assert request(path="/passthrough-local")[0] == 302
            for bad in [followed.replace('"LOCALHOST."', '"*.example.com"'),
                        'redirect_passthrough_domains = ["cdn.example.net"]\n' + followed]:
                config.write_text(bad)
                cli("reload", success=False)
                assert raw_reload()["ok"] is False
                assert request(path="/passthrough-local")[0] == 302
            config.write_text('redirect_follow_domains = []\n' + original)
            cli("reload")
            assert request(path="/passthrough-local")[0] == 302
            config.write_text(original)
            cli("reload")
            assert request(path="/passthrough-local")[0] == 200
            assert process.poll() is None

            good = config.read_text()
            for bad in ["this is not TOML", good + '\nunknown_setting = true\n',
                        good.replace(f'default_upstream = "{origins[1]}"', ''),
                        good.replace(f'default_upstream = "{origins[1]}"', 'default_upstream = ""'),
                        good.replace(origins[1], "file:///etc/passwd"),
                        good + '\n"DUP.example" = "https://one.example"\n"dup.EXAMPLE" = "https://two.example"\n']:
                config.write_text(bad)
                assert "reload not sent" in cli("reload", success=False)
                # The daemon repeats validation even when bypassing the CLI.
                assert raw_reload()["ok"] is False
                assert routed() == alternate.server_port
            # A valid file that changes a startup-only setting cannot partially apply.
            config.write_text(good.replace(origins[1], origins[0]).replace(f"127.0.0.1:{port}", "127.0.0.1:0"))
            assert "requires a service restart" in cli("reload", success=False)
            assert routed() == alternate.server_port
            config.write_text(good)
            # Oversized configs and named pipes fail before reading indefinitely.
            huge = directory / "huge.toml"
            huge.write_bytes(b"#" * (1024 * 1024 + 1))
            cli("reload", path=huge, success=False)
            fifo = directory / "pipe.toml"
            os.mkfifo(fifo)
            cli("reload", path=fifo, success=False)
            other = directory / "other.toml"
            other.write_text(good)
            assert "does not match" in cli("reload", path=other, success=False)
            duplicate = directory / "duplicate.toml"
            duplicate.write_text(good.replace(f"127.0.0.1:{port}", "127.0.0.1:0"))
            assert "control socket lock" in cli(path=duplicate, success=False)
            cli("reload")
            assert raw_reload(command="arbitrary")["ok"] is False

            # Reload must not release reservations held by unfinished uploads.
            with reserve_upload(port, lambda: request(method="POST", body=b"x")[0]) as upload:
                cli("reload")
                assert request(method="POST", body=b"x")[0] == 503
                assert request()[0] == 200
                upload.sendall(b"x" * 1023)
                response = http.client.HTTPResponse(upload)
                response.begin()
                assert response.status == 200
                response.read()

            # Both media streaming and an already upgraded WebSocket survive reload.
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=5)
            connection.request("GET", "/slow")
            response = connection.getresponse()
            assert response.read(4096) == b"a" * 4096
            cli("default", origins[0])
            cli("reload")
            assert routed() == origin.server_port
            assert response.read() == b"b" * (65536 - 4096)
            connection.close()
            with socket.create_connection(("127.0.0.1", port), timeout=5) as ws:
                ws.sendall(b"GET /ws HTTP/1.1\r\nHost: test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n")
                header = b""
                while b"\r\n\r\n" not in header:
                    header += ws.recv(1)
                assert b"101 Switching Protocols" in header
                cli("reload")
                ws.sendall(bytes([0x81, 0x84]) + b"abcd" + bytes(x ^ b"abcd"[i % 4] for i, x in enumerate(b"ping")))
                received = b""
                while len(received) < 6:
                    received += ws.recv(6 - len(received))
                assert received == b"\x81\x04ping"
            assert process.poll() is None
        finally:
            process.terminate()
            try:
                process.communicate(timeout=8)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()
            origin.shutdown()
            alternate.shutdown()
        assert not control.exists(), "control socket left behind after shutdown"
        print("PASS: offline CRUD, concurrent atomic edits, permissions, explicit validated reload, rejected-config preservation, in-flight budget, media and WebSocket continuity")


if __name__ == "__main__":
    main()
