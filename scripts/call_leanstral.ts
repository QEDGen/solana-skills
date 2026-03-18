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
 *         bun scripts/call_leanstral.ts --output-dir /tmp/out --passes 2 --validate
 *
 * Environment:
 *     MISTRAL_API_KEY — required. Get one free at https://console.mistral.ai
 *
 * Output:
 *     Creates output-dir/ as a Lean 4 project scaffold with:
 *         Best.lean              — selected proof candidate
 *         lakefile.lean          — Lean build configuration
 *         lean-toolchain         — Lean version specifier
 *         Main.lean              — entry point that imports Best
 *         README.md              — verification instructions
 *         .gitignore             — ignores build artifacts
 *         metadata.json          — timing, token usage, model info per completion
 *         prompt.txt             — the input prompt
 *         attempts/              — all completion attempts
 *             completion_0.lean      — first completion
 *             completion_0_raw.txt   — raw response with explanations
 *             completion_1.lean      — second completion
 *             completion_1_raw.txt   — raw response with explanations
 *             ...
 *
 *     To verify: cd output-dir && lake build
 */

import { parseArgs } from "util";
import {
  mkdir,
  writeFile,
  readFile,
  copyFile,
  access,
  cp,
} from "fs/promises";
import { join, dirname } from "path";
import { fileURLToPath } from "url";
import { homedir, platform } from "os";
import { spawn } from "child_process";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const API_URL = "https://api.mistral.ai/v1/chat/completions";
const MODEL = "labs-leanstral-2603";
const DEFAULT_PASSES = 4;
const DEFAULT_TEMPERATURE = 0.6;
const DEFAULT_MAX_TOKENS = 16384;
const TIMEOUT_MS = 180000; // 3 minutes per request
const MAX_RETRIES = 3;
const BACKOFF_BASE_MS = 2000; // 2 seconds

const SYSTEM_PROMPT = `You are Leanstral, an expert Lean 4 proof engineer.

Produce a single Lean 4 module that is as likely as possible to compile under Lean 4.15 + Mathlib 4.15.

Hard requirements:
1. Output exactly one Lean module.
2. Do not emit duplicate declarations.
3. Do not leave theorem bodies empty after \`:= by\`.
4. Do not invent identifiers, namespaces, or APIs that are not defined in the file or imported from Lean/Mathlib.
5. Use only Lean 4 / Mathlib identifiers you are confident exist in this toolchain version.
6. If a proof is incomplete, use \`sorry\` inside the proof body rather than leaving a stub.
7. Prefer a smaller, explicit semantic model over an ambitious but broken one.
8. Define the state transition functions before proving theorems about them.
9. If several properties are requested, it is acceptable to prove a smaller subset well rather than all of them badly.

Recommended structure:
- imports
- model types
- instruction/state transition functions
- helper lemmas
- theorems

Output policy:
- Prefer plain Lean code only.
- Do not include prose before or after the code unless explicitly requested.
- Use \`import Mathlib\` only if needed.
- Prefer self-contained proofs and simple executable definitions.
`;

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
  build_status: "not_run" | "success" | "failed" | "skipped";
  build_log_path?: string;
}

interface Metadata {
  model: string;
  passes: number;
  temperature: number;
  max_tokens: number;
  validate: boolean;
  completions: CompletionMetadata[];
  best_completion_index: number;
  best_sorry_count: number;
  best_selection_reason: string;
}

interface CommandResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

