#!/usr/bin/env bun
/**
 * call_leanstral.ts — Call Mistral's Leanstral model for Lean 4 proof generation.
 *
 * Sends a prompt to the labs-leanstral-2603 endpoint and returns N independent
 * completions (pass@N) so the caller can pick the best proof.
 *
 * Usage:
 *     bun scripts/call_leanstral.ts \
 *         --prompt-file /tmp/leanstral_prompt.txt \
 *         --output-dir /tmp/leanstral_output \
 *         --passes 4 \
 *         --temperature 0.6
 *
 *     # Or pipe a prompt directly:
 *     echo "Prove that addition is commutative in Lean 4" | \
 *         bun scripts/call_leanstral.ts --output-dir /tmp/out --passes 2
 *
 * Environment:
 *     MISTRAL_API_KEY — required. Get one free at https://console.mistral.ai
 *
 * Output:
 *     Creates output-dir/ with:
 *         best.lean              — best completion (fewest sorry markers)
 *         metadata.json          — timing, token usage, model info per completion
 *         prompt.txt             — the input prompt
 *         attempts/              — all completion attempts
 *             completion_0.lean      — first completion
 *             completion_0_raw.txt   — raw response with explanations
 *             completion_1.lean      — second completion
 *             completion_1_raw.txt   — raw response with explanations
 *             ...
 */

import { parseArgs } from "util";
import { mkdir, writeFile, readFile } from "fs/promises";
import { join } from "path";

const API_URL = "https://api.mistral.ai/v1/chat/completions";
const MODEL = "labs-leanstral-2603";
const DEFAULT_PASSES = 4;
const DEFAULT_TEMPERATURE = 0.6;
const DEFAULT_MAX_TOKENS = 16384;
const TIMEOUT_MS = 180000; // 3 minutes per request
const MAX_RETRIES = 3;
const BACKOFF_BASE_MS = 2000; // 2 seconds

const SYSTEM_PROMPT = `You are Leanstral, an expert Lean 4 proof engineer. When given a program or specification:

1. Define the relevant types and functions in Lean 4 that faithfully model the program.
2. State the theorem or property formally as a Lean 4 theorem.
3. Prove the theorem using appropriate tactics (simp, omega, induction, cases, etc.).
4. If you cannot complete a sub-proof, use \`sorry\` and explain what additional lemmas would be needed.
5. After the proof, briefly explain your proof strategy.

Always produce valid Lean 4 syntax. Use \`import Mathlib\` only if needed. Prefer self-contained proofs where possible.`;

interface ApiResponse {
  choices?: Array<{
    message?: {
      content?: string;
    };
    finish_reason?: string;
  }>;
  usage?: {
    prompt_tokens?: number;
    completion_tokens?: number;
    total_tokens?: number;
  };
  _elapsed_seconds?: number;
}

interface CompletionMetadata {
  index: number;
  sorry_count: number;
  elapsed_seconds: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  finish_reason: string;
}

interface Metadata {
  model: string;
  passes: number;
  temperature: number;
  max_tokens: number;
  completions: CompletionMetadata[];
  best_completion_index: number;
  best_sorry_count: number;
}

