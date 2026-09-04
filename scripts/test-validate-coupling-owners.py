#!/usr/bin/env python3
"""Controlled no-network regressions for validate-coupling-owners.py."""

import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest.mock
import urllib.error

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "validate-coupling-owners.py"
TMP = ROOT / "tmp" / "verify65-r9" / "owner-fixtures"


def run_case(name, responses=None, token=None, malformed_fixture=None):
    case = TMP / name
    # Self-contained runs: drop any leftovers from a previous (possibly
    # concurrent) run so stale fixtures never leak into this one.
    shutil.rmtree(case, ignore_errors=True)
    case.mkdir(parents=True, exist_ok=True)
    ledger = case / "ledger.tsv"
    # 17 is deliberately repeated to prove deterministic owner deduplication.
    ledger.write_text("a\tb\t17\nb\tc\t23\nc\td\t17\n", encoding="utf-8")
    fixture = case / "responses.json"
    if malformed_fixture is not None:
        fixture.write_text(malformed_fixture, encoding="utf-8")
    elif responses is not None:
        fixture.write_text(json.dumps(responses), encoding="utf-8")

    command = [sys.executable, str(SCRIPT), "--ledger", str(ledger)]
    if responses is not None or malformed_fixture is not None:
        command += ["--fixture", str(fixture)]
    environment = os.environ.copy()
    environment.pop("GITHUB_TOKEN", None)
    if token is not None:
        environment["GITHUB_TOKEN"] = token
    return subprocess.run(command, text=True, capture_output=True, env=environment)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


PRODUCTION = None


class FakeResponse:
    """Minimal stand-in for the http.client response object urlopen yields."""

    def __init__(self, body=None, status=200):
        self.status = status
        payload = {"state": "open"} if body is None else body
        self.body = json.dumps(payload).encode("utf-8")

    def read(self, amount=-1):
        return self.body

    def __enter__(self):
        return self

    def __exit__(self, *details):
        return False


def load_production():
    """Import the real validator so tests exercise the production request boundary."""
    global PRODUCTION
    if PRODUCTION is None:
        spec = importlib.util.spec_from_file_location("validate_coupling_owners", SCRIPT)
        PRODUCTION = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(PRODUCTION)
    return PRODUCTION


def invoke_production(ledger_text, outcome):
    """Run main() over the real ledger/HTTP boundary with urlopen mocked out."""
    case = TMP / "boundary"
    shutil.rmtree(case, ignore_errors=True)
    case.mkdir(parents=True, exist_ok=True)
    ledger = case / "ledger.tsv"
    ledger.write_text(ledger_text, encoding="utf-8")
    module = load_production()
    stdout, stderr = io.StringIO(), io.StringIO()
    argv = ["validate-coupling-owners.py", "--ledger", str(ledger)]
    with unittest.mock.patch.object(sys, "argv", argv), \
            unittest.mock.patch.dict(os.environ, {"GITHUB_TOKEN": "test-token"}), \
            unittest.mock.patch("urllib.request.urlopen", **outcome) as opened, \
            unittest.mock.patch("sys.stdout", stdout), \
            unittest.mock.patch("sys.stderr", stderr):
        code = module.main()
    return code, stdout.getvalue(), stderr.getvalue(), opened


def check_request_boundary():
    code, stdout, stderr, opened = invoke_production(
        "adapter\tcli\t17\n", {"return_value": FakeResponse()}
    )
    require(code == 0, stderr)
    require(stdout == "17\topen\n", repr(stdout))
    opened.assert_called_once()
    call = opened.call_args
    require(call.kwargs == {"timeout": 15}, repr(call.kwargs))
    request = call.args[0]
    expected = "https://api.github.com/repos/vybestack/llxprt-code-rs/issues/17"
    require(request.full_url == expected, request.full_url)
    headers = {name.lower(): value for name, value in request.headers.items()}
    require(headers.get("accept") == "application/vnd.github+json", repr(headers))
    require(headers.get("authorization") == "Bearer test-token", repr(headers))
    require(headers.get("x-github-api-version") == "2022-11-28", repr(headers))
    require(headers.get("user-agent") == "llxprt-coupling-owner-validator", repr(headers))