async function pathExists(path: string): Promise<boolean> {
  try {
    await access(path);
    return true;
  } catch {
    return false;
  }
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
        } else if (response.status === 403) {
          const errorBody = await response.text();
          if (errorBody.includes("labs_not_enabled")) {
            console.error(
              "ERROR: The Leanstral Labs model is not enabled for this Mistral organization.\n" +
                "Ask an org admin to enable Labs models at https://admin.mistral.ai/plateforme/privacy and retry."
            );
          } else {
            console.error(`ERROR: HTTP 403: ${errorBody}`);
          }
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

function normalizeLeanCode(code: string): string {
  const lines = code.split("\n");
  const normalizedImports: string[] = [];
  const bodyLines: string[] = [];
  let sawMathlibImport = false;

  for (const line of lines) {
    if (/^import\s+Mathlib(\..+)?\s*$/.test(line)) {
      sawMathlibImport = true;
      continue;
    }
    if (/^import\s+/.test(line)) {
      normalizedImports.push(line);
      continue;
    }
    bodyLines.push(line);
  }

  const importBlock = [
    ...(sawMathlibImport ? ["import Mathlib"] : []),
    ...normalizedImports,
  ];

  if (importBlock.length === 0) {
    return code;
  }

  const trimmedBody = bodyLines.join("\n").replace(/^\n+/, "");
  return `${importBlock.join("\n")}\n\n${trimmedBody}`.trimEnd() + "\n";
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

async function setupLeanProject(outputDir: string): Promise<void> {
  const templatesDir = join(__dirname, "templates");
  const supportDir = join(__dirname, "..", "lean_support");

  // Copy Lean project files
  await copyFile(
    join(templatesDir, "lakefile.lean"),
    join(outputDir, "lakefile.lean")
  );
  await copyFile(
    join(templatesDir, "lean-toolchain"),
    join(outputDir, "lean-toolchain")
  );
  await copyFile(
    join(templatesDir, "Main.lean"),
    join(outputDir, "Main.lean")
  );
  await copyFile(
    join(templatesDir, ".gitignore"),
    join(outputDir, ".gitignore")
  );
  await copyFile(
    join(templatesDir, "README.lean.md"),
    join(outputDir, "README.md")
  );
  await cp(supportDir, join(outputDir, "lean_support"), { recursive: true });
}

function validationWorkspaceDir(): string {
  if (process.env.LEANSTRAL_VALIDATION_WORKSPACE) {
    return process.env.LEANSTRAL_VALIDATION_WORKSPACE;
  }

  if (process.env.XDG_CACHE_HOME) {
    return join(process.env.XDG_CACHE_HOME, "leanstral-solana-skill", "validation-workspace");
  }

  if (platform() === "darwin") {
    return join(
      homedir(),
      "Library",
      "Caches",
      "leanstral-solana-skill",
      "validation-workspace"
    );
  }

  return join(homedir(), ".cache", "leanstral-solana-skill", "validation-workspace");
}

async function ensureValidationWorkspace(): Promise<string> {
  const workspaceDir = validationWorkspaceDir();
  await mkdir(workspaceDir, { recursive: true });
  await setupLeanProject(workspaceDir);

  return workspaceDir;
}

async function runCommand(
  command: string,
  args: string[],
  cwd: string,
  envOverrides: Record<string, string | undefined> = {}
): Promise<CommandResult> {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env: {
        ...process.env,
        ...envOverrides,
      },
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";

    child.stdout.on("data", (chunk) => {
      stdout += chunk.toString();
    });

    child.stderr.on("data", (chunk) => {
      stderr += chunk.toString();
    });

    child.on("error", reject);
    child.on("close", (code) => {
      resolve({ code, stdout, stderr });
    });
  });
}

