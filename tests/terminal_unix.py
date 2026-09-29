"""Exercise the real terminal UI with the test-only router. Never opens router ports."""
import json
import os
import pathlib
import select
import shutil
import signal
import subprocess
import sys
import tempfile
import time

target = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target").resolve()
profile = sys.argv[2] if len(sys.argv) > 2 else "debug"
candidates = [p for p in (target / profile / "deps").glob("upnp_engage-*") if p.is_file() and os.access(p, os.X_OK)]
fixture = next(
    p for p in sorted(candidates, key=lambda p: p.stat().st_mtime, reverse=True)
    if "tests::interactive_fixture: test" in subprocess.run(
        [str(p), "--list"], capture_output=True, text=True, timeout=8
    ).stdout
)
binary = target / profile / "upnp-engage"


class Terminal:
    def __init__(self, env):
        self.pid, self.fd = os.forkpty()
        if not self.pid:
            os.execve(str(fixture), [str(fixture), "--exact", "tests::interactive_fixture", "--ignored", "--nocapture"], env)
        self.text = b""
        self.alive = True

    def expect(self, text):
        expected = text.encode()
        deadline = time.monotonic() + 8
        while expected not in self.text:
            if time.monotonic() > deadline:
                raise AssertionError(f"Missing {text!r}. Terminal output: {self.text.decode(errors='replace')}")
            if select.select([self.fd], [], [], 0.1)[0]:
                try:
                    chunk = os.read(self.fd, 8192)
                except OSError as error:
                    raise AssertionError(f"Terminal closed before {text!r}: {self.text.decode(errors='replace')}") from error
                assert chunk, f"Terminal closed before {text!r}"
                self.text += chunk
        self.text = self.text.split(expected, 1)[1]

    def send(self, text):
        os.write(self.fd, text.encode())

    def wait(self):
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            pid, status = os.waitpid(self.pid, os.WNOHANG)
            if pid:
                self.alive = False
                assert os.waitstatus_to_exitcode(status) == 0, f"Fixture exit status: {status}"
                return
            time.sleep(0.02)
        raise AssertionError("Fixture did not exit")

    def close(self):
        if self.alive:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        os.close(self.fd)


