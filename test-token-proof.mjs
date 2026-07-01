/**
 * Token optimization proof for meeting demo.
 *
 * The real cost problem: when an agent reads a large doc in session,
 * that doc content stays in context for EVERY subsequent turn.
 *
 * WITHOUT compression: 27,000 tokens sent to API on turn 1, 2, 3... every turn.
 * WITH CCR:            305 tokens (outline) on turn 1, ~2,000 per fetch.
 *
 * Usage:
 *   LYZR_API_KEY=sk-... LYZR_AGENT_ID=... node test-token-proof.mjs <large-pdf>
 *   (use Lyzr-Employee-Handbook.pdf — it's 27K tokens, triggers CCR)
 */

import { readFile } from "fs/promises";
import { resolve, basename } from "path";

const PDF_PATH = process.argv[2];
if (!PDF_PATH) {
  console.error("Usage: node test-token-proof.mjs <path-to-pdf>");
  process.exit(1);
}

const LYZR_API_KEY = process.env.LYZR_API_KEY;
const LYZR_AGENT_ID = process.env.LYZR_AGENT_ID;
if (!LYZR_API_KEY || !LYZR_AGENT_ID) {
  console.error("Set LYZR_API_KEY and LYZR_AGENT_ID");
  process.exit(1);
}
process.env.OPENAI_API_KEY = LYZR_API_KEY;

function tok(s) { return Math.ceil(s.length / 4); }
function sep(t) { console.log("\n" + "═".repeat(54) + "\n " + t + "\n" + "═".repeat(54)); }

// ── Step 1: Show what the compression pipeline does offline ───────────
async function analyzeDoc(absPath) {
  const { convertToMarkdown, isConvertible } = await import("./dist/tools/doc-converter.js");
  const { chunkDocument } = await import("./dist/tools/doc-store.js");

  const buf = await readFile(absPath);
  const rawSizeKB = (buf.length / 1024).toFixed(1);

  if (!isConvertible(absPath)) {
    console.log("Not a convertible format"); process.exit(1);
  }

  const result = await convertToMarkdown(absPath, buf);
  if (!result) { console.log("Conversion failed"); process.exit(1); }

  const fullTokens = tok(result.markdown);
  const chunks     = chunkDocument(result.markdown);
  const outline    = chunks.map(c => `[${c.id}] ${c.title} (~${c.estimatedTokens} tok)`).join("\n");
  const outlineTok = tok(outline);
  const CCR        = fullTokens > 8000;

  return { buf, rawSizeKB, fullTokens, chunks, outline, outlineTok, CCR, savedTokens: result.savedTokens };
}

// ── Step 2: Multi-turn simulation ─────────────────────────────────────
// Shows token cost per turn with and without CCR
function multiTurnSimulation(fullTokens, outlineTok, chunks, turns = 5) {
  const AVG_SECTION_TOKENS = Math.round(
    chunks.reduce((s, c) => s + c.estimatedTokens, 0) / chunks.length
  );
  const BASE_SYSTEM = 910;       // system prompt tokens
  const BASE_CONV   = 200;       // avg conversation overhead per turn
  const ANSWER_TOKENS = 400;     // avg agent output per turn

  // WITHOUT compression: full doc dumped into context every turn
  // (assuming client pre-processes PDF to text and puts it in knowledge base)
  const withoutPerTurn = [];
  let withoutCtx = BASE_SYSTEM + fullTokens;
  for (let i = 1; i <= turns; i++) {
    withoutCtx += BASE_CONV;
    withoutPerTurn.push({ turn: i, input: withoutCtx, output: ANSWER_TOKENS });
    withoutCtx += ANSWER_TOKENS; // accumulates in history
  }

  // WITH compression: outline on first read, then 1-2 sections per question
  const withPerTurn = [];
  let withCtx = BASE_SYSTEM;
  for (let i = 1; i <= turns; i++) {
    withCtx += BASE_CONV;
    if (i === 1) withCtx += outlineTok;           // first turn: read outline
    else withCtx += AVG_SECTION_TOKENS;            // subsequent: fetch 1 section
    withPerTurn.push({ turn: i, input: withCtx, output: ANSWER_TOKENS });
    withCtx += ANSWER_TOKENS;
  }

  return { withoutPerTurn, withPerTurn, AVG_SECTION_TOKENS };
}

