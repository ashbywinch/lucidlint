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


def send(proc: subprocess.Popen, msg) -> None:
    """One JSON-RPC frame out. Popen types stdin Optional — a live session owns the pipe."""
    stdin = proc.stdin
    assert stdin is not None
    stdin.write(frame(msg))
    stdin.flush()


def session() -> tuple[subprocess.Popen, dict]:
    proc = subprocess.Popen(
        [str(BINARY), "--lsp"], stdin=subprocess.PIPE, stdout=subprocess.PIPE
    )
    send(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})
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
    send(
        proc,
        {
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///tmp/buf.py", "text": bad}},
        },
    )
    diag = read_msg(proc)
    assert diag["method"] == "textDocument/publishDiagnostics"
    messages = [d["message"] for d in diag["params"]["diagnostics"]]
    assert any("magic number 60" in m for m in messages)
    assert any("swallows" in m for m in messages)
    assert all(d["range"]["start"]["line"] >= 0 for d in diag["params"]["diagnostics"])
    proc.kill()


def test_diagnostics_payload_never_carries_the_report_header(binary):
    # the header is a REPORT surface (CLI text/json) — per-buffer LSP
    # diagnostics are editor UI, not the report; the banner must never leak
    proc, _ = session()
    bad = "def f():\n    return a * 60\n"
    send(
        proc,
        {
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///tmp/buf3.py", "text": bad}},
        },
    )
    diag = read_msg(proc)
    assert diag["params"]["diagnostics"], "the buffer must actually be scanned"
    assert "obviously correct" not in json.dumps(diag)
    proc.kill()


def test_did_change_updates_diagnostics(binary):
    proc, _ = session()
    bad = "def f():\n    return a * 60\n"
    fixed = "def f():\n    return 60\n"
    send(
        proc,
        {
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {"uri": "file:///tmp/buf2.py", "text": bad}},
        },
    )
    first = read_msg(proc)
    assert any("magic number" in d["message"] for d in first["params"]["diagnostics"])

    send(
        proc,
        {
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": "file:///tmp/buf2.py"},
                "contentChanges": [{"text": fixed}],
            },
        },
    )
    second = read_msg(proc)
    assert not any("magic number" in d["message"] for d in second["params"]["diagnostics"])
    proc.kill()


def test_shutdown_and_exit(binary):
    proc, _ = session()
    send(proc, {"jsonrpc": "2.0", "id": 2, "method": "shutdown", "params": {}})
    assert read_msg(proc)["result"] is None
    send(proc, {"jsonrpc": "2.0", "method": "exit"})
    assert proc.wait(timeout=5) == 0
