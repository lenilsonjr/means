#!/usr/bin/env python3
"""Check tracked working-tree files for private artifacts and host-specific paths.

This is a structural guard, not a secret scanner or a review of Git history.
"""
from pathlib import Path
import re
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
paths = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
private_dirs = {".beads", ".dolt", ".codex", ".claude", ".private-audit"}
private_suffixes = (".db", ".db-wal", ".db-shm", ".sqlite", ".sqlite3", ".atb")
host_path = re.compile(rb"/(?:Users|home)/[A-Za-z0-9_.-]+/")
errors = []
for name in filter(None, paths):
    path = root / name
    if not path.is_file():
        continue
    if Path(name).parts[0] in private_dirs or name.endswith(private_suffixes):
        errors.append(f"{name}: private artifact is tracked")
    data = path.read_bytes()
    if host_path.search(data):
        errors.append(f"{name}: host-specific home path")
    if b"Library/" + b"Mobile Documents/" in data:
        errors.append(f"{name}: personal cloud-backup path")
if errors:
    print("\n".join(errors), file=sys.stderr)
    sys.exit(1)
print("Tracked-tree privacy structure check passed. Review fixture contents and scan secrets separately.")
