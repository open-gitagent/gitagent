/**
 * Token usage benchmark: before vs after compression features.
 * Tests SmartCrusher (JSON), CodeCompressor (TS), and doc-converter (PDF).
 *
 * Usage: node test-compression.mjs [path-to-pdf]
 */

import { readFile } from "fs/promises";
import { resolve, basename } from "path";

// ── Token estimator (same formula as read.ts) ──────────────────────────
function estimateTokens(s) {
  return Math.ceil(s.length / 4);
}

function bar(pct, width = 30) {
  const filled = Math.round((pct / 100) * width);
  return "[" + "█".repeat(filled) + "░".repeat(width - filled) + "]";
}

function printResult(label, before, after, note = "") {
  const saved = before - after;
  const pct = before > 0 ? Math.round((saved / before) * 100) : 0;
  console.log(`\n  ${label}`);
  console.log(`    Before : ${before.toLocaleString()} tokens`);
  console.log(`    After  : ${after.toLocaleString()} tokens`);
  console.log(`    Saved  : ${saved.toLocaleString()} tokens  ${bar(pct)} ${pct}%`);
  if (note) console.log(`    Note   : ${note}`);
}

// ── 1. SmartCrusher on package-lock.json ──────────────────────────────
async function testSmartCrusher() {
  console.log("\n══════════════════════════════════════════");
  console.log(" TEST 1: SmartCrusher  (package-lock.json)");
  console.log("══════════════════════════════════════════");

  const { crushJson, isJson } = await import("./dist/compression/index.js");

  const raw = await readFile(new URL("./package-lock.json", import.meta.url), "utf-8").catch(() => null);
  if (!raw) { console.log("  [SKIP] package-lock.json not found — run npm install first"); return; }

  if (!isJson(raw)) { console.log("  [SKIP] not valid JSON"); return; }

  const beforeTokens = estimateTokens(raw);
  const { compressed, reductionPct } = crushJson(raw);
  const afterTokens = estimateTokens(compressed);

  printResult("package-lock.json", beforeTokens, afterTokens,
    `SmartCrusher reported −${reductionPct}%`);
}

// ── 2. CodeCompressor on a TypeScript source file ─────────────────────
async function testCodeCompressor() {
  console.log("\n══════════════════════════════════════════");
  console.log(" TEST 2: CodeCompressor  (src/loader.ts)");
  console.log("══════════════════════════════════════════");

  const { compressCode, isSourceFile } = await import("./dist/compression/index.js");

  const filePath = new URL("./src/loader.ts", import.meta.url);
  const raw = await readFile(filePath, "utf-8").catch(() => null);
  if (!raw) { console.log("  [SKIP] src/loader.ts not found"); return; }

  const filename = "loader.ts";
  if (!isSourceFile(filename)) { console.log("  [SKIP] not a recognised source file"); return; }

  const beforeTokens = estimateTokens(raw);
  const { compressed, reductionPct, language } = compressCode(raw, filename);
  const afterTokens = estimateTokens(compressed);

  printResult(`loader.ts (${language})`, beforeTokens, afterTokens,
    `CodeCompressor reported −${reductionPct}%`);
}

