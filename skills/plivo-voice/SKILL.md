---
name: plivo-voice
description: Place outbound voice calls via the Plivo Call API using Auth ID/Auth Token authentication.
---

# Plivo Voice Skill

Place outbound voice calls via Plivo.

## Setup

1. **Create a Plivo account** and open the console at https://cx.plivo.com
2. **Get your credentials**:
   - Copy your **Auth ID** and **Auth Token** from the dashboard
   - Rent a Plivo phone number to call from

3. **Configure credentials**:
   ```bash
   export PLIVO_AUTH_ID="your-auth-id"
   export PLIVO_AUTH_TOKEN="your-auth-token"
   ```

   Or create a `.env` file in the skill directory:
   ```
   PLIVO_AUTH_ID=your-auth-id
   PLIVO_AUTH_TOKEN=your-auth-token
   ```

## Usage

```bash
python3 scripts/make_call.py \
  --from "+14150000002" \
  --to "+14150000001" \
  --answer-url "https://example.com/answer.xml"
```

When the call is answered, Plivo fetches the answer URL and runs the Plivo XML it returns. To speak a message, have that URL return XML such as:

```xml
<Response>
  <Speak>Your task has finished.</Speak>
</Response>
```

## Requirements

- Python 3.6+
- No additional packages needed (uses stdlib urllib)
