#!/usr/bin/env python3
"""
call_leanstral.py — Call Mistral's Leanstral model for Lean 4 proof generation.

Sends a prompt to the labs-leanstral-2603 endpoint and returns N independent
completions (pass@N) so the caller can pick the best proof.

Usage:
    python3 call_leanstral.py \
        --prompt-file /tmp/leanstral_prompt.txt \
        --output-dir /tmp/leanstral_output \
        --passes 4 \
        --temperature 0.6

    # Or pipe a prompt directly:
    echo "Prove that addition is commutative in Lean 4" | \
        python3 call_leanstral.py --output-dir /tmp/out --passes 2

Environment:
    MISTRAL_API_KEY — required. Get one free at https://console.mistral.ai

Output:
    Creates output-dir/ with:
        completion_0.lean  — first completion
        completion_1.lean  — second completion
        ...
        metadata.json      — timing, token usage, model info per completion
"""

import argparse
import json
import os
import sys
import time
from pathlib import Path

try:
    import urllib.request
    import urllib.error
except ImportError:
    pass  # Should always be available in Python 3

API_URL = "https://api.mistral.ai/v1/chat/completions"
MODEL = "labs-leanstral-2603"
DEFAULT_PASSES = 4
DEFAULT_TEMPERATURE = 0.6
DEFAULT_MAX_TOKENS = 16384
TIMEOUT_SECONDS = 180  # 3 minutes per request — proofs can be long
MAX_RETRIES = 3
BACKOFF_BASE = 2  # exponential backoff base in seconds

SYSTEM_PROMPT = """You are Leanstral, an expert Lean 4 proof engineer. When given a program or specification:

1. Define the relevant types and functions in Lean 4 that faithfully model the program.
2. State the theorem or property formally as a Lean 4 theorem.
3. Prove the theorem using appropriate tactics (simp, omega, induction, cases, etc.).
4. If you cannot complete a sub-proof, use `sorry` and explain what additional lemmas would be needed.
5. After the proof, briefly explain your proof strategy.

Always produce valid Lean 4 syntax. Use `import Mathlib` only if needed. Prefer self-contained proofs where possible."""


def call_api(prompt: str, api_key: str, temperature: float, max_tokens: int) -> dict:
    """Make a single API call to Leanstral. Returns the parsed JSON response."""
    payload = {
        "model": MODEL,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": prompt},
        ],
        "temperature": temperature,
        "max_tokens": max_tokens,
    }

    headers = {
        "Content-Type": "application/json",
        "Authorization": f"Bearer {api_key}",
    }

    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(API_URL, data=data, headers=headers, method="POST")

    for attempt in range(MAX_RETRIES):
        try:
            start = time.time()
            with urllib.request.urlopen(req, timeout=TIMEOUT_SECONDS) as resp:
                elapsed = time.time() - start
                body = json.loads(resp.read().decode("utf-8"))
                body["_elapsed_seconds"] = round(elapsed, 2)
                return body
        except urllib.error.HTTPError as e:
            if e.code == 429:
                wait = BACKOFF_BASE ** (attempt + 1)
                print(
                    f"  Rate limited (429). Retrying in {wait}s... (attempt {attempt + 1}/{MAX_RETRIES})",
                    file=sys.stderr,
                )
                time.sleep(wait)
                continue
            elif e.code == 401:
                print(
                    "ERROR: Invalid or missing MISTRAL_API_KEY. "
                    "Get one at https://console.mistral.ai",
                    file=sys.stderr,
                )
                sys.exit(1)
            else:
                error_body = e.read().decode("utf-8") if e.fp else ""
                print(
                    f"ERROR: HTTP {e.code}: {error_body}",
                    file=sys.stderr,
                )
                if attempt < MAX_RETRIES - 1:
                    time.sleep(BACKOFF_BASE ** (attempt + 1))
                    continue
                sys.exit(1)
        except Exception as e:
            print(f"ERROR: {e}", file=sys.stderr)
            if attempt < MAX_RETRIES - 1:
                time.sleep(BACKOFF_BASE ** (attempt + 1))
                continue
            sys.exit(1)

    print("ERROR: All retries exhausted.", file=sys.stderr)
    sys.exit(1)


def extract_lean_code(content: str) -> str:
    """Extract Lean code from a response, handling markdown code fences."""
    # If the response has ```lean ... ``` blocks, extract them
    import re

    blocks = re.findall(r"```lean4?\s*\n(.*?)```", content, re.DOTALL)
    if blocks:
        return "\n\n".join(blocks)

    # If no code fences, return the whole content (Leanstral often returns raw Lean)
    return content


def count_sorry(code: str) -> int:
    """Count occurrences of 'sorry' in Lean code (indicates incomplete proofs)."""
    import re

    return len(re.findall(r"\bsorry\b", code))


