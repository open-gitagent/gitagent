#!/usr/bin/env python3
"""Send an SMS through the Plivo Messages API using only the standard library."""

import argparse
import base64
import json
import os
import sys
from pathlib import Path
from typing import Optional
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


API_BASE = "https://api.plivo.com/v1/Account"


def load_env() -> None:
    """Load simple KEY=VALUE entries from the skill-local .env file."""
    env_file = Path(__file__).resolve().parent.parent / ".env"
    if not env_file.exists():
        return
    for raw_line in env_file.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        os.environ.setdefault(key.strip(), value.strip().strip('"\''))


def send_sms(
    destination: str,
    text: str,
    source: Optional[str] = None,
    *,
    auth_id: Optional[str] = None,
    auth_token: Optional[str] = None,
    timeout: float = 30,
) -> dict:
    """Send one request to Plivo and return its decoded JSON response."""
    auth_id = auth_id or os.getenv("PLIVO_AUTH_ID")
    auth_token = auth_token or os.getenv("PLIVO_AUTH_TOKEN")
    source = source or os.getenv("PLIVO_SMS_SOURCE")
    missing = [
        name
        for name, value in (
            ("PLIVO_AUTH_ID", auth_id),
            ("PLIVO_AUTH_TOKEN", auth_token),
            ("PLIVO_SMS_SOURCE", source),
        )
        if not value
    ]
    if missing:
        raise ValueError(f"Missing required configuration: {', '.join(missing)}")
    if not destination.strip():
        raise ValueError("destination must not be empty")
    if not text:
        raise ValueError("text must not be empty")

    credentials = base64.b64encode(f"{auth_id}:{auth_token}".encode()).decode()
    body = json.dumps({"src": source, "dst": destination, "text": text}).encode()
    request = Request(
        f"{API_BASE}/{auth_id}/Message/",
        data=body,
        headers={
            "Authorization": f"Basic {credentials}",
            "Content-Type": "application/json",
            "Accept": "application/json",
        },
        method="POST",
    )
    try:
        with urlopen(request, timeout=timeout) as response:
            return json.loads(response.read().decode("utf-8"))
    except HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"Plivo rejected the SMS ({error.code}): {detail}") from error
    except URLError as error:
        raise RuntimeError(f"Could not reach Plivo: {error.reason}") from error


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--to", required=True, dest="destination")
    parser.add_argument("--text", required=True)
    parser.add_argument("--from", dest="source", help="Override PLIVO_SMS_SOURCE")
    args = parser.parse_args()
    load_env()
    try:
        result = send_sms(args.destination, args.text, args.source)
    except (RuntimeError, ValueError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
