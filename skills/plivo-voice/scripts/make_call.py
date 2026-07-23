#!/usr/bin/env python3
"""
Make an outbound voice call via the Plivo Call API
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

def make_call(from_number, to, answer_url, answer_method='GET', auth_id=None, auth_token=None):
    """Place an outbound call via the Plivo Call API"""

    # Get credentials
    auth_id = auth_id or os.getenv('PLIVO_AUTH_ID')
    auth_token = auth_token or os.getenv('PLIVO_AUTH_TOKEN')

    if not auth_id or not auth_token:
        print("ERROR: Plivo credentials not found!", file=sys.stderr)
        print("\nPlease set credentials using one of these methods:", file=sys.stderr)
        print("\n1. Environment variables:", file=sys.stderr)
        print("   export PLIVO_AUTH_ID='your-auth-id'", file=sys.stderr)
        print("   export PLIVO_AUTH_TOKEN='your-auth-token'", file=sys.stderr)
        print("\n2. Create a .env file in skills/plivo-voice/:", file=sys.stderr)
        print("   PLIVO_AUTH_ID=your-auth-id", file=sys.stderr)
        print("   PLIVO_AUTH_TOKEN=your-auth-token", file=sys.stderr)
        print("\nFind both in the Plivo console at https://cx.plivo.com", file=sys.stderr)
        sys.exit(1)

    # Build request
    url = f"https://api.plivo.com/v1/Account/{auth_id}/Call/"
    payload = json.dumps({
        "from": from_number,
        "to": to,
        "answer_url": answer_url,
        "answer_method": answer_method,
    }).encode()
    credentials = base64.b64encode(f"{auth_id}:{auth_token}".encode()).decode()

    request = urllib.request.Request(url, data=payload, method='POST')
    request.add_header('Authorization', f"Basic {credentials}")
    request.add_header('Content-Type', 'application/json')

    # Place call
    try:
        print(f"Placing call to {to}...", file=sys.stderr)
        with urllib.request.urlopen(request) as response:
            body = json.loads(response.read().decode())

        request_uuid = body.get('request_uuid')
        if not request_uuid:
            print(f"ERROR: Plivo accepted the request but returned no request_uuid: {body}", file=sys.stderr)
            return False
        print(f"✓ Call placed successfully (request_uuid: {request_uuid})")
        return True

    except urllib.error.HTTPError as e:
        error_body = e.read().decode()
        print(f"ERROR: Plivo returned HTTP {e.code}", file=sys.stderr)
        print(error_body, file=sys.stderr)
        return False
    except Exception as e:
        print(f"ERROR: Failed to place call: {e}", file=sys.stderr)
        return False

def main():
    parser = argparse.ArgumentParser(description='Make an outbound voice call via the Plivo Call API')
    parser.add_argument('--to', required=True, help='Recipient number in E.164')
    parser.add_argument('--from', dest='from_number', required=True, help='Caller ID (a Plivo number)')
    parser.add_argument('--answer-url', dest='answer_url', required=True,
                        help='URL returning Plivo XML for the call (e.g. a <Speak> response)')
    parser.add_argument('--answer-method', dest='answer_method', default='GET',
                        help='HTTP method Plivo uses to fetch the answer URL (default: GET)')
    parser.add_argument('--auth-id', dest='auth_id', help='Plivo Auth ID (default: PLIVO_AUTH_ID env var)')
    parser.add_argument('--auth-token', dest='auth_token', help='Plivo Auth Token (default: PLIVO_AUTH_TOKEN env var)')

    args = parser.parse_args()

    # Load .env file if exists
    load_env()

    # Place call
    success = make_call(
        from_number=args.from_number,
        to=args.to,
        answer_url=args.answer_url,
        answer_method=args.answer_method,
        auth_id=args.auth_id,
        auth_token=args.auth_token
    )

    sys.exit(0 if success else 1)

if __name__ == '__main__':
    main()
