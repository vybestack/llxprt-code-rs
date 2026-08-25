#!/usr/bin/env python3
"""Derive release names from Cargo metadata and validate an exact release tag."""

import argparse
import json
import subprocess
import sys


def package_version() -> str:
    result = subprocess.run(
        [
            "cargo",
            "+1.88.0",
            "metadata",
            "--offline",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
        ],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    metadata = json.loads(result.stdout)
    packages = [item for item in metadata["packages"] if item["name"] == "llxprt-code-rs"]
    if len(packages) != 1:
        raise RuntimeError("Cargo metadata did not contain exactly one llxprt-code-rs package")
    return packages[0]["version"]


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag")
    parser.add_argument("--github-output")
    parser.add_argument("--value", choices=("version", "tag", "archive", "sidecar"))
    args = parser.parse_args()
    if args.github_output and args.value:
        raise RuntimeError("--github-output and --value cannot be combined")
    version = package_version()
    expected_tag = f"v{version}"
    if args.tag is not None and args.tag != expected_tag:
        raise RuntimeError(f"release tag must be exactly {expected_tag}")
    archive = f"llxprt-code-rs-{version}-source.tar.gz"
    values = {
        "version": version,
        "tag": expected_tag,
        "archive": archive,
        "sidecar": f"{archive}.sha256",
    }
    if args.github_output:
        with open(args.github_output, "a", encoding="utf-8") as output:
            for key, value in values.items():
                output.write(f"{key}={value}\n")
    elif args.value:
        sys.stdout.write(f"{values[args.value]}\n")
    else:
        json.dump(values, sys.stdout, sort_keys=True)
        sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        raise SystemExit(str(error)) from error
