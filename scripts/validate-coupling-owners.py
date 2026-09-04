#!/usr/bin/env python3
"""Fail-closed validation that every coupling-ledger owner is an open GitHub issue."""

import argparse
import json
import os
import sys
import urllib.error
import urllib.request

REPOSITORY = "vybestack/llxprt-code-rs"
API_URL = "https://api.github.com"


def ledger_owners(path):
    owners = set()
    with open(path, encoding="utf-8") as source:
        for number, line in enumerate(source, 1):
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            fields = line.rstrip("\n").split("\t")
            if len(fields) != 3:
                raise ValueError(f"{path}:{number}: expected FROM<TAB>TO<TAB>ISSUE")
            try:
                issue = int(fields[2])
            except ValueError as error:
                raise ValueError(f"{path}:{number}: issue must be a positive integer") from error
            if issue <= 0:
                raise ValueError(f"{path}:{number}: issue must be positive")
            owners.add(issue)
    return sorted(owners)


def classify_response(issue, response):
    """Classify one issues-endpoint response; fixtures use this exact boundary too."""
    if not isinstance(response, dict):
        raise ValueError(f"issue #{issue}: malformed response")
    if "pull_request" in response:
        raise ValueError(f"issue #{issue}: owner is a pull request, not an issue")
    state = response.get("state")
    if state not in ("open", "closed"):
        raise ValueError(f"issue #{issue}: malformed response state")
    if state != "open":
        raise ValueError(f"issue #{issue}: owner is not open (state={state})")


def fixture_responses(path):
    with open(path, encoding="utf-8") as source:
        value = json.load(source)
    if not isinstance(value, dict):
        raise ValueError("fixture must be a JSON object keyed by issue number")
    return value


def github_response(issue, token):
    request = urllib.request.Request(
        f"{API_URL}/repos/{REPOSITORY}/issues/{issue}",
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": "llxprt-coupling-owner-validator",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            if response.status != 200:
                raise ValueError(f"issue #{issue}: GitHub returned HTTP {response.status}")
            return json.load(response)
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise ValueError(f"issue #{issue}: owner issue is missing") from error
        raise ValueError(f"issue #{issue}: GitHub returned HTTP {error.code}") from error
    except urllib.error.URLError as error:
        raise ValueError(f"issue #{issue}: GitHub lookup failed: {error.reason}") from error


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ledger", required=True)
    parser.add_argument(
        "--fixture",
        help="controlled no-network issues-endpoint responses (tests only)",
    )
    args = parser.parse_args()

    try:
        owners = ledger_owners(args.ledger)
        responses = fixture_responses(args.fixture) if args.fixture else None
        token = os.environ.get("GITHUB_TOKEN")
        if responses is None and not token:
            raise ValueError("GITHUB_TOKEN is required for owner validation")

        for issue in owners:
            if responses is not None:
                key = str(issue)
                if key not in responses:
                    raise ValueError(f"issue #{issue}: owner issue is missing")
                response = responses[key]
            else:
                response = github_response(issue, token)
            classify_response(issue, response)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"coupling owner validation failed: {error}", file=sys.stderr)
        return 1

    for issue in owners:
        print(f"{issue}\topen")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
