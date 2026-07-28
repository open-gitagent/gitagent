---
name: plivo-voice
description: Place an outbound voice call through the Plivo Voice API.
---

# Plivo Voice Skill

Place an outbound call using Plivo's REST API. Plivo fetches the supplied
`answer_url` when the call is answered; that URL must return valid Plivo XML,
for example `<Response><Speak>Hello from GitAgent.</Speak></Response>`.

## Setup

Set these environment variables (or put them in `skills/plivo-voice/.env`):

```bash
export PLIVO_AUTH_ID="your-auth-id"
export PLIVO_AUTH_TOKEN="your-auth-token"
export PLIVO_VOICE_SOURCE="+14155551234"
```

## Usage

```bash
python3 scripts/make_call.py \
  --to "+14155559876" \
  --answer-url "https://example.com/plivo/answer.xml"
```

Use `--from` to override `PLIVO_VOICE_SOURCE` and `--answer-method GET` when
the answer endpoint expects GET instead of the default POST. The answer URL
must be publicly reachable by Plivo; the skill intentionally does not host a
temporary endpoint or expose credentials in the URL.

## Requirements

- Python 3.8+
- No additional packages
