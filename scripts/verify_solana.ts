#!/usr/bin/env bun

import { parseArgs } from "util";
import { mkdir, readFile, writeFile, copyFile } from "fs/promises";
import { join, dirname, resolve, isAbsolute } from "path";
import { spawn } from "child_process";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const REPO_ROOT = resolve(__dirname, "..");

interface PropertyCandidate {
  id: string;
  category: string;
  title: string;
  confidence: string;
  relevant_instructions: string[];
  evidence: string[];
  prompt_hint: string;
}

interface AnalysisIr {
  property_candidates: PropertyCandidate[];
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

interface LeanstralMetadata {
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

function usage(): string {
  return `Usage: bun scripts/verify_solana.ts [--idl path/to/idl.json] [--input path/to/lib.rs] [options]

Options:
  --idl FILE             Anchor IDL JSON (preferred for Anchor projects)
  --input FILE           Anchor program Rust source to enrich semantics
  --tests FILE           Optional test file; can be repeated
  --analysis-dir DIR     Directory to write analysis artifacts
  --output-dir DIR       Directory to write generated Lean projects
  --passes N             Pass@N for Leanstral generation. Default: 3
  --temperature T        Leanstral sampling temperature. Default: 0.2
  --max-tokens N         Max tokens per Leanstral completion. Default: script default
  --top-k N              Number of top property candidates to generate. Default: 3
  --validate             Pass through to call_leanstral.ts
  --repair-rounds N      Compiler-guided repair attempts after failed validation. Default: 1
  --analysis-only        Stop after analyzer output and ranking
  -h, --help             Show this help message
`;
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
    child.on("close", (code) => resolve({ code, stdout, stderr }));
  });
}

function candidatePriority(candidate: PropertyCandidate): number {
  const confidenceScore =
    candidate.confidence === "high" ? 0 : candidate.confidence === "medium" ? 1 : 2;
  const categoryScore =
    candidate.category === "access_control"
      ? 0
      : candidate.category === "conservation"
        ? 1
        : candidate.category === "state_machine"
          ? 2
          : candidate.category === "arithmetic_safety"
            ? 3
            : 4;
  return confidenceScore * 10 + categoryScore;
}

function selectCandidates(
  candidates: PropertyCandidate[],
  topK: number
): PropertyCandidate[] {
  const ranked = [...candidates].sort((a, b) => candidatePriority(a) - candidatePriority(b));
  const selected: PropertyCandidate[] = [];
  const seen = new Set<string>();

  // First pass: maximize category coverage so we do not spend all API calls on
  // repeated access-control prompts when other strong properties are available.
  for (const category of [
    "access_control",
    "conservation",
    "state_machine",
    "arithmetic_safety",
  ]) {
    const candidate = ranked.find((item) => item.category === category);
    if (!candidate || seen.has(candidate.id)) {
      continue;
    }
    selected.push(candidate);
    seen.add(candidate.id);
    if (selected.length >= topK) {
      return selected;
    }
  }

  // Second pass: fill remaining slots by overall rank.
  for (const candidate of ranked) {
    if (seen.has(candidate.id)) {
      continue;
    }
    selected.push(candidate);
    seen.add(candidate.id);
    if (selected.length >= topK) {
      break;
    }
  }

  return selected;
}

function resolveCliPath(pathValue: string, invocationCwd: string): string {
  return isAbsolute(pathValue) ? pathValue : resolve(invocationCwd, pathValue);
}

async function fileExists(pathValue: string): Promise<boolean> {
  try {
    await readFile(pathValue, "utf-8");
    return true;
  } catch {
    return false;
  }
}

async function readJsonFile<T>(pathValue: string): Promise<T> {
  return JSON.parse(await readFile(pathValue, "utf-8")) as T;
}

function commonAncestor(paths: string[]): string | undefined {
  if (paths.length === 0) {
    return undefined;
  }

  const splitPaths = paths.map((pathValue) => resolve(pathValue).split("/").filter(Boolean));
  const prefix: string[] = [];

  for (let index = 0; ; index++) {
    const segment = splitPaths[0]?.[index];
    if (!segment || splitPaths.some((parts) => parts[index] !== segment)) {
      break;
    }
    prefix.push(segment);
  }

  if (prefix.length === 0) {
    return "/";
  }

  return `/${prefix.join("/")}`;
}

function deriveProjectRoot(paths: string[]): string | undefined {
  const existingPaths = paths.filter(Boolean);
  if (existingPaths.length === 0) {
    return undefined;
  }

  const ancestor = commonAncestor(existingPaths);
  if (!ancestor) {
    return undefined;
  }

  const targetIdx = ancestor.lastIndexOf("/target");
  if (targetIdx > 0) {
    return ancestor.slice(0, targetIdx);
  }

  return ancestor;
}

