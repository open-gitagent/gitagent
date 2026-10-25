use super::*;

fn write(root: &Path, files: &[(&str, &str)]) {
    for (name, text) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn messages(r: &Report) -> Vec<String> {
    r.findings.iter().map(|f| f.to_string()).collect()
}

const GOOD: &[(&str, &str)] = &[
    (
        "agent.yaml",
        "spec_version: \"0.1.0\"\nname: helper\nversion: 1.2.0-beta.1\ndescription: Helps.\nmodel:\n  preferred: claude-opus-5-5\n  constraints: {temperature: 0.2, max_tokens: 4000}\nskills: [summarize]\ntools: [lookup]\nagents:\n  checker: {description: Checks.}\nmcp_servers:\n  docs: {url: \"https://docs.example.com/mcp\"}\nmetadata: {team: support, tier: 2}\n",
    ),
    ("SOUL.md", "# Identity\nI help.\n"),
    (
        "skills/summarize/SKILL.md",
        "---\nname: summarize\ndescription: Summarizes text.\nmetadata: {category: writing}\n---\nSteps.\n",
    ),
    (
        "tools/lookup.yaml",
        "name: lookup\ndescription: Looks up.\ninput_schema: {type: object, properties: {q: {type: string}}}\nimplementation: {type: script, path: lookup.sh, runtime: bash}\n",
    ),
    ("tools/lookup.sh", "echo ok\n"),
    ("agents/checker.md", "---\nname: checker\n---\nCheck.\n"),
    ("hooks/log.sh", "true\n"),
    (
        "hooks/hooks.yaml",
        "hooks:\n  pre_tool_use:\n    - {script: log.sh, description: Logs, timeout: 5, compliance: true}\n",
    ),
];

#[test]
fn a_well_formed_agent_passes() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), GOOD);
    let r = validate(dir.path(), false);
    assert!(r.findings.is_empty(), "{:?}", messages(&r));
}

#[test]
fn problems_are_reported_with_file_and_line() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), GOOD);
    write(
        dir.path(),
        &[
            (
                "agent.yaml",
                "name: Helper\nversion: \"1.0\"\ndescription: Helps.\nruntime:\n  max_turns: 0\n  temperature: 3\nskills: [summarize, missing]\ntools: [lookup, lookup]\nagents:\n  ghost: {description: Not there.}\nmcp_servers:\n  both: {command: x, url: \"https://x\"}\nplugins: {x: 1}\nfavorite_color: blue\n",
            ),
            ("SOUL.md", "# Only a heading\n"),
            (
                "hooks/hooks.yaml",
                "hooks:\n  pre_query:\n    - {script: log.sh, description: Q}\n  pre_tool_use:\n    - {script: gone.sh}\n  on_lunch: []\n",
            ),
            ("tools/odd.yaml", "name: Odd_Tool\ndescription: x\n"),
            ("skills/bad/SKILL.md", "---\nname: bad--name\ndescription: x\nversion: 1\n---\nx\n"),
            ("skills/nofront/SKILL.md", "Just text.\n"),
        ],
    );
    let r = validate(dir.path(), false);
    let got = messages(&r);
    for expected in [
        "error: agent.yaml:1: name: must be lowercase letters, digits and dashes, starting with a letter (got \"Helper\")",
        "error: agent.yaml:2: version: must be a semantic version like 1.0.0 (got \"1.0\")",
        "error: agent.yaml:5: runtime.max_turns: must be at least 1 (got 0)",
        "error: agent.yaml:6: runtime.temperature: must be between 0 and 2 (got 3)",
        "error: agent.yaml:7: skills: skills/missing/ does not exist",
        "error: agent.yaml:8: tools: lists a name more than once",
        "error: agent.yaml:10: agents.ghost: neither agents/ghost/ nor agents/ghost.md exists",
        "error: agent.yaml:12: mcp_servers.both: give either `command` or `url`, not both",
        "error: agent.yaml:14: favorite_color: is not a known setting",
        "warning: agent.yaml:13: plugins: is a gitagent 1.x setting, not part of the OpenGAP format",
        "error: SOUL.md: has headings but no text",
        "warning: hooks/hooks.yaml:4: hooks.pre_tool_use.0: has no `description` (OpenGAP asks for one)",
        "error: hooks/hooks.yaml:5: hooks.pre_tool_use.0.script: hooks/gone.sh does not exist",
        "error: hooks/hooks.yaml:6: hooks.on_lunch: is not a known event",
        "warning: hooks/hooks.yaml:2: hooks.pre_query: is a gitagent extension, not one of the OpenGAP events",
        "warning: tools/odd.yaml:1: name: must be lowercase letters, digits and dashes (got \"Odd_Tool\")",
        "warning: tools/odd.yaml: `input_schema` is required",
        "error: skills/bad/SKILL.md:2: name: must be lowercase words joined by single dashes, at most 64 characters (got \"bad--name\")",
        "error: skills/bad/SKILL.md:4: version: is not a known setting (extensions go under `metadata`)",
        "error: skills/nofront/SKILL.md: has no YAML frontmatter (--- name: … description: … ---)",
    ] {
        assert!(got.iter().any(|g| g == expected), "missing {expected:?} in\n{}", got.join("\n"));
    }
    assert!(r.errors() >= 15);
    // Errors come before warnings.
    let first_warning = r
        .findings
        .iter()
        .position(|f| f.severity == Severity::Warning)
        .unwrap();
    assert!(r.findings[..first_warning]
        .iter()
        .all(|f| f.severity == Severity::Error));
}

