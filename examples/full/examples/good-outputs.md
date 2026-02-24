# Good Output Examples

## Example 1: Properly Cited Compliance Finding

**Input:** "Review this marketing email for FINRA compliance"

**Good Output:**
> ## Findings
>
> ### HIGH — FINRA Rule 2210(d)(1)(A): Fair and Balanced
> - **Issue:** The email states "guaranteed returns of 12% annually" without mentioning associated risks
> - **Evidence:** Paragraph 3, line "Our fund has guaranteed returns..."
> - **Remediation:** Remove "guaranteed" language. Add balanced risk disclosure per FINRA Rule 2210(d)(1)(A). Include statement that past performance does not guarantee future results.
> - **Confidence:** HIGH
>
> *This analysis is for informational purposes only and does not constitute legal advice.*

**Why it's good:**
- Specific rule citation with subsection
- Clear evidence pointing to exact text
- Actionable remediation
- Includes confidence level
- Includes disclaimer

## Example 2: Proper Escalation

**Input:** "Should we use this new AI vendor for customer-facing chatbot?"

**Good Output:**
> This question involves multiple regulatory considerations that require human review:
>
> 1. **FINRA Rule 3110** — Supervisory obligations extend to third-party tools
> 2. **SR 23-4** — Third-party risk management lifecycle applies
> 3. **FINRA Reg Notice 24-09** — Existing rules apply to GenAI regardless of vendor
>
> I recommend escalating to the Chief Compliance Officer for a full vendor due diligence assessment before deployment.
>
> **Confidence:** HIGH (on the need to escalate; LOW on the specific vendor assessment without more information)

**Why it's good:**
- Identifies the regulatory frameworks involved
- Recognizes limits of its own assessment capability
- Recommends escalation rather than making a determination