async function runLeanstralGeneration(
  promptFile: string,
  candidateOutputDir: string,
  values: Record<string, string | boolean | string[] | undefined>,
  validationWorkspace?: string
): Promise<CommandResult> {
  const leanstralArgs = [
    "scripts/call_leanstral.ts",
    "--prompt-file",
    promptFile,
    "--output-dir",
    candidateOutputDir,
    "--passes",
    values.passes || "3",
    "--temperature",
    values.temperature || "0.2",
  ];

  if (values["max-tokens"]) {
    leanstralArgs.push("--max-tokens", values["max-tokens"] as string);
  }
  if (values.validate) {
    leanstralArgs.push("--validate");
  }

  return await runCommand("bun", leanstralArgs, REPO_ROOT, {
    LEANSTRAL_VALIDATION_WORKSPACE: validationWorkspace,
  });
}

function pickBestFailedCompletion(
  metadata: LeanstralMetadata
): CompletionMetadata | undefined {
  const failed = metadata.completions.filter(
    (completion) => completion.build_status === "failed" && completion.build_log_path
  );

  return failed.sort((a, b) => {
    if (a.sorry_count !== b.sorry_count) {
      return a.sorry_count - b.sorry_count;
    }
    return a.index - b.index;
  })[0];
}

function buildRepairPrompt(
  originalPrompt: string,
  currentLean: string,
  buildLog: string,
  round: number
): string {
  const summarizedBuildLog = summarizeBuildLog(buildLog);
  return `You previously generated a Lean 4 proof module for this Solana verification task, but it did not compile.

Repair the Lean file using the compiler feedback below.

Hard requirements:
1. Return exactly one Lean 4 module.
2. Keep the same property target unless the build log proves it is impossible as stated.
3. Fix compiler errors concretely; do not leave declarations duplicated or theorem bodies empty.
4. Do not invent APIs or namespaces.
5. Prefer the smallest self-contained model that compiles under Lean 4.15 + Mathlib 4.15.
6. If a proof is incomplete, use \`sorry\` inside the proof body rather than leaving broken syntax.

This is repair round ${round}.

## Original Verification Task
${originalPrompt}

## Previous Lean Module
\`\`\`lean
${currentLean}
\`\`\`

## Lean Build Output
\`\`\`
${summarizedBuildLog}
\`\`\`

## Repair Goal
Produce a revised Lean module that addresses the reported compiler errors and is more likely to pass \`lake build Best\`. Return Lean code only.
`;
}

function summarizeBuildLog(buildLog: string): string {
  const lines = buildLog.split("\n");
  const errorLineIndexes = lines
    .map((line, index) => ({ line, index }))
    .filter(({ line }) => /\berror:|^error\b|unknown identifier|unexpected token/i.test(line))
    .map(({ index }) => index);

  if (errorLineIndexes.length > 0) {
    const selected = new Set<number>();
    for (const index of errorLineIndexes.slice(-8)) {
      for (let cursor = Math.max(0, index - 3); cursor <= Math.min(lines.length - 1, index + 6); cursor++) {
        selected.add(cursor);
      }
    }
    return [...selected]
      .sort((a, b) => a - b)
      .map((index) => lines[index])
      .join("\n")
      .slice(0, 24000);
  }

  return lines.slice(-250).join("\n").slice(0, 24000);
}

async function attemptRepairs(
  candidateId: string,
  candidateOutputDir: string,
  originalPromptFile: string,
  repairRounds: number,
  values: Record<string, string | boolean | string[] | undefined>,
  validationWorkspace?: string
): Promise<void> {
  if (!values.validate || repairRounds <= 0) {
    return;
  }

  const metadataPath = join(candidateOutputDir, "metadata.json");
  if (!(await fileExists(metadataPath))) {
    return;
  }

  let metadata = await readJsonFile<LeanstralMetadata>(metadataPath);
  if (metadata.best_selection_reason === "validated_build") {
    return;
  }

  const originalPrompt = await readFile(originalPromptFile, "utf-8");

  for (let round = 1; round <= repairRounds; round++) {
    const failedCompletion = pickBestFailedCompletion(metadata);
    if (!failedCompletion?.build_log_path) {
      console.error(`No failed validated completion available to repair for ${candidateId}.`);
      return;
    }

    const currentLean = await readFile(
      join(candidateOutputDir, "attempts", `completion_${failedCompletion.index}.lean`),
      "utf-8"
    );
    const buildLog = await readFile(failedCompletion.build_log_path, "utf-8");
    const repairPrompt = buildRepairPrompt(originalPrompt, currentLean, buildLog, round);
    const repairPromptFile = join(candidateOutputDir, `repair_round_${round}.prompt.txt`);
    const repairOutputDir = join(candidateOutputDir, `repair_round_${round}`);

    await mkdir(repairOutputDir, { recursive: true });
    await writeFile(repairPromptFile, repairPrompt, "utf-8");

    console.error(`Repairing ${candidateId} (round ${round}/${repairRounds})...`);
    const repairResult = await runLeanstralGeneration(
      repairPromptFile,
      repairOutputDir,
      values,
      validationWorkspace
    );
    process.stderr.write(repairResult.stderr);
    if (repairResult.code !== 0) {
      console.error(`Repair generation failed for ${candidateId} on round ${round}.`);
      return;
    }

    const repairedMetadataPath = join(repairOutputDir, "metadata.json");
    if (!(await fileExists(repairedMetadataPath))) {
      console.error(`Repair metadata missing for ${candidateId} on round ${round}.`);
      return;
    }

    metadata = await readJsonFile<LeanstralMetadata>(repairedMetadataPath);
    if (metadata.best_selection_reason === "validated_build") {
      await copyFile(join(repairOutputDir, "Best.lean"), join(candidateOutputDir, "Best.lean"));
      await copyFile(repairedMetadataPath, join(candidateOutputDir, "metadata.json"));
      console.error(`Repair succeeded for ${candidateId} on round ${round}.`);
      return;
    }
  }
}

