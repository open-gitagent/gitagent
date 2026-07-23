#!/usr/bin/env python3
"""
Send SMS via Plivo Messages API
"""
import argparse
import base64
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

def load_env():
    """Load environment variables from .env file if it exists"""
    env_file = Path(__file__).parent.parent / '.env'
    if env_file.exists():
        with open(env_file) as f:
            for line in f:
                line = line.strip()
                if not line or line.startswith('#') or '=' not in line:
                    continue
                key, value = line.split('=', 1)
                value = value.strip()
                if len(value) >= 2 and value[0] == value[-1] and value[0] in ('"', "'"):
                    value = value[1:-1]
                os.environ[key.strip()] = value

def send_sms(from_number, to, text, auth_id=None, auth_token=None):
    """Send SMS via Plivo Messages API"""

    # Get credentials
    auth_id = auth_id or os.getenv('PLIVO_AUTH_ID')
    auth_token = auth_token or os.getenv('PLIVO_AUTH_TOKEN')

    if not auth_id or not auth_token:
        print("ERROR: Plivo credentials not found!", file=sys.stderr)
        print("\nPlease set credentials using one of these methods:", file=sys.stderr)
        print("\n1. Environment variables:", file=sys.stderr)
        print("   export PLIVO_AUTH_ID='your-auth-id'", file=sys.stderr)
        print("   export PLIVO_AUTH_TOKEN='your-auth-token'", file=sys.stderr)
        print("\n2. Create a .env file in skills/plivo-sms/:", file=sys.stderr)
        print("   PLIVO_AUTH_ID=your-auth-id", file=sys.stderr)
        print("   PLIVO_AUTH_TOKEN=your-auth-token", file=sys.stderr)
        print("\nFind both in the Plivo console at https://cx.plivo.com", file=sys.stderr)
        sys.exit(1)

    # Build request
    url = f"https://api.plivo.com/v1/Account/{auth_id}/Message/"
    payload = json.dumps({"src": from_number, "dst": to, "text": text, "type": "sms"}).encode()
    credentials = base64.b64encode(f"{auth_id}:{auth_token}".encode()).decode()

    request = urllib.request.Request(url, data=payload, method='POST')
    request.add_header('Authorization', f"Basic {credentials}")
    request.add_header('Content-Type', 'application/json')

    # Send SMS
    try:
        print(f"Sending SMS to {to}...", file=sys.stderr)
        with urllib.request.urlopen(request) as response:
            body = json.loads(response.read().decode())

        uuids = body.get('message_uuid', [])
        if not uuids:
            print(f"ERROR: Plivo accepted the request but returned no message_uuid: {body}", file=sys.stderr)
            return False
        print(f"✓ SMS queued successfully (message_uuid: {', '.join(uuids)})")
        return True

    except urllib.error.HTTPError as e:
        error_body = e.read().decode()
        print(f"ERROR: Plivo returned HTTP {e.code}", file=sys.stderr)
        print(error_body, file=sys.stderr)
        return False
    except Exception as e:
        print(f"ERROR: Failed to send SMS: {e}", file=sys.stderr)
        return False

def main():
    parser = argparse.ArgumentParser(description='Send SMS via Plivo Messages API')
    parser.add_argument('--to', required=True, help='Recipient number in E.164 (join multiple with "<")')
    parser.add_argument('--from', dest='from_number', required=True, help='Sender number or sender ID')
    parser.add_argument('--text', required=True, help='Message body')
    parser.add_argument('--auth-id', dest='auth_id', help='Plivo Auth ID (default: PLIVO_AUTH_ID env var)')
    parser.add_argument('--auth-token', dest='auth_token', help='Plivo Auth Token (default: PLIVO_AUTH_TOKEN env var)')

    args = parser.parse_args()

    # Load .env file if exists
    load_env()

    # Send SMS
    success = send_sms(
        from_number=args.from_number,
        to=args.to,
        text=args.text,
        auth_id=args.auth_id,
        auth_token=args.auth_token
    )

    sys.exit(0 if success else 1)

if __name__ == '__main__':
    main()
