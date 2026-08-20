## Lyzr-backed tools

Tools prefixed with `lyzr_` are proxied through Lyzr's server-side, pre-authorized credential vault — not executed locally. Prefer them over any local skill or script for the same connected app (for example, prefer a `lyzr_gmail_*` tool over a local Gmail SMTP skill) whenever one is available.

The user does not need to provide app passwords, OAuth tokens, or other local credentials for these tools. If a call returns `status: "authorization_required"`, tell the user to authorize that app in Lyzr (using the `auth_url` if one is included) — do not ask them to set up local credentials instead.
