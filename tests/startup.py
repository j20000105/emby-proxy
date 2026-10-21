#!/usr/bin/env python3
"""Missing configuration fails before creating runtime resources or listening."""
import pathlib
import socket
import subprocess
import sys
import tempfile


def main():
    binary = pathlib.Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="emby-startup-") as temporary:
        directory = pathlib.Path(temporary)
        config = directory / "proxy.toml"
        control = directory / "control.sock"
        logs = directory / "logs"
        with socket.socket() as reserve:
            reserve.bind(("127.0.0.1", 0))
            port = reserve.getsockname()[1]
        settings = (f'listen="127.0.0.1:{port}"\nlog_directory="{logs}"\n'
                    f'control_socket="{control}"\n')

        def rejected(arguments, message):
            result = subprocess.run([str(binary), *arguments], cwd=directory,
                                    capture_output=True, text=True, timeout=5)
            assert result.returncode != 0, result.stdout
            assert message in result.stderr, result.stderr
            assert "private-marker" not in result.stderr, "configuration contents leaked"
            assert not logs.exists() and not control.exists()
            with socket.socket() as check:
                check.bind(("127.0.0.1", port))

        for command in ([], ["--check-config"]):
            rejected(["--config", str(config), *command], "configuration file")
            # Do not touch an operator's configuration if this machine has one.
            if not pathlib.Path("/etc/emby-proxy/proxy.toml").exists():
                rejected(command, "configuration file")

        for contents in ["", "# configure before starting", settings,
                         settings + 'default_upstream=""\n',
                         settings + 'default_upstream="   "\n',
                         settings + '[routes]\n"proxy.example.com"="https://origin.example.com"\n',
                         pathlib.Path(__file__).resolve().parents[1].joinpath("examples/proxy.toml").read_text()]:
            config.write_text(contents)
            for command in ([], ["--check-config"]):
                rejected(["--config", str(config), *command], "missing required configuration: default_upstream")

        config.write_text(settings + 'default_upstream="https://example.com/?token=private-marker"\ninvalid=[\n')
        rejected(["--config", str(config)], "invalid configuration")
        config.write_text(settings + 'default_upstream="https://origin.example.com"\n')
        checked = subprocess.run([str(binary), "--config", str(config), "--check-config"],
                                 capture_output=True, text=True, timeout=5)
        assert checked.returncode == 0, checked.stderr
        assert not logs.exists() and not control.exists()
        for command in ("--help", "--version"):
            assert subprocess.run([str(binary), command], capture_output=True, timeout=5).returncode == 0
    print("PASS: required configuration/upstream, startup diagnostics, no runtime side effects, redacted parse errors")


if __name__ == "__main__":
    main()