async function runAgentRound(label, dir, prompt, model) {
  const { query } = await import("./dist/exports.js");
  sep(label);

  const result = query({ prompt, dir, model, maxTurns: 8, constraints: { maxTokens: 800 } });

  for await (const msg of result) {
    switch (msg.type) {
      case "system":  console.log(`[${msg.subtype}] ${msg.content.slice(0, 100)}`); break;
      case "delta":   process.stdout.write(msg.content); break;
      case "assistant": console.log(); break;
      case "tool_use":
        console.log(`\n  → ${msg.toolName}(${JSON.stringify(msg.args).slice(0, 80)})`);
        break;
      case "tool_result":
        const p = typeof msg.content === "string" ? msg.content.slice(0, 150) : "";
        console.log(`    ✓ ${p}`);
        break;
    }
  }
}

async function main() {
  const absPath = resolve(PDF_PATH);
  const model = `lyzr:${LYZR_AGENT_ID}@https://agent-prod.studio.lyzr.ai/v4`;
  const agentDir = resolve("./agents/assistant");

  console.log("\n╔══════════════════════════════════════════════════════╗");
  console.log("║   Token Optimization Proof  —  gitagent + Lyzr AI   ║");
  console.log("╚══════════════════════════════════════════════════════╝");
  console.log(`  File : ${basename(absPath)}`);

  // ── Analysis ─────────────────────────────────────────────────────────
  sep("STEP 1: What compression does to this document");
  const doc = await analyzeDoc(absPath);

  console.log(`  Raw PDF size       : ${doc.rawSizeKB} KB  (binary — unreadable by LLM)`);
  console.log(`  After doc-convert  : ${doc.fullTokens.toLocaleString()} tokens  (clean markdown)`);
  console.log(`  CCR triggered      : ${doc.CCR ? "YES — too large, returning outline" : "NO — small enough to serve in full"}`);
  if (doc.CCR) {
    console.log(`  Outline size       : ${doc.outlineTok} tokens  (what agent sees first)`);
    console.log(`  Sections           : ${doc.chunks.length}`);
    console.log(`  Avg section size   : ~${Math.round(doc.chunks.reduce((s,c)=>s+c.estimatedTokens,0)/doc.chunks.length)} tokens`);
  }

  // ── Multi-turn cost simulation ────────────────────────────────────────
  if (doc.CCR) {
    sep("STEP 2: Token cost over a 5-question session");

    const SIM_TURNS = 5;
    const { withoutPerTurn, withPerTurn } = multiTurnSimulation(
      doc.fullTokens, doc.outlineTok, doc.chunks, SIM_TURNS
    );

    console.log(`\n  ${"Turn".padEnd(6)} ${"WITHOUT compression".padEnd(24)} ${"WITH CCR".padEnd(24)} ${"Saved"}`);
    console.log("  " + "─".repeat(70));

    let totalWithout = 0, totalWith = 0;
    for (let i = 0; i < SIM_TURNS; i++) {
      const wo = withoutPerTurn[i].input + withoutPerTurn[i].output;
      const wi = withPerTurn[i].input    + withPerTurn[i].output;
      totalWithout += wo;
      totalWith    += wi;
      const saved = wo - wi;
      const pct   = Math.round((saved / wo) * 100);
      console.log(
        `  ${"Q" + (i+1) + ":".padEnd(5)} ` +
        `${wo.toLocaleString().padEnd(24)} ` +
        `${wi.toLocaleString().padEnd(24)} ` +
        `${saved > 0 ? `−${saved.toLocaleString()} (${pct}%)` : "overhead"}`
      );
    }
    console.log("  " + "─".repeat(70));
    const totalSaved = totalWithout - totalWith;
    const totalPct   = Math.round((totalSaved / totalWithout) * 100);
    console.log(
      `  ${"TOTAL".padEnd(6)} ` +
      `${totalWithout.toLocaleString().padEnd(24)} ` +
      `${totalWith.toLocaleString().padEnd(24)} ` +
      `−${totalSaved.toLocaleString()} (${totalPct}%)`
    );

    console.log(`\n  At $3/million input tokens (GPT-4o pricing):`);
    console.log(`    Without compression : $${(totalWithout * 3 / 1_000_000).toFixed(4)} per 5-question session`);
    console.log(`    With CCR            : $${(totalWith    * 3 / 1_000_000).toFixed(4)} per 5-question session`);
    console.log(`    Savings             : $${(totalSaved   * 3 / 1_000_000).toFixed(4)} per session`);
  }

  // ── Live agent round: WITH compression ────────────────────────────────
  sep("STEP 3: Live agent reading the document (WITH compression)");
  await runAgentRound(
    "Live — WITH doc-converter + CCR",
    agentDir,
    `Read the document at ${absPath} and give me a 3-bullet summary of the leave policy.`,
    model,
  );

  console.log("\n\n══════════════════════════════════════════════════════");
  console.log(" Check Lyzr Studio → Monitoring for actual API token counts");
  console.log("══════════════════════════════════════════════════════\n");
}

main().catch(err => { console.error(err); process.exit(1); });
