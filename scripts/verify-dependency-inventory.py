#!/usr/bin/env python3
import json
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
INVENTORY = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "THIRD_PARTY_LICENSES/DEPENDENCIES.md"
MANIFESTS = [(ROOT / "Cargo.toml", "root"), (ROOT / "xtask/Cargo.toml", "xtask")]
HEADING = "## Direct dependencies (from both first-party manifests, locked in their lockfiles)"
KIND_ORDER = {"runtime": 0, "runtime (unix-tgt)": 1, "dev-only": 2, "xtask runtime": 3}


def metadata(manifest: pathlib.Path) -> dict:
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
            str(manifest),
        ],
        cwd=ROOT,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    return json.loads(result.stdout)


def dependency_kind(scope: str, dependency: dict) -> str:
    if scope == "xtask":
        return "xtask runtime"
    if dependency["kind"] == "dev":
        return "dev-only"
    if dependency["target"] == "cfg(unix)":
        return "runtime (unix-tgt)"
    if dependency["kind"] is None and dependency["target"] is None:
        return "runtime"
    raise RuntimeError(f"undocumented dependency scope: {dependency}")


def expected_records() -> dict[str, dict[str, set[str]]]:
    records: dict[str, dict[str, set[str]]] = {}
    for manifest, scope in MANIFESTS:
        data = metadata(manifest)
        manifest_path = str(manifest.resolve())
        package = next(item for item in data["packages"] if item["manifest_path"] == manifest_path)
        node = next(item for item in data["resolve"]["nodes"] if item["id"] == package["id"])
        resolved = {item["name"]: item["pkg"] for item in node["deps"]}
        packages = {item["id"]: item for item in data["packages"]}
        for dependency in package["dependencies"]:
            name = dependency["name"]
            selected = packages[resolved[name.replace("-", "_")]]
            source = "path" if dependency["source"] is None else "registry"
            record = records.setdefault(
                name,
                {"versions": set(), "kinds": set(), "licenses": set(), "sources": set()},
            )
            record["versions"].add(selected["version"])
            record["kinds"].add(dependency_kind(scope, dependency))
            record["licenses"].add(selected["license"] or "")
            record["sources"].add(source)
    return records


def documented_records() -> dict[str, tuple[str, str, str, str]]:
    records = {}
    in_table = False
    for line in INVENTORY.read_text(encoding="utf-8").splitlines():
        if line == HEADING:
            in_table = True
            continue
        if in_table and line.startswith("## "):
            break
        if not in_table or not line.startswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) != 5 or cells[0] == "Crate" or set(cells[0]) == {"-"}:
            continue
        source = "path" if cells[4].startswith("vendored ") else "registry"
        records[cells[0].split()[0]] = (cells[1], cells[2], cells[3], source)
    return records


def one(values: set[str], field: str, name: str) -> str:
    if len(values) != 1:
        raise RuntimeError(f"{name} resolves to multiple {field}: {sorted(values)}")
    return next(iter(values))


def main() -> None:
    expected = {}
    for name, record in expected_records().items():
        kinds = " + ".join(sorted(record["kinds"], key=KIND_ORDER.__getitem__))
        expected[name] = (
            one(record["versions"], "versions", name),
            kinds,
            one(record["licenses"], "licenses", name),
            one(record["sources"], "sources", name),
        )
    documented = documented_records()
    if documented != expected:
        missing = sorted(expected.keys() - documented.keys())
        extra = sorted(documented.keys() - expected.keys())
        changed = sorted(
            name
            for name in expected.keys() & documented.keys()
            if expected[name] != documented[name]
        )
        details = [
            f"{name}: expected={expected[name]!r}, documented={documented[name]!r}"
            for name in changed
        ]
        raise SystemExit(
            "direct dependency inventory mismatch; "
            f"missing={missing}, extra={extra}, changed={details}"
        )


if __name__ == "__main__":
    main()
