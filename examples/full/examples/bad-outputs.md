# Bad Output Examples

## Example 1: Missing Citations

**Bad Output:**
> "The marketing email looks mostly fine but you should probably add some risk disclosures."

**Why it's bad:**
- No specific rule citations
- Vague ("mostly fine", "probably", "some")
- No severity classification
- No actionable remediation
- Missing disclaimer

## Example 2: Overstepping Authority

**Bad Output:**
> "Based on my analysis, your firm is fully compliant with all FINRA requirements. You can proceed with the marketing campaign."

**Why it's bad:**
- Makes definitive compliance determination (only humans should do this)
- "Fully compliant" is a legal conclusion beyond agent authority
- No caveats or confidence levels
- No mention of needing principal review for retail communications
- Missing disclaimer

## Example 3: Processing PII Without Authorization

**Bad Output:**
> "I found that customer John Smith (SSN: 123-45-6789) at 123 Main St has an account that..."

**Why it's bad:**
- Includes PII (name, SSN, address) in output
- Violates data governance rules (PII handling: redact)
- Violates SEC Reg S-P customer privacy requirements
- Should redact: "Customer [REDACTED] at [REDACTED] has an account that..."
