#!/usr/bin/env python3
"""Run a build unchanged, reporting process resource use once a minute."""
import datetime
import os
import subprocess
import sys


def report():
    print(f"\nBuild heartbeat {datetime.datetime.now(datetime.timezone.utc).isoformat()}", flush=True)
    if os.name == "nt":
        command = [
            "powershell", "-NoProfile", "-Command",
            "Get-Process | Where-Object { $_.ProcessName -match 'cargo|rustc|link|lld|clang' } "
            "| Select-Object Id,ProcessName,CPU,WorkingSet64,StartTime | Format-Table -AutoSize; "
            "Get-CimInstance Win32_OperatingSystem | Select-Object FreePhysicalMemory,TotalVisibleMemorySize",
        ]
    else:
        # CPU is a percentage; RSS is KiB. Windows CPU is cumulative seconds.
        command = ["ps", "-eo", "pid,ppid,pcpu,rss,etime,comm"]
    try:
        result = subprocess.run(command, capture_output=True, text=True, timeout=20)
        if os.name == "nt":
            print(result.stdout, flush=True)
        else:
            lines = result.stdout.splitlines()
            print("\n".join(line for i, line in enumerate(lines)
                            if i == 0 or line.rsplit(None, 1)[-1].split("/")[-1].startswith(
                                ("cargo", "rustc", "clang", "ld", "cc1"))), flush=True)
        if result.returncode:
            print(f"Resource probe failed: {result.stderr}", flush=True)
    except (OSError, subprocess.TimeoutExpired) as error:
        print(f"Resource probe unavailable: {error}", flush=True)


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: build-with-telemetry.py command [args ...]")
    child = subprocess.Popen(sys.argv[1:])
    try:
        while True:
            report()
            try:
                return child.wait(timeout=60)
            except subprocess.TimeoutExpired:
                pass
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()


if __name__ == "__main__":
    sys.exit(main())
