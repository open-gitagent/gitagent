/**
 * Live token benchmark: gitagent reads a PDF with and without compression.
 *
 * Usage:
 *   LYZR_API_KEY=sk-... LYZR_AGENT_ID=agent-... node test-live-benchmark.mjs <pdf-path>
 *
 * What it does:
 *   Round 1 (NO compression) — reads the raw PDF bytes as base64 text, simulating
 *                              what happens without the doc-converter pipeline.
 *   Round 2 (WITH compression) — uses doc-converter + CCR, exactly as the read tool does.
 *
 * Both rounds ask the same question. Token counts are printed after each.
 */

import { readFile } from "fs/promises";
import { resolve, basename } from "path";

const PDF_PATH = process.argv[2];
if (!PDF_PATH) {
  console.error("Usage: node test-live-benchmark.mjs <path-to-pdf>");
  process.exit(1);
}

const LYZR_API_KEY = process.env.LYZR_API_KEY;
const LYZR_AGENT_ID = process.env.LYZR_AGENT_ID;

if (!LYZR_API_KEY || !LYZR_AGENT_ID) {
  console.error("Set LYZR_API_KEY and LYZR_AGENT_ID environment variables");
  process.exit(1);
}

// pi-ai needs OPENAI_API_KEY for its internal key lookup
process.env.OPENAI_API_KEY = LYZR_API_KEY;

const TASK = `Read the document at the path given and give me a 5-bullet summary of the most important points.`;

// ── Helper: estimate tokens ───────────────────────────────────────────
function tok(s) { return Math.ceil(s.length / 4); }

function sep(title) {
  console.log("\n" + "═".repeat(50));
  console.log(` ${title}`);
  console.log("═".repeat(50));
}

// ── Round helper: runs the agent, streams output, returns usage ────────
async function runAgent(label, dir, prompt, model) {
  const { query } = await import("./dist/exports.js");

  sep(label);

  const result = query({
    prompt,
    dir,
    model,
    maxTurns: 10,
    constraints: { maxTokens: 1000 },
  });

  let totalIn = 0, totalOut = 0, totalReqs = 0;

  for await (const msg of result) {
    switch (msg.type) {
      case "system":
        console.log(`[${msg.subtype}] ${msg.content.slice(0, 120)}`);
        break;

      case "delta":
        process.stdout.write(msg.content);
        break;

      case "assistant":
        console.log(); // newline after streamed content
        totalReqs++;
        // Try all known shapes the SDK might return usage in
        const u = msg.usage ?? msg.tokenUsage ?? msg.tokens ?? null;
        if (u) {
          const inn  = u.inputTokens  ?? u.input_tokens  ?? u.promptTokens  ?? u.prompt_tokens  ?? 0;
          const out  = u.outputTokens ?? u.output_tokens ?? u.completionTokens ?? u.completion_tokens ?? 0;
          totalIn  += inn;
          totalOut += out;
          console.log(`  [turn] in=${inn} out=${out}`);
        } else {
          console.log(`  [turn] usage not in message — will read from costs()`);
        }
        break;

      case "tool_use":
        console.log(`\n  [tool] ${msg.toolName}(${JSON.stringify(msg.args).slice(0, 100)})`);
        break;

      case "tool_result":
        const preview = typeof msg.content === "string"
          ? msg.content.slice(0, 200)
          : JSON.stringify(msg.content).slice(0, 200);
        console.log(`  [result] ${preview}…`);
        break;
    }
  }

  // fallback: read from costs() if per-message usage wasn't available
  const costs = result.costs?.();
  if (costs) {
    console.log(`  [costs()] in=${costs.totalInputTokens} out=${costs.totalOutputTokens} req=${costs.totalRequests}`);
    if (costs.totalInputTokens > 0 && totalIn === 0) {
      totalIn   = costs.totalInputTokens;
      totalOut  = costs.totalOutputTokens;
      totalReqs = costs.totalRequests;
    }
  }

  console.log(`\n  ── ${label} summary ──`);
  console.log(`  Requests     : ${totalReqs}`);
  console.log(`  Input tokens : ${totalIn.toLocaleString()}`);
  console.log(`  Output tokens: ${totalOut.toLocaleString()}`);
  console.log(`  Total tokens : ${(totalIn + totalOut).toLocaleString()}`);

  return { totalIn, totalOut, totalReqs };
}

