"""Close a hidden, dedicated test console and verify mapping cleanup before exit."""
import ctypes
from ctypes import wintypes
import os
import pathlib
import subprocess
import sys
import tempfile
import time

target = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target").resolve()
profile = sys.argv[2] if len(sys.argv) > 2 else "debug"
fixture = next(
    p for p in sorted((target / profile / "deps").glob("upnp_engage-*.exe"), key=lambda p: p.stat().st_mtime, reverse=True)
    if "session::tests::signal_fixture: test" in subprocess.run(
        [str(p), "--list"], capture_output=True, text=True, timeout=8
    ).stdout
)
post = ctypes.windll.user32.PostMessageW
post.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
post.restype = wintypes.BOOL

with tempfile.TemporaryDirectory(prefix="upnp-console-") as directory:
    log = pathlib.Path(directory) / "signal.log"
    env = dict(os.environ, UPNP_TEST_LOG=str(log))
    startup = subprocess.STARTUPINFO()
    startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startup.wShowWindow = 0
    child = subprocess.Popen([str(fixture), "--exact", "session::tests::signal_fixture", "--ignored", "--nocapture"], env=env,
                             creationflags=subprocess.CREATE_NEW_CONSOLE, startupinfo=startup)
    try:
        deadline = time.monotonic() + 10
        ready = log.with_suffix(".ready")
        while not ready.exists():
            assert time.monotonic() < deadline and child.poll() is None, "Console fixture did not start"
            time.sleep(0.02)
        handle = int(ready.read_text())
        assert handle and post(handle, 0x0010, 0, 0), "Cannot close the test console"
        child.wait(timeout=6)
        contents = log.read_text()
        assert "remove TCP 9000" in contents and "remove UDP 9000" in contents and "cleaned" in contents, contents
        print("PASS: closing a Windows console removes both mappings before termination")
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=5)