with tempfile.TemporaryDirectory(prefix="upnp-tests-") as directory:
    root = pathlib.Path(directory)
    env = os.environ.copy()
    env.pop("WAYLAND_DISPLAY", None)
    env.pop("UPNP_ENGAGE_TERMINAL_CHILD", None)
    env["TERM"] = "xterm-256color"
    env["DISPLAY"] = ":invalid"
    env["UPNP_TEST_LOG"] = str(root / "mapping.log")
    env["UPNP_TEST_CONFIG"] = str(root / "config.toml")
    config = root / "config.toml"

    # Start without saving, survive clipboard failure, change and save, cancel an edit, quit.
    terminal = Terminal(env)
    try:
        terminal.expect("Device port")
        terminal.send("65536\r")
        terminal.expect("Enter a whole port number")
        terminal.send("8080\r")
        terminal.expect("Router port")
        terminal.send("\r")
        terminal.expect("Save these settings")
        terminal.send("n\r")
        terminal.expect("203.0.113.1:8080")
        assert not config.exists()
        if sys.platform == "linux":
            terminal.send("c")
            terminal.expect("Clipboard unavailable")
        terminal.send("p")
        terminal.expect("Device port")
        terminal.send("8081\r")
        terminal.expect("Router port")
        terminal.send("9001\r")
        terminal.expect("Save these settings")
        terminal.send("y\r")
        terminal.expect("Saved ")
        terminal.expect("203.0.113.1:9001")
        assert "device_port = 8081" in config.read_text()
        assert "router_port = 9001" in config.read_text()
        terminal.send("p")
        terminal.expect("Device port")
        terminal.send("\x1b")
        terminal.expect("Cancelled.")
        terminal.send("q")
        terminal.wait()
    finally:
        terminal.close()
    log = (root / "mapping.log").read_text()
    assert log.count("add ") == 4 and log.count("remove ") == 4, log
    print("PASS: first-run prompts, validation, no-save, change/save, cancel, Q cleanup")

    # Existing config starts automatically; raw Ctrl+C cleans both protocols.
    terminal = Terminal(env)
    try:
        terminal.expect("203.0.113.1:9001")
        terminal.send("\x03")
        terminal.wait()
    finally:
        terminal.close()
    assert (root / "mapping.log").read_text().count("remove ") == 6
    print("PASS: automatic config startup and raw Ctrl+C cleanup")

    # Native macOS clipboard tests run only on disposable CI runners.
    if sys.platform == "darwin" and os.environ.get("GITHUB_ACTIONS") == "true":
        terminal = Terminal(env)
        try:
            terminal.expect("203.0.113.1:9001")
            terminal.send("c")
            terminal.expect("Copied 203.0.113.1:9001")
            pasted = subprocess.run(["/usr/bin/pbpaste"], capture_output=True, text=True, check=True, timeout=8)
            assert pasted.stdout == "203.0.113.1:9001", pasted.stdout
            terminal.send("q")
            terminal.wait()
        finally:
            terminal.close()
        print("PASS: macOS clipboard paste from another process")

    # Copy/paste on an isolated display, never the user's clipboard.
    if sys.platform == "linux" and shutil.which("Xvfb"):
        # Use Linux abstract sockets; WSLg may own a read-only /tmp/.X11-unix.
        server = subprocess.Popen(["Xvfb", "-displayfd", "1", "-screen", "0", "800x600x24", "-nolisten", "tcp", "-nolisten", "unix"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        try:
            assert select.select([server.stdout], [], [], 8)[0], "Xvfb did not start"
            display = server.stdout.readline().decode().strip()
            assert display.isdecimal()
            env["DISPLAY"] = ":" + display
            terminal = Terminal(env)
            try:
                terminal.expect("203.0.113.1:9001")
                terminal.send("c")
                terminal.expect("Copied 203.0.113.1:9001")
                env["UPNP_TEST_CLIPBOARD"] = str(root / "clipboard.txt")
                subprocess.run([str(fixture), "--exact", "tests::clipboard_fixture", "--ignored"], env=env, check=True, capture_output=True, timeout=8)
                assert (root / "clipboard.txt").read_text() == "203.0.113.1:9001"
                terminal.send("q")
                terminal.wait()
            finally:
                terminal.close()
            print("PASS: X11 clipboard ownership and paste from another process")
        finally:
            server.terminate()
            server.wait(timeout=5)
    else:
        print("SKIP: X11 clipboard (Xvfb unavailable)")

    if sys.platform != "linux":
        sys.exit(0)

    # Desktop launching forwards literal arguments to the preferred terminal.
    launcher = root / "xdg-terminal-exec"
    launcher.write_text("#!/usr/bin/python3\nimport json, os, sys\njson.dump([sys.argv[1:], os.environ.get('UPNP_ENGAGE_TERMINAL_CHILD')], open(os.environ['UPNP_LAUNCH_LOG'], 'w'))\n")
    launcher.chmod(0o755)
    env.update(PATH=str(root), DISPLAY=":unused", UPNP_LAUNCH_LOG=str(root / "launch.json"))
    config_arg = str(root / "config with $spaces.toml")
    subprocess.run([str(binary), "--config", config_arg], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, check=True, timeout=8)
    args, marker = json.loads((root / "launch.json").read_text())
    assert args == ["--", str(binary), "--config", config_arg] and marker == "1", args
    (root / "launch.json").unlink()
    env["UPNP_ENGAGE_TERMINAL_CHILD"] = "1"
    result = subprocess.run([str(binary), "--config", config_arg], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, timeout=8)
    assert result.returncode != 0 and not (root / "launch.json").exists()
    print("PASS: preferred-terminal relaunch, literal arguments, and loop prevention")
