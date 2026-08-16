# lucidlint: ignore-file fakefs the LSP test spawns the real binary over a
# pipe — subprocess interop, the same named real-FS exception as the parity gate
"""End-to-end LSP session: the server speaks stdio JSON-RPC over a pipe and
publishes diagnostics from the in-process scan core."""

import json
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parent.parent
BINARY = ROOT / "scanner" / "target" / "release" / "lucidlint"


def frame(msg) -> bytes:
    body = json.dumps(msg).encode()
    return b"Content-Length: %d\r\n\r\n%s" % (len(body), body)


def read_msg(proc) -> dict:
    headers = {}
    while True:
        line = proc.stdout.readline()
        if line in (b"\r\n", b"\n"):
            break
        key, _, value = line.decode().partition(":")
        headers[key.strip().lower()] = value.strip()
    return json.loads(proc.stdout.read(int(headers["content-length"])))


def session() -> tuple[subprocess.Popen, object]:
    proc = subprocess.Popen(
        [str(BINARY), "--lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE
    )
    proc.stdin.write(frame({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}))
    proc.stdin.flush()
    return proc, read_msg(proc)


@pytest.fixture(scope="module")
def binary() -> Path:
    if not BINARY.exists():
        subprocess.run(
            ["cargo", "build", "--release", "--manifest-path", str(ROOT / "scanner" / "Cargo.toml")],
            check=True,
        )
    return BINARY


def test_initialize_and_publish_diagnostics(binary):
    proc, init = session()
    assert init["id"] == 1
    assert init["result"]["capabilities"]["textDocumentSync"]["change"] == 1

    bad = "def f():\n    try:\n        g()\n    except Exception:\n        log('x')\n    return a * 60\n"
    proc.stdin.write(
        frame(
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {"textDocument": {"uri": "file:///tmp/buf.py", "text": bad}},
            }
        )
    )
    proc.stdin.flush()
    diag = read_msg(proc)
    assert diag["method"] == "textDocument/publishDiagnostics"
    messages = [d["message"] for d in diag["params"]["diagnostics"]]
    assert any("magic number 60" in m for m in messages)
    assert any("swallows" in m for m in messages)
    assert all(d["range"]["start"]["line"] >= 0 for d in diag["params"]["diagnostics"])
    proc.kill()


def test_did_change_updates_diagnostics(binary):
    proc, _ = session()
    bad = "def f():\n    return a * 60\n"
    fixed = "def f():\n    return 60\n"
    proc.stdin.write(
        frame(
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {"textDocument": {"uri": "file:///tmp/buf2.py", "text": bad}},
            }
        )
    )
    proc.stdin.flush()
    first = read_msg(proc)
    assert any("magic number" in d["message"] for d in first["params"]["diagnostics"])

    proc.stdin.write(
        frame(
            {
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": {"uri": "file:///tmp/buf2.py"},
                    "contentChanges": [{"text": fixed}],
                },
            }
        )
    )
    proc.stdin.flush()
    second = read_msg(proc)
    assert not any("magic number" in d["message"] for d in second["params"]["diagnostics"])
    proc.kill()


def test_shutdown_and_exit(binary):
    proc, _ = session()
    proc.stdin.write(frame({"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}}))
    proc.stdin.flush()
    assert read_msg(proc)["result"] is None
    proc.stdin.write(frame({"jsonrpc": "2.0", "method": "exit"}))
    proc.stdin.flush()
    assert proc.wait(timeout=5) == 0
