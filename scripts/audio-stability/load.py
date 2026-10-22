#!/usr/bin/env python3
"""Keep CPU, memory, and paced I/O workers inside the caller's bounded user scope."""

import argparse
from pathlib import Path
import signal
import subprocess
import time
import os


def terminate(_signal, _frame):
    raise SystemExit(0)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seconds", type=int, required=True)
    parser.add_argument("--directory", type=Path, required=True)
    args = parser.parse_args()
    assert 1 <= args.seconds <= 3610
    signal.signal(signal.SIGTERM, terminate)
    worker = subprocess.Popen(["stress-ng", "--cpu", "2", "--vm", "1", "--vm-bytes", "512M",
                               "--timeout", f"{args.seconds}s", "--metrics-brief"])
    block = bytes([0xA5]) * (64 * 1024)
    deadline = time.monotonic() + args.seconds
    try:
        with (args.directory / "paced-io.bin").open("wb", buffering=0) as output:
            writes = 0
            while time.monotonic() < deadline:
                assert worker.poll() is None, "stress-ng exited before its deadline"
                if output.tell() >= 64 * 1024 * 1024:
                    output.seek(0)
                output.write(block)
                writes += 1
                if writes % 8 == 0:
                    os.fsync(output.fileno())
                time.sleep(0.125)
    finally:
        if worker.poll() is None:
            worker.terminate()
            try:
                worker.wait(timeout=3)
            except subprocess.TimeoutExpired:
                worker.kill()
                worker.wait(timeout=3)


if __name__ == "__main__":
    main()
