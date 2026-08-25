#!/usr/bin/env python3
"""Require the resolved SerdesAI provider registry to compile OpenAI alone."""

import json
import pathlib
import re
import subprocess
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent
EXPECTED = {"openai"}
EXPECTED_DECLARED = {"default": ["openai"], "full": ["openai"], "openai": []}
ALLOWED_MODULES = {"openai", "provider", "registry"}
FORBIDDEN_ACTIVE = re.compile(
    r"\b(?:Anthropic|Azure|Cohere|DeepSeek|Fireworks|Gateway|Google|Groq|Mistral|"
    r"OAuth|Ollama|OpenRouter|Together|VertexAI)Provider\b|\bmod\s+(?:compatible|gateway|oauth)\b"
)


def verify_compile_surface() -> None:
    provider_root = ROOT / "vendor" / "serdes-ai-providers"
    with (provider_root / "Cargo.toml").open("rb") as stream:
        declared = tomllib.load(stream).get("features")
    if declared != EXPECTED_DECLARED:
        raise SystemExit(
            "serdes-ai-providers must declare only OpenAI feature surfaces; "
            f"found {declared!r}"
        )
    source = (provider_root / "src" / "lib.rs").read_text()
    active = "\n".join(
        line for line in source.splitlines() if not line.lstrip().startswith("//")
    )
    modules = set(re.findall(r"^\s*(?:pub\s+)?mod\s+([A-Za-z0-9_]+)\s*;", active, re.M))
    if modules != ALLOWED_MODULES:
        raise SystemExit(
            "serdes-ai-providers active modules must be exactly "
            f"{sorted(ALLOWED_MODULES)}, found {sorted(modules)}"
        )
    match = FORBIDDEN_ACTIVE.search(active)
    if match:
        raise SystemExit(f"non-OpenAI provider compile surface remains active: {match.group(0)}")


def main() -> None:
    result = subprocess.run(
        [
            "cargo",
            "+1.88.0",
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
        ],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    metadata = json.loads(result.stdout)
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = [
        node
        for node in metadata["resolve"]["nodes"]
        if packages[node["id"]]["name"] == "serdes-ai-providers"
    ]
    if len(nodes) != 1:
        raise SystemExit(
            f"expected one resolved serdes-ai-providers package, found {len(nodes)}"
        )
    actual = set(nodes[0]["features"])
    if actual != EXPECTED:
        raise SystemExit(
            "resolved serdes-ai-providers features must be exactly "
            f"{sorted(EXPECTED)}, found {sorted(actual)}"
        )
    verify_compile_surface()
    print("resolved SerdesAI provider features and compile surface are OpenAI-only")


if __name__ == "__main__":
    main()
