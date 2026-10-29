"""Reject private home-directory build paths in yorusphoto's native binaries.

Only the editor and CLI built by this project belong in this gate. Third-party
runtime binaries and license notices have their own provenance checks. Matches
are reported as categories and byte offsets, never as identity-bearing strings.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re


CHUNK_BYTES = 1024 * 1024
MAX_BINARY_BYTES = 512 * 1024 * 1024
MAX_REPORTED_MATCHES = 24
_OVERLAP_BYTES = 128


def _encoded_pattern(pattern, encoding):
    """Encode a byte regex whose only metacharacter is the drive-letter class."""
    drive, suffix = pattern
    encoded = re.escape(suffix.encode(encoding))
    if not drive:
        return re.compile(encoded)
    if encoding == "utf-16-le":
        return re.compile(rb"[a-zA-Z]\x00" + encoded, re.IGNORECASE)
    if encoding == "utf-16-be":
        return re.compile(rb"\x00[a-zA-Z]" + encoded, re.IGNORECASE)
    return re.compile(rb"[a-zA-Z]" + encoded, re.IGNORECASE)


def _patterns():
    patterns = []
    def add(category, drive, suffix, encoding):
        if encoding == "ascii":
            pattern = _encoded_pattern((drive, suffix), encoding)
        else:
            little = _encoded_pattern((drive, suffix), "utf-16-le").pattern
            big = _encoded_pattern((drive, suffix), "utf-16-be").pattern
            # A UTF-16 string may also match the opposite byte order one byte
            # away. One alternation reports that byte range only once.
            pattern = re.compile(b"(?:" + little + b"|" + big + b")", re.IGNORECASE if drive else 0)
        patterns.append((category, encoding, pattern))
    for encoding in ("ascii", "utf-16"):
        for separator in ("\\", "/"):
            for folder_separator in ("\\", "/"):
                add("windows-user-home", True, ":" + separator + "Users" + folder_separator, encoding)
        for root, category in (("/Users/", "macos-user-home"), ("/home/", "linux-user-home")):
            add(category, False, root, encoding)
    return patterns


_PATTERNS = _patterns()


def scan_stream(stream, *, chunk_bytes=CHUNK_BYTES, max_bytes=MAX_BINARY_BYTES):
    """Scan incrementally with fixed memory and preserve matches across reads."""
    if not 1 <= chunk_bytes <= CHUNK_BYTES or not 0 <= max_bytes <= MAX_BINARY_BYTES:
        raise ValueError("Invalid privacy scan limits")
    total = 0
    digest = hashlib.sha256()
    tail = b""
    counts = {}
    matches = []
    while True:
        block = stream.read(min(chunk_bytes, max_bytes - total + 1))
        if not block:
            break
        if len(block) > max_bytes - total:
            return {"passed": False, "error": "Native binary exceeds the privacy scan size limit"}
        digest.update(block)
        window = tail + block
        base = total - len(tail)
        for category, encoding, pattern in _PATTERNS:
            for match in pattern.finditer(window):
                # The overlap was already checked; retain only newly complete matches.
                if base + match.end() <= total:
                    continue
                key = category + ":" + encoding
                counts[key] = counts.get(key, 0) + 1
                if len(matches) < MAX_REPORTED_MATCHES:
                    matches.append({"category": category, "encoding": encoding, "offset": base + match.start()})
        total += len(block)
        tail = window[-_OVERLAP_BYTES:]
    return {"passed": not counts, "bytes": total, "sha256": digest.hexdigest(),
            "matchCounts": counts, "matches": matches}


def verify_native_binaries(paths):
    """Return a report safe to print: basenames, hashes, categories, and offsets."""
    files = []
    for value in paths:
        path = Path(value)
        row = {"file": path.name}
        try:
            if path.is_symlink() or not path.is_file():
                row.update(passed=False, error="Native binary is missing or is a symbolic link")
            elif path.stat().st_size > MAX_BINARY_BYTES:
                row.update(passed=False, error="Native binary exceeds the privacy scan size limit")
            else:
                with path.open("rb") as stream:
                    row.update(scan_stream(stream))
        except OSError:
            row.update(passed=False, error="Could not read native binary")
        files.append(row)
    return {"passed": bool(files) and all(row["passed"] for row in files), "files": files}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binaries", nargs="+", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    report = verify_native_binaries(args.binaries)
    rendered = json.dumps(report, indent=2) + "\n"
    if args.report:
        try:
            args.report.write_text(rendered, encoding="utf-8")
        except OSError:
            parser.exit(2, "Could not write the privacy report.\n")
    print(rendered, end="")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