def main():
    parser = argparse.ArgumentParser(description="Call Leanstral for Lean 4 proof generation")
    parser.add_argument(
        "--prompt-file",
        type=str,
        help="Path to a text file containing the prompt. If omitted, reads from stdin.",
    )
    parser.add_argument(
        "--output-dir",
        type=str,
        required=True,
        help="Directory to write completions and metadata to.",
    )
    parser.add_argument(
        "--passes",
        type=int,
        default=DEFAULT_PASSES,
        help=f"Number of independent completions (pass@N). Default: {DEFAULT_PASSES}",
    )
    parser.add_argument(
        "--temperature",
        type=float,
        default=DEFAULT_TEMPERATURE,
        help=f"Sampling temperature. Default: {DEFAULT_TEMPERATURE}",
    )
    parser.add_argument(
        "--max-tokens",
        type=int,
        default=DEFAULT_MAX_TOKENS,
        help=f"Max tokens per completion. Default: {DEFAULT_MAX_TOKENS}",
    )
    args = parser.parse_args()

    # Read API key
    api_key = os.environ.get("MISTRAL_API_KEY")
    if not api_key:
        print(
            "ERROR: MISTRAL_API_KEY environment variable is not set.\n"
            "Get a free key at https://console.mistral.ai\n"
            "Then run: export MISTRAL_API_KEY=your_key_here",
            file=sys.stderr,
        )
        sys.exit(1)

    # Read prompt
    if args.prompt_file:
        prompt = Path(args.prompt_file).read_text(encoding="utf-8")
    else:
        if sys.stdin.isatty():
            print("Reading prompt from stdin (Ctrl+D to finish):", file=sys.stderr)
        prompt = sys.stdin.read()

    if not prompt.strip():
        print("ERROR: Empty prompt.", file=sys.stderr)
        sys.exit(1)

    # Create output directory
    out_dir = Path(args.output_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    # Save the prompt for reference
    (out_dir / "prompt.txt").write_text(prompt, encoding="utf-8")

    print(f"Calling Leanstral ({MODEL}) with pass@{args.passes}...", file=sys.stderr)

    metadata = {
        "model": MODEL,
        "passes": args.passes,
        "temperature": args.temperature,
        "max_tokens": args.max_tokens,
        "completions": [],
    }

    best_idx = 0
    best_sorry_count = float("inf")

    for i in range(args.passes):
        print(f"  Pass {i + 1}/{args.passes}...", file=sys.stderr, end=" ", flush=True)
        response = call_api(prompt, api_key, args.temperature, args.max_tokens)

        # Extract the assistant's message
        content = response.get("choices", [{}])[0].get("message", {}).get("content", "")
        elapsed = response.get("_elapsed_seconds", 0)
        usage = response.get("usage", {})

        # Extract Lean code and count sorry markers
        lean_code = extract_lean_code(content)
        sorry_count = count_sorry(lean_code)

        print(
            f"done ({elapsed}s, {usage.get('completion_tokens', '?')} tokens, {sorry_count} sorry)",
            file=sys.stderr,
        )

        # Save the raw completion (full response including explanations)
        (out_dir / f"completion_{i}_raw.txt").write_text(content, encoding="utf-8")
        # Save just the extracted Lean code
        (out_dir / f"completion_{i}.lean").write_text(lean_code, encoding="utf-8")

        # Track metadata
        metadata["completions"].append(
            {
                "index": i,
                "sorry_count": sorry_count,
                "elapsed_seconds": elapsed,
                "prompt_tokens": usage.get("prompt_tokens", 0),
                "completion_tokens": usage.get("completion_tokens", 0),
                "total_tokens": usage.get("total_tokens", 0),
                "finish_reason": response.get("choices", [{}])[0].get("finish_reason", "unknown"),
            }
        )

        # Track best completion (fewest sorry markers)
        if sorry_count < best_sorry_count:
            best_sorry_count = sorry_count
            best_idx = i

    metadata["best_completion_index"] = best_idx
    metadata["best_sorry_count"] = best_sorry_count

    # Save metadata
    (out_dir / "metadata.json").write_text(
        json.dumps(metadata, indent=2), encoding="utf-8"
    )

    # Copy best completion to a convenient location
    best_lean = (out_dir / f"completion_{best_idx}.lean").read_text(encoding="utf-8")
    (out_dir / "best.lean").write_text(best_lean, encoding="utf-8")

    print(f"\nResults saved to {out_dir}/", file=sys.stderr)
    print(f"Best completion: completion_{best_idx}.lean ({best_sorry_count} sorry)", file=sys.stderr)

    # Print the best completion to stdout for easy piping
    print(best_lean)


if __name__ == "__main__":
    main()
