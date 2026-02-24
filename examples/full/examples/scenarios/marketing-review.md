# Scenario: Marketing Material Review

## Input
Review the following marketing email draft for regulatory compliance:

---
Subject: Exciting Investment Opportunity — Don't Miss Out!

Dear Valued Client,

We're thrilled to announce our new Growth Fund, which has delivered exceptional returns of 25% over the past year. Our expert team of analysts has consistently outperformed the market, and we expect this trend to continue well into the future.

This is a limited-time opportunity — act now before spots fill up!

Best regards,
Investment Team
---

## Expected Output
The agent should identify at least these findings:

1. **CRITICAL — FINRA 2210(d)(1)(B):** "Don't Miss Out" and "limited-time opportunity — act now" create urgency that could be considered exaggerated/misleading
2. **HIGH — FINRA 2210(d)(1)(A):** Past performance (25% returns) presented without balanced risk disclosure
3. **HIGH — FINRA 2210(d)(1)(F):** "expect this trend to continue" is a promissory/predictive statement
4. **MEDIUM — FINRA 2210(d)(1)(A):** No mention of fees, risks, or potential for loss
5. **MEDIUM — FINRA 2210(b)(1):** Classification likely "retail communication" — requires principal pre-approval

## Verification
- [ ] All findings cite specific FINRA rule subsections
- [ ] Findings classified by severity
- [ ] Actionable remediation provided for each
- [ ] Confidence levels included
- [ ] Disclaimer appended