// ── 3. Doc-converter on the Employee Handbook PDF ─────────────────────
async function testDocConverter(pdfPath) {
  console.log("\n══════════════════════════════════════════");
  console.log(" TEST 3: Doc-converter  (PDF → markdown)");
  console.log("══════════════════════════════════════════");

  const { convertToMarkdown, isConvertible } = await import("./dist/tools/doc-converter.js");

  if (!pdfPath) {
    console.log("  [SKIP] no PDF path provided. Pass path as argument: node test-compression.mjs /path/to/file.pdf");
    return;
  }

  const abs = resolve(pdfPath);
  if (!isConvertible(abs)) { console.log(`  [SKIP] ${basename(abs)} is not a convertible format`); return; }

  console.log(`  Converting: ${basename(abs)}`);
  const buffer = await readFile(abs).catch(() => null);
  if (!buffer) { console.log("  [SKIP] file not found"); return; }

  // Raw PDF size as "before" — how many tokens the raw bytes would waste
  const rawSizeTokens = estimateTokens(buffer.toString("latin1"));

  const result = await convertToMarkdown(abs, buffer);
  if (!result) { console.log("  [FAIL] conversion returned null"); return; }

  const afterTokens = estimateTokens(result.markdown);

  console.log(`\n  ${basename(abs)}`);
  console.log(`    Raw binary size  : ${(buffer.length / 1024).toFixed(1)} KB  (~${rawSizeTokens.toLocaleString()} raw bytes/4 tokens)`);
  console.log(`    Markdown output  : ${afterTokens.toLocaleString()} tokens`);
  console.log(`    Markdown preview :\n`);
  const preview = result.markdown.slice(0, 500).replace(/\n/g, "\n      ");
  console.log(`      ${preview}…`);
  console.log(`\n    Doc-converter savedTokens field: ${result.savedTokens.toLocaleString()}`);

  // ── CCR chunking ─────────────────────────────────────────────────────
  const { chunkDocument } = await import("./dist/tools/doc-store.js");
  const CCR_THRESHOLD = 8000;

  if (afterTokens > CCR_THRESHOLD) {
    console.log(`\n  ── CCR chunking (doc > ${CCR_THRESHOLD} tokens) ──`);
    const chunks = chunkDocument(result.markdown);
    const agentSeesTokens = estimateTokens(
      chunks.map(c => `[${c.id}] ${c.title} (~${c.estimatedTokens} tokens)`).join("\n")
    );
    console.log(`    Total sections   : ${chunks.length}`);
    console.log(`    Outline tokens   : ${agentSeesTokens.toLocaleString()}  (what agent sees first)`);
    console.log(`    Full doc tokens  : ${afterTokens.toLocaleString()}  (fetched on demand)`);
    printResult("CCR vs full markdown", afterTokens, agentSeesTokens,
      "agent sees outline; fetches sections on demand");
    console.log("\n  Section outline:");
    chunks.forEach(c => console.log(`    [${c.id}] ${c.title}  (~${c.estimatedTokens} tok)`));
  } else {
    console.log(`  Doc is ${afterTokens} tokens — below CCR threshold (${CCR_THRESHOLD}), served in full.`);
  }
}

// ── 4. CacheAligner system prompt ─────────────────────────────────────
async function testCacheAligner() {
  console.log("\n══════════════════════════════════════════");
  console.log(" TEST 4: CacheAligner  (system prompt)");
  console.log("══════════════════════════════════════════");

  const { alignSystemPrompt, cacheEfficiency } = await import("./dist/compression/index.js");

  // Simulate typical gitagent system prompt parts
  const parts = {
    identity: "You are a helpful AI agent built on the GitAgent framework.\n".repeat(20),
    knowledge: "## Knowledge\nRelevant domain knowledge goes here.\n".repeat(10),
    skills:    "## Skills\n- skill_a\n- skill_b\n- skill_c\n".repeat(15),
    memory:    "## Memory\nPrevious conversations and learned facts.\n".repeat(5),
    task:      "User task: analyze the codebase and summarise findings.",
  };

  const naivePrompt = Object.values(parts).join("\n\n");
  const aligned = alignSystemPrompt(parts);
  const efficiency = cacheEfficiency(aligned);

  const beforeTokens = estimateTokens(naivePrompt);
  const staticTokens = aligned.staticTokens;
  const dynamicTokens = aligned.dynamicTokens;
  const efficiencyPct = Math.round(efficiency * 100);

  console.log(`\n  Naive (unordered) prompt : ${beforeTokens.toLocaleString()} tokens`);
  console.log(`  Aligned static prefix    : ${staticTokens.toLocaleString()} tokens  ← KV cache eligible`);
  console.log(`  Aligned dynamic suffix   : ${dynamicTokens.toLocaleString()} tokens  ← changes per session`);
  console.log(`  Cache efficiency         : ${efficiencyPct}%`);
  console.log(`  (same total tokens, but ${efficiencyPct}% is cache-stable across sessions)`);
}

// ── Main ───────────────────────────────────────────────────────────────
async function main() {
  const pdfArg = process.argv[2];

  console.log("\n╔══════════════════════════════════════════╗");
  console.log("║   gitagent compression benchmark         ║");
  console.log("╚══════════════════════════════════════════╝");

  await testSmartCrusher();
  await testCodeCompressor();
  await testDocConverter(pdfArg);
  await testCacheAligner();

  console.log("\n══════════════════════════════════════════");
  console.log(" Done.");
  console.log("══════════════════════════════════════════\n");
}

main().catch(err => { console.error(err); process.exit(1); });