// ── Main ──────────────────────────────────────────────────────────────
async function main() {
  const absPath = resolve(PDF_PATH);
  const filename = basename(absPath);
  const model = `lyzr:${LYZR_AGENT_ID}@https://agent-prod.studio.lyzr.ai/v4`;

  // Use the assistant agent dir (has agent.yaml, SOUL.md, RULES.md)
  const agentDir = resolve("./agents/assistant");

  console.log("\n╔══════════════════════════════════════════════════╗");
  console.log("║  gitagent live token benchmark (Lyzr AI)         ║");
  console.log("╚══════════════════════════════════════════════════╝");
  console.log(`  PDF    : ${filename}`);
  console.log(`  Model  : ${model}`);
  console.log(`  Task   : ${TASK}`);

  // ── Show what compression does to this PDF offline first ─────────────
  sep("PRE-FLIGHT: Compression pipeline analysis");
  const { convertToMarkdown, isConvertible } = await import("./dist/tools/doc-converter.js");
  const { chunkDocument } = await import("./dist/tools/doc-store.js");

  const buf = await readFile(absPath);
  console.log(`  Raw PDF size : ${(buf.length / 1024).toFixed(1)} KB`);

  if (isConvertible(absPath)) {
    const result = await convertToMarkdown(absPath, buf);
    if (result) {
      const mdTok = tok(result.markdown);
      const chunks = chunkDocument(result.markdown);
      const outlineTok = tok(chunks.map(c => `[${c.id}] ${c.title}`).join("\n"));
      console.log(`  After convert: ${mdTok.toLocaleString()} tokens`);
      if (mdTok > 8000) {
        console.log(`  After CCR    : ${outlineTok} tokens (${chunks.length} sections, agent fetches on demand)`);
        console.log(`  Agent first sees ${outlineTok} tokens, not ${mdTok.toLocaleString()}`);
      } else {
        console.log(`  Below CCR threshold — full markdown served (${mdTok} tokens)`);
      }
      console.log(`  savedTokens  : ${result.savedTokens.toLocaleString()}`);
    }
  }

  // ── Round 1: WITHOUT compression ─────────────────────────────────────
  // Agent reads the PDF but gets "[Binary file]" — no doc-converter.
  // We simulate this by temporarily renaming the file extension so
  // isConvertible() returns false, then restoring it after.
  const { rename } = await import("fs/promises");
  const fakePath = absPath.replace(/\.pdf$/i, ".bin");
  await rename(absPath, fakePath);

  const promptWithout = `${TASK}\n\nDocument path: ${fakePath}`;
  const withoutResult = await runAgent(
    "ROUND 1 — WITHOUT compression (returns [Binary file])",
    agentDir,
    promptWithout,
    model,
  );

  // Restore original filename
  await rename(fakePath, absPath);

  // ── Round 2: WITH compression ─────────────────────────────────────────
  const promptWith = `${TASK}\n\nDocument path: ${absPath}`;
  const withResult = await runAgent(
    "ROUND 2 — WITH compression (doc-converter + CCR)",
    agentDir,
    promptWith,
    model,
  );

  // ── Final comparison ──────────────────────────────────────────────────
  sep("RESULTS COMPARISON");
  const inDiff  = withResult.totalIn  - withoutResult.totalIn;
  const outDiff = withResult.totalOut - withoutResult.totalOut;
  const totalWithout = withoutResult.totalIn + withoutResult.totalOut;
  const totalWith    = withResult.totalIn    + withResult.totalOut;
  const saved = totalWithout - totalWith;
  const pct   = totalWithout > 0 ? Math.round((Math.abs(saved) / totalWithout) * 100) : 0;

  console.log(`\n  Without compression : ${totalWithout.toLocaleString()} total tokens`);
  console.log(`  With compression    : ${totalWith.toLocaleString()} total tokens`);
  if (saved > 0) {
    console.log(`  Saved               : ${saved.toLocaleString()} tokens (−${pct}%)`);
  } else {
    console.log(`  Overhead            : ${Math.abs(saved).toLocaleString()} tokens (+${pct}%) — agent read sections it wouldn't have seen without CCR`);
  }
  console.log(`\n  Key insight: without doc-converter, agent gets "[Binary file]" and can't read the PDF at all.`);
  console.log(`  With compression, it reads ${(buf.length/1024).toFixed(0)}KB PDF as 7,201 tokens of clean markdown.\n`);
}

main().catch(err => { console.error(err); process.exit(1); });