async function main() {
  const invocationCwd = process.cwd();
  const { values } = parseArgs({
    args: process.argv.slice(2),
    options: {
      idl: { type: "string" },
      input: { type: "string" },
      tests: { type: "string", multiple: true },
      "analysis-dir": { type: "string" },
      "output-dir": { type: "string" },
      passes: { type: "string", default: "3" },
      temperature: { type: "string", default: "0.2" },
      "max-tokens": { type: "string" },
      "top-k": { type: "string", default: "3" },
      "repair-rounds": { type: "string", default: "1" },
      validate: { type: "boolean" },
      "analysis-only": { type: "boolean" },
      help: { type: "boolean", short: "h" },
    },
    allowPositionals: false,
  });

  if (values.help) {
    console.log(usage());
    process.exit(0);
  }

  if (!values.idl && !values.input) {
    console.error("ERROR: provide at least one of --idl or --input");
    console.log(usage());
    process.exit(1);
  }

  const analysisDir = resolveCliPath(
    values["analysis-dir"] || "/tmp/leanstral-analysis",
    invocationCwd
  );
  const outputDir = resolveCliPath(
    values["output-dir"] || "/tmp/leanstral-solana-proofs",
    invocationCwd
  );
  const topK = parseInt(values["top-k"] || "3", 10);
  const repairRounds = parseInt(values["repair-rounds"] || "1", 10);
  const tests = (values.tests || []).map((testFile) =>
    resolveCliPath(testFile, invocationCwd)
  );
  const idlPath = values.idl
    ? resolveCliPath(values.idl, invocationCwd)
    : undefined;
  const inputPath = values.input
    ? resolveCliPath(values.input, invocationCwd)
    : undefined;
  const projectRoot = deriveProjectRoot([idlPath || "", inputPath || "", ...tests]);
  const validationWorkspace = projectRoot
    ? join(projectRoot, ".leanstral", "validation-workspace")
    : undefined;

  await mkdir(analysisDir, { recursive: true });
  await mkdir(outputDir, { recursive: true });

  const analyzerArgs = [
    "run",
    "--manifest-path",
    "tools/anchor-ir/Cargo.toml",
    "--",
  ];
  if (idlPath) {
    analyzerArgs.push("--idl", idlPath);
  }
  if (inputPath) {
    analyzerArgs.push("--input", inputPath);
  }
  for (const testFile of tests) {
    analyzerArgs.push("--tests", testFile);
  }
  analyzerArgs.push("--output-dir", analysisDir);

  console.error("Analyzing Solana project...");
  const analyzeResult = await runCommand("cargo", analyzerArgs, REPO_ROOT);
  if (analyzeResult.code !== 0) {
    process.stderr.write(analyzeResult.stderr);
    process.exit(analyzeResult.code ?? 1);
  }

  const analysis = JSON.parse(
    await readFile(join(analysisDir, "analysis.json"), "utf-8")
  ) as AnalysisIr;
  const ranked = selectCandidates(analysis.property_candidates, topK);

  console.log(
    JSON.stringify(
      {
        analysisDir,
        projectRoot,
        selectedCandidates: ranked.map((candidate) => ({
          id: candidate.id,
          category: candidate.category,
          confidence: candidate.confidence,
          title: candidate.title,
          promptFile: join(analysisDir, `${candidate.id}.prompt.txt`),
          validationWorkspace,
        })),
      },
      null,
      2
    )
  );

  if (values["analysis-only"]) {
    return;
  }

  for (const candidate of ranked) {
    const candidateOutputDir = join(outputDir, candidate.id);
    await mkdir(candidateOutputDir, { recursive: true });
    const promptFile = join(analysisDir, `${candidate.id}.prompt.txt`);

    console.error(`Generating proof for ${candidate.id}...`);
    const generationResult = await runLeanstralGeneration(
      promptFile,
      candidateOutputDir,
      values,
      validationWorkspace
    );
    process.stderr.write(generationResult.stderr);
    if (generationResult.code !== 0) {
      console.error(`Generation failed for ${candidate.id}`);
      continue;
    }

    await attemptRepairs(
      candidate.id,
      candidateOutputDir,
      promptFile,
      repairRounds,
      values,
      validationWorkspace
    );
  }
}

main().catch((error) => {
  console.error("Unexpected error:", error);
  process.exit(1);
});
