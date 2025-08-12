"""Common utils"""

import gzip
import mimetypes
import os
import re
import secrets

from flask import Response, abort, request, send_file

from utils.config import BLACKLISTED_FILES

_RANGE_RE = re.compile(r"bytes=(.+)", re.IGNORECASE)


def check(file_path, blacklisted_files=None):
    """Check if the file path or any parent directory is blacklisted."""
    if blacklisted_files is None:
        blacklisted_files = BLACKLISTED_FILES

    current_directory = os.getcwd()
    if os.path.basename(file_path) in blacklisted_files:
        return True

    for blacklisted in blacklisted_files:
        blacklisted_path = os.path.join(current_directory, blacklisted)
        if os.path.commonpath([file_path, blacklisted_path]) == blacklisted_path:
            if os.path.abspath(file_path).startswith(os.path.abspath(blacklisted_path)):
                return True

    return False


def allowed_file(filename):
    """List allowed uploaded files."""
    return "." in filename and filename.rsplit(".", 1)[1].lower() in ALLOWED_EXTENSIONS


def format_size(size_bytes):
    """Format size in a human-readable way."""
    if size_bytes == 0:
        return "0 B"

    size_units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"]
    i = 0
    while size_bytes >= 1024 and i < len(size_units) - 1:
        size_bytes /= 1024.0
        i += 1

    return f"{size_bytes:.2f} {size_units[i]}"


def gzip_response(app, data):
    """Sends a gzip-compressed response."""
    compressed_content = gzip.compress(data)
    response = app.response_class(
        compressed_content, content_type="text/html; charset=utf-8"
    )
    response.headers["Content-Encoding"] = "gzip"
    response.headers["Content-Length"] = str(len(compressed_content))
    return response


def _parse_ranges(header_value: str, file_size: int):
    """
    Parse 'Range' header into list of (start, end) (inclusive) tuples.
    Supports multiple ranges and suffix ranges (e.g. '-500').
    Returns a list sorted and coalesced; raises ValueError on bad syntax.
    """
    m = _RANGE_RE.match(header_value.strip())
    if not m:
        raise ValueError("Bad Range unit")

    parts = m.group(1).split(",")
    ranges = []
    for part in parts:
        part = part.strip()
        if not part:
            continue
        if "-" not in part:
            raise ValueError("Missing '-'")

        start_s, end_s = part.split("-", 1)
        if start_s == "":
            try:
                length = int(end_s)
            except ValueError:
                raise ValueError("Bad suffix length")
            if length <= 0:
                continue
            if length > file_size:
                start, end = 0, file_size - 1
            else:
                start, end = file_size - length, file_size - 1
        else:
            try:
                start = int(start_s)
            except ValueError:
                raise ValueError("Bad start")
            if end_s == "":
                end = file_size - 1
            else:
                try:
                    end = int(end_s)
                except ValueError:
                    raise ValueError("Bad end")

            if start > end:
                continue
            if start >= file_size:
                continue
            end = min(end, file_size - 1)

        ranges.append((start, end))

    if not ranges:
        return []

    ranges.sort()
    merged = []
    cs, ce = ranges[0]
    for s, e in ranges[1:]:
        if s <= ce + 1:
            ce = max(ce, e)
        else:
            merged.append((cs, ce))
            cs, ce = s, e
    merged.append((cs, ce))

    return merged


def cust_send_file(full_path: str):
    """Send file with HTTP Range (single & multi-range) support."""
    file_size = os.path.getsize(full_path)
    mime_type = mimetypes.guess_type(full_path)[0] or "application/octet-stream"

    range_header = request.headers.get("Range")
    if not range_header:
        return send_file(full_path, as_attachment=True)

    try:
        ranges = _parse_ranges(range_header, file_size)
    except ValueError:
        resp = Response(status=416)
        resp.headers["Content-Range"] = f"bytes */{file_size}"
        return resp

    if not ranges:
        resp = Response(status=416)
        resp.headers["Content-Range"] = f"bytes */{file_size}"
        return resp

    if len(ranges) == 1:
        start, end = ranges[0]
        length = end - start + 1

        def generate_one():
            with open(full_path, "rb") as f:
                f.seek(start)
                remaining = length
                chunk = 64 * 1024
                while remaining > 0:
                    data = f.read(min(chunk, remaining))
                    if not data:
                        break
                    yield data
                    remaining -= len(data)

        resp = Response(generate_one(), status=206, mimetype=mime_type)
        resp.headers["Content-Range"] = f"bytes {start}-{end}/{file_size}"
        resp.headers["Accept-Ranges"] = "bytes"
        resp.headers["Content-Length"] = str(length)
        return resp

    boundary = f"range_{secrets.token_hex(8)}"
    multipart_type = f"multipart/byteranges; boundary={boundary}"

    def generate_multi():
        with open(full_path, "rb") as f:
            for start, end in ranges:
                yield (
                    f"--{boundary}\r\n"
                    f"Content-Type: {mime_type}\r\n"
                    f"Content-Range: bytes {start}-{end}/{file_size}\r\n"
                    f"\r\n"
                ).encode("ascii")

                remaining = end - start + 1
                f.seek(start)
                chunk = 64 * 1024
                while remaining > 0:
                    data = f.read(min(chunk, remaining))
                    if not data:
                        break
                    yield data
                    remaining -= len(data)
                yield b"\r\n"

            yield f"--{boundary}--\r\n".encode("ascii")

    resp = Response(generate_multi(), status=206, mimetype=multipart_type)
    resp.headers["Accept-Ranges"] = "bytes"
    return resp