async function validateCompletion(
  outputDir: string,
  completionIndex: number
): Promise<{ status: "success" | "failed" | "skipped"; logPath?: string }> {
  const logDir = join(outputDir, "validation");
  await mkdir(logDir, { recursive: true });

  const logPath = join(logDir, `completion_${completionIndex}.log`);

  try {
    const workspaceDir = await ensureValidationWorkspace();
    await copyFile(join(outputDir, "Best.lean"), join(workspaceDir, "Best.lean"));
    const lakeEnv = {
      LAKE_ARTIFACT_CACHE: process.env.LAKE_ARTIFACT_CACHE || "true",
    };

    const updateResult = await runCommand(
      "lake",
      ["update", "leanstralSupport"],
      workspaceDir,
      lakeEnv
    );
    if (updateResult.code !== 0) {
      const combinedOutput = [updateResult.stdout, updateResult.stderr].filter(Boolean).join("\n");
      await writeFile(logPath, combinedOutput, "utf-8");
      return {
        status: "failed",
        logPath,
      };
    }

    const result = await runCommand(
      "lake",
      ["--try-cache", "build", "Best"],
      workspaceDir,
      lakeEnv
    );
    const combinedOutput = [result.stdout, result.stderr].filter(Boolean).join("\n");
    await writeFile(logPath, combinedOutput, "utf-8");
    return {
      status: result.code === 0 ? "success" : "failed",
      logPath,
    };
  } catch (error) {
    const message =
      error instanceof Error ? error.message : `Unexpected error: ${String(error)}`;
    await writeFile(logPath, message, "utf-8");
    return {
      status: "skipped",
      logPath,
    };
  }
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
      validate: { type: "boolean" },
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
  --validate              Run 'lake build Best' on candidates and prefer a successful build
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
  const validate = Boolean(values.validate);

  // Create output directory
  await mkdir(outputDir, { recursive: true });
  const attemptsDir = join(outputDir, "attempts");
  await mkdir(attemptsDir, { recursive: true });

  // Set up Lean project files (lakefile, toolchain, etc.)
  await setupLeanProject(outputDir);

  // Save the prompt for reference
  await writeFile(join(outputDir, "prompt.txt"), prompt, "utf-8");

  console.error(`Calling Leanstral (${MODEL}) with pass@${passes}...`);

  const metadata: Metadata = {
    model: MODEL,
    passes,
    temperature,
    max_tokens: maxTokens,
    validate,
    completions: [],
    best_completion_index: 0,
    best_sorry_count: Infinity,
    best_selection_reason: "fewest_sorry",
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
    const leanCode = normalizeLeanCode(extractLeanCode(content));
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
      build_status: "not_run",
    });

    // Track best completion (fewest sorry markers)
    if (sorryCount < bestSorryCount) {
      bestSorryCount = sorryCount;
      bestIdx = i;
    }
  }

  if (validate) {
    console.error(`\nValidating completions with 'lake build Best'...`);
    const rankedCandidates = [...metadata.completions].sort((a, b) => {
      if (a.sorry_count !== b.sorry_count) {
        return a.sorry_count - b.sorry_count;
      }
      return a.index - b.index;
    });

    let foundValidatedCompletion = false;

    for (const candidate of rankedCandidates) {
      const candidateLean = await readFile(
        join(attemptsDir, `completion_${candidate.index}.lean`),
        "utf-8"
      );
      await writeFile(join(outputDir, "Best.lean"), candidateLean, "utf-8");

      process.stderr.write(
        `  Validate completion_${candidate.index}.lean (${candidate.sorry_count} sorry)... `
      );
      const validation = await validateCompletion(outputDir, candidate.index);
      candidate.build_status = validation.status;
      candidate.build_log_path = validation.logPath;
      console.error(validation.status);

      if (validation.status === "success") {
        bestIdx = candidate.index;
        bestSorryCount = candidate.sorry_count;
        metadata.best_selection_reason = "validated_build";
        foundValidatedCompletion = true;
        break;
      }
    }

    if (!foundValidatedCompletion) {
      metadata.best_selection_reason = "fewest_sorry_no_valid_build";
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

  // Copy best completion to a convenient location (Best.lean for Lean module naming)
  const bestLean = await readFile(
    join(attemptsDir, `completion_${bestIdx}.lean`),
    "utf-8"
  );
  await writeFile(join(outputDir, "Best.lean"), bestLean, "utf-8");

  console.error(`\nResults saved to ${outputDir}/`);
  console.error(
    `Best completion: Best.lean (from attempts/completion_${bestIdx}.lean, ${bestSorryCount} sorry)`
  );
  console.error(`Selection reason: ${metadata.best_selection_reason}`);
  console.error(`\nTo verify the proof:`);
  console.error(`  cd ${outputDir}`);
  console.error(`  lake build   # Build and verify proofs`);

  // Print the best completion to stdout for easy piping
  console.log(bestLean);
}

main().catch((error) => {
  console.error("Unexpected error:", error);
  process.exit(1);
});