def check_fail_closed_boundary():
    failures = [
        (
            "http-404",
            {"side_effect": urllib.error.HTTPError("url", 404, "Not Found", None, io.BytesIO(b"{}"))},
            "missing",
        ),
        (
            "http-500",
            {"side_effect": urllib.error.HTTPError("url", 500, "Server Error", None, io.BytesIO(b"{}"))},
            "HTTP 500",
        ),
        ("url-error", {"side_effect": urllib.error.URLError("name resolution failed")}, "lookup failed"),
        ("non-200-status", {"return_value": FakeResponse(status=202)}, "HTTP 202"),
        ("not-a-dict", {"return_value": FakeResponse(body=[])}, "malformed"),
        (
            "pull-request",
            {"return_value": FakeResponse(body={"state": "open", "pull_request": {}})},
            "pull request",
        ),
        ("closed-state", {"return_value": FakeResponse(body={"state": "closed"})}, "not open"),
    ]
    for name, outcome, diagnostic in failures:
        code, stdout, stderr, _ = invoke_production("adapter\tcli\t17\n", outcome)
        require(code != 0, f"{name} unexpectedly passed")
        require(stdout == "", f"{name} emitted stdout: {stdout!r}")
        require(diagnostic in stderr, f"{name}: {stderr}")


def check_ledger_parsing():
    module = load_production()
    case = TMP / "parsing"
    shutil.rmtree(case, ignore_errors=True)
    case.mkdir(parents=True, exist_ok=True)
    ledger = case / "ledger.tsv"
    real_format = (
        "# Coupling debt burn-down ledger. Entries may only be removed by the ordinary gate.\n"
        "# FROM<TAB>TO<TAB>OPEN_GITHUB_ISSUE\n"
        "\n"
        "adapter\tcli\t69\n"
        "session\tagent\t70\n"
        "session\tmodel_api\t71\n"
        "profile\tmodel_api\t71\n"
    )
    ledger.write_text(real_format, encoding="utf-8")
    require(module.ledger_owners(ledger) == [69, 70, 71], repr(module.ledger_owners(ledger)))

    ledger.write_text("alpha\tbeta\t17\nalpha\tbeta\t17\n", encoding="utf-8")
    require(module.ledger_owners(ledger) == [17], "duplicate owners did not dedupe")

    broken = [
        ("two fields", "alpha\tbeta\n"),
        ("comment-looking issue", "alpha\tbeta\t#x\n"),
        ("non-numeric issue", "alpha\tbeta\tsoon\n"),
        ("zero issue", "alpha\tbeta\t0\n"),
        ("negative issue", "alpha\tbeta\t-1\n"),
    ]
    for name, row in broken:
        ledger.write_text(row, encoding="utf-8")
        try:
            module.ledger_owners(ledger)
        except ValueError as error:
            require(str(error).startswith(f"{ledger}:1:"), f"{name}: {error}")
        else:
            raise AssertionError(f"{name} unexpectedly parsed")


def main():
    valid = {"17": {"state": "open"}, "23": {"state": "open"}}
    result = run_case("all-open-deduplicated", valid)
    require(result.returncode == 0, result.stderr)
    require(result.stdout == "17\topen\n23\topen\n", repr(result.stdout))
    require(result.stderr == "", repr(result.stderr))

    cases = {
        "closed": ({"17": {"state": "open"}, "23": {"state": "closed"}}, "not open"),
        "missing": ({"17": {"state": "open"}}, "missing"),
        "pull-request": (
            {"17": {"state": "open", "pull_request": {}}, "23": {"state": "open"}},
            "pull request",
        ),
        "malformed-response": (
            {"17": {"state": "OPEN"}, "23": {"state": "open"}},
            "malformed response",
        ),
    }
    for name, (responses, diagnostic) in cases.items():
        result = run_case(name, responses)
        require(result.returncode != 0, f"{name} unexpectedly passed")
        require(result.stdout == "", f"{name} emitted stdout: {result.stdout!r}")
        require(diagnostic in result.stderr, result.stderr)

    result = run_case("malformed-fixture", malformed_fixture="{")
    require(result.returncode != 0, "malformed fixture unexpectedly passed")
    require(result.stdout == "", repr(result.stdout))

    # Omitting fixture mode must fail before attempting any network request.
    result = run_case("no-token")
    require(result.returncode != 0, "missing token unexpectedly passed")
    require(result.stdout == "", repr(result.stdout))
    require("GITHUB_TOKEN is required" in result.stderr, result.stderr)

    check_request_boundary()
    check_fail_closed_boundary()
    check_ledger_parsing()

    print("validate-coupling-owners controlled tests: ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