#[test]
fn the_compliance_rules() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), GOOD);
    let r = validate(dir.path(), true);
    assert_eq!(
        messages(&r),
        vec!["warning: agent.yaml: there is no `compliance` section"]
    );

    let mut manifest = GOOD[0].1.to_string();
    manifest.push_str(
        "compliance:\n  risk_tier: high\n  frameworks: [finra, federal_reserve]\n  supervision:\n    human_in_the_loop: advisory\n  communications: {fair_balanced: true}\n  model_risk: {validation_cadence: annual}\n  financial_governance:\n    enabled: true\n    firewall: https://fw.example.com\n    spending: {max_per_transaction_cents: 0}\n  segregation_of_duties:\n    roles:\n      - {id: maker, description: Makes}\n      - {id: checker, description: Checks}\n    conflicts: [[maker, checker], [maker, auditor]]\n    assignments:\n      helper: [maker, checker]\n    handoffs:\n      - {action: payment, required_roles: [maker, maker]}\n    enforcement: strict\n",
    );
    write(dir.path(), &[("agent.yaml", &manifest)]);
    let got = messages(&validate(dir.path(), true));
    for expected in [
        "[FINRA 3110] a high or critical risk tier needs supervision.human_in_the_loop always or conditional",
        "[FINRA 4511] a high or critical risk tier needs recordkeeping.audit_logging: true",
        "[SR 11-7] a high or critical risk tier must validate quarterly or more often",
        "[FINRA 2210] finra needs communications.no_misleading: true",
        "[SR 11-7] federal_reserve needs model_risk.ongoing_monitoring: true",
        "compliance.financial_governance.spending.max_per_transaction_cents: must be at least 1",
        "compliance.financial_governance.firewall: must name the firewall, not give its endpoint URL",
        "compliance.segregation_of_duties.conflicts: \"auditor\" is not a defined role",
        "compliance.segregation_of_duties.assignments: \"helper\" holds both maker and checker, which conflict",
        "compliance.segregation_of_duties.handoffs: the handoff \"payment\" needs at least two different roles",
        "compliance.segregation_of_duties: isolation.state is not full at a high or critical risk tier",
        "compliance.risk_tier: compliance/risk-assessment.md is missing",
    ] {
        assert!(got.iter().any(|g| g.ends_with(expected)), "missing {expected:?} in\n{}", got.join("\n"));
    }
}

#[test]
fn sub_agent_directories_are_checked_too() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), GOOD);
    write(
        dir.path(),
        &[
            (
                "agents/helper/agent.yaml",
                "name: helper\nversion: one\ndescription: Helper.\n",
            ),
            ("agents/helper/SOUL.md", "# Heading only\n"),
        ],
    );
    let got = messages(&validate(dir.path(), false));
    for expected in [
        "error: agents/helper/agent.yaml:2: version: must be a semantic version like 1.0.0 (got \"one\")",
        "error: agents/helper/SOUL.md: has headings but no text",
    ] {
        assert!(got.iter().any(|g| g == expected), "missing {expected:?} in\n{}", got.join("\n"));
    }
}

#[test]
fn workflows_are_checked_for_what_they_use() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), GOOD);
    write(
        dir.path(),
        &[
            (
                "workflows/review.yaml",
                "name: review\ndescription: d\nsteps:\n  - {id: a, action: x, skill: summarize}\n  - {id: b, action: y, skill: nope, tool: lookup, agent: checker}\n  - {id: c, action: z, tool: gone, agent: ghost}\n",
            ),
            ("workflows/broken.yaml", "name: broken\nsteps: []\n"),
        ],
    );
    let got = messages(&validate(dir.path(), false));
    for expected in [
        "warning: workflows/review.yaml: step \"b\" uses the skill \"nope\", which the agent does not have",
        "warning: workflows/review.yaml: step \"c\" uses the tool \"gone\", which the agent does not have",
        "warning: workflows/review.yaml: step \"c\" uses the sub-agent \"ghost\", which the agent does not have",
        "warning: workflows/broken.yaml: `steps` is empty",
    ] {
        assert!(got.iter().any(|g| g == expected), "missing {expected:?} in\n{}", got.join("\n"));
    }
    assert_eq!(got.len(), 4, "{got:?}");
}
