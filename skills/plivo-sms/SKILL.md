---
name: plivo-sms
description: Send outbound SMS messages through the Plivo Messages API.
---

# Plivo SMS Skill

Send an outbound SMS using Plivo's REST API. The script uses only Python's
standard library and never prints the auth token.

## Setup

Set these environment variables (or put them in `skills/plivo-sms/.env`):

```bash
export PLIVO_AUTH_ID="your-auth-id"
export PLIVO_AUTH_TOKEN="your-auth-token"
export PLIVO_SMS_SOURCE="+14155551234"
```

The source number must be a Plivo number, short code, Powerpack sender, or
approved alphanumeric sender ID for the account.

## Usage

```bash
python3 scripts/send_sms.py \
  --to "+14155559876" \
  --text "The agent finished the task."
```

Use `--from` to override `PLIVO_SMS_SOURCE` for one message. Phone numbers
should use E.164 format. Multiple recipients can be supplied as a comma-
separated list; the script sends them in one Plivo request.

## Requirements

- Python 3.8+
- No additional packages
