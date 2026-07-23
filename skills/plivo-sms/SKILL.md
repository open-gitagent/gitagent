---
name: plivo-sms
description: Send SMS messages via the Plivo Messages API using Auth ID/Auth Token authentication.
---

# Plivo SMS Skill

Send SMS messages via Plivo.

## Setup

1. **Create a Plivo account** and open the console at https://cx.plivo.com
2. **Get your credentials**:
   - Copy your **Auth ID** and **Auth Token** from the dashboard
   - Rent a Plivo phone number (or use an approved sender ID) to send from

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
python3 scripts/send_sms.py \
  --from "+14150000002" \
  --to "+14150000001" \
  --text "Message body text"
```

Send to multiple recipients by joining numbers with `<`:

```bash
python3 scripts/send_sms.py \
  --from "+14150000002" \
  --to "+14150000001<+14160000003" \
  --text "Message body text"
```

## Requirements

- Python 3.6+
- No additional packages needed (uses stdlib urllib)