async function callApi(
  prompt: string,
  apiKey: string,
  temperature: number,
  maxTokens: number
): Promise<ApiResponse> {
  const payload = {
    model: MODEL,
    messages: [
      { role: "system", content: SYSTEM_PROMPT },
      { role: "user", content: prompt },
    ],
    temperature,
    max_tokens: maxTokens,
  };

  for (let attempt = 0; attempt < MAX_RETRIES; attempt++) {
    try {
      const start = Date.now();
      const response = await fetch(API_URL, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${apiKey}`,
        },
        body: JSON.stringify(payload),
        signal: AbortSignal.timeout(TIMEOUT_MS),
      });

      const elapsed = (Date.now() - start) / 1000;

      if (!response.ok) {
        if (response.status === 429) {
          const wait = BACKOFF_BASE_MS * Math.pow(2, attempt);
          console.error(
            `  Rate limited (429). Retrying in ${wait / 1000}s... (attempt ${attempt + 1}/${MAX_RETRIES})`
          );
          await new Promise((resolve) => setTimeout(resolve, wait));
          continue;
        } else if (response.status === 401) {
          console.error(
            "ERROR: Invalid or missing MISTRAL_API_KEY. Get one at https://console.mistral.ai"
          );
          process.exit(1);
        } else {
          const errorBody = await response.text();
          console.error(`ERROR: HTTP ${response.status}: ${errorBody}`);
          if (attempt < MAX_RETRIES - 1) {
            await new Promise((resolve) =>
              setTimeout(resolve, BACKOFF_BASE_MS * Math.pow(2, attempt))
            );
            continue;
          }
          process.exit(1);
        }
      }

      const body: ApiResponse = await response.json();
      body._elapsed_seconds = elapsed;
      return body;
    } catch (error) {
      console.error(`ERROR: ${error}`);
      if (attempt < MAX_RETRIES - 1) {
        await new Promise((resolve) =>
          setTimeout(resolve, BACKOFF_BASE_MS * Math.pow(2, attempt))
        );
        continue;
      }
      process.exit(1);
    }
  }

  console.error("ERROR: All retries exhausted.");
  process.exit(1);
}

function extractLeanCode(content: string): string {
  // If the response has ```lean or ```lean4 blocks, extract them
  const blocks = content.matchAll(/```lean4?\s*\n(.*?)```/gs);
  const extracted = Array.from(blocks, (m) => m[1]);

  if (extracted.length > 0) {
    return extracted.join("\n\n");
  }

  // If no code fences, return the whole content
  return content;
}

function countSorry(code: string): number {
  return (code.match(/\bsorry\b/g) || []).length;
}

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk);
  }
  return Buffer.concat(chunks).toString("utf-8");
}

async function main() {
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      "prompt-file": { type: "string" },
      "output-dir": { type: "string" },
      passes: { type: "string", default: String(DEFAULT_PASSES) },
      temperature: { type: "string", default: String(DEFAULT_TEMPERATURE) },
      "max-tokens": { type: "string", default: String(DEFAULT_MAX_TOKENS) },
      help: { type: "boolean", short: "h" },
    },
    allowPositionals: false,
  });

  if (values.help) {
    console.log(`Usage: bun scripts/call_leanstral.ts [options]

Options:
  --prompt-file FILE      Path to a text file containing the prompt. If omitted, reads from stdin.
  --output-dir DIR        Directory to write completions and metadata to. (required)
  --passes N              Number of independent completions (pass@N). Default: ${DEFAULT_PASSES}
  --temperature T         Sampling temperature. Default: ${DEFAULT_TEMPERATURE}
  --max-tokens N          Max tokens per completion. Default: ${DEFAULT_MAX_TOKENS}
  -h, --help              Show this help message

Environment:
  MISTRAL_API_KEY         Required. Get one free at https://console.mistral.ai
`);
    process.exit(0);
  }

  // Check required arguments
  if (!values["output-dir"]) {
    console.error("ERROR: --output-dir is required");
    process.exit(1);
  }

  const apiKey = process.env.MISTRAL_API_KEY;
  if (!apiKey) {
    console.error(
      "ERROR: MISTRAL_API_KEY environment variable is not set.\n" +
        "Get a free key at https://console.mistral.ai\n" +
        "Then run: export MISTRAL_API_KEY=your_key_here"
    );
    process.exit(1);
  }

  // Read prompt
  let prompt: string;
  if (values["prompt-file"]) {
    prompt = await readFile(values["prompt-file"], "utf-8");
  } else {
    if (process.stdin.isTTY) {
      console.error("Reading prompt from stdin (Ctrl+D to finish):");
    }
    prompt = await readStdin();
  }

  if (!prompt.trim()) {
    console.error("ERROR: Empty prompt.");
    process.exit(1);
  }

  const outputDir = values["output-dir"];
  const passes = parseInt(values.passes || String(DEFAULT_PASSES));
  const temperature = parseFloat(
    values.temperature || String(DEFAULT_TEMPERATURE)
  );
  const maxTokens = parseInt(values["max-tokens"] || String(DEFAULT_MAX_TOKENS));

  // Create output directory
  await mkdir(outputDir, { recursive: true });
  const attemptsDir = join(outputDir, "attempts");
  await mkdir(attemptsDir, { recursive: true });

  // Save the prompt for reference
  await writeFile(join(outputDir, "prompt.txt"), prompt, "utf-8");

  console.error(`Calling Leanstral (${MODEL}) with pass@${passes}...`);

  const metadata: Metadata = {
    model: MODEL,
    passes,
    temperature,
    max_tokens: maxTokens,
    completions: [],
    best_completion_index: 0,
    best_sorry_count: Infinity,
  };

  let bestIdx = 0;
  let bestSorryCount = Infinity;

  for (let i = 0; i < passes; i++) {
    process.stderr.write(`  Pass ${i + 1}/${passes}... `);
    const response = await callApi(prompt, apiKey, temperature, maxTokens);

    // Extract the assistant's message
    const content =
      response.choices?.[0]?.message?.content || "";
    const elapsed = response._elapsed_seconds || 0;
    const usage = response.usage || {};

    // Extract Lean code and count sorry markers
    const leanCode = extractLeanCode(content);
    const sorryCount = countSorry(leanCode);

    console.error(
      `done (${elapsed.toFixed(1)}s, ${usage.completion_tokens || "?"} tokens, ${sorryCount} sorry)`
    );

    // Save the raw completion (full response including explanations)
    await writeFile(
      join(attemptsDir, `completion_${i}_raw.txt`),
      content,
      "utf-8"
    );
    // Save just the extracted Lean code
    await writeFile(
      join(attemptsDir, `completion_${i}.lean`),
      leanCode,
      "utf-8"
    );

    // Track metadata
    metadata.completions.push({
      index: i,
      sorry_count: sorryCount,
      elapsed_seconds: elapsed,
      prompt_tokens: usage.prompt_tokens || 0,
      completion_tokens: usage.completion_tokens || 0,
      total_tokens: usage.total_tokens || 0,
      finish_reason: response.choices?.[0]?.finish_reason || "unknown",
    });

    // Track best completion (fewest sorry markers)
    if (sorryCount < bestSorryCount) {
      bestSorryCount = sorryCount;
      bestIdx = i;
    }
  }

  metadata.best_completion_index = bestIdx;
  metadata.best_sorry_count = bestSorryCount;

  // Save metadata
  await writeFile(
    join(outputDir, "metadata.json"),
    JSON.stringify(metadata, null, 2),
    "utf-8"
  );

  // Copy best completion to a convenient location
  const bestLean = await readFile(
    join(attemptsDir, `completion_${bestIdx}.lean`),
    "utf-8"
  );
  await writeFile(join(outputDir, "best.lean"), bestLean, "utf-8");

  console.error(`\nResults saved to ${outputDir}/`);
  console.error(
    `Best completion: attempts/completion_${bestIdx}.lean (${bestSorryCount} sorry)`
  );

  // Print the best completion to stdout for easy piping
  console.log(bestLean);
}

main().catch((error) => {
  console.error("Unexpected error:", error);
  process.exit(1);
});
