#!/usr/bin/env python3
"""Dependency inventory mutation cases, driven by xtask."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    verifier = ROOT / "scripts/verify-dependency-inventory.py"
    subprocess.run(["python3", verifier], cwd=ROOT, check=True)
    source = (ROOT / "THIRD_PARTY_LICENSES/DEPENDENCIES.md").read_text()
    base = "| sha2                          | 0.10.9      | runtime       | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |"
    mac = "| security-framework (macOS)    | 3.7.0      | runtime (macos-tgt) | MIT OR Apache-2.0 | registry (locked in `Cargo.lock`) |"
    mutations = [
        ("name", base, base.replace("sha2", "sha3")),
        ("version", base, base.replace("0.10.9", "0.10.8")),
        ("kind", base, base.replace("runtime", "dev-only")),
        ("license", base, base.replace("MIT OR Apache-2.0", "MIT")),
        ("source", base, base.replace("registry (locked in `Cargo.lock`)", "vendored `vendor/sha2`")),
        ("macos-kind-label", mac, mac.replace("runtime (macos-tgt)", "runtime (macos)")),
        ("macos-wrong-kind", mac, mac.replace("runtime (macos-tgt)", "runtime (unix-tgt)")),
    ]
    with tempfile.TemporaryDirectory(prefix="llxprt-dependency-inventory-test.") as temporary:
        for label, old, replacement in mutations:
            assert source.count(old) == 1, "test dependency row was not unique"
            inventory = Path(temporary) / f"{label}.md"
            inventory.write_text(source.replace(old, replacement))
            result = subprocess.run(["python3", verifier, inventory], cwd=ROOT, capture_output=True)
            assert result.returncode != 0, f"dependency inventory accepted changed {label}"
    spec = importlib.util.spec_from_file_location("inventory", verifier)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    assert module.dependency_kind("root", {"kind": None, "target": 'cfg(target_os = "macos")'}) == "runtime (macos-tgt)"
    try:
        module.dependency_kind("root", {"kind": None, "target": 'cfg(target_os = "ios")'})
    except RuntimeError:
        pass
    else:
        raise AssertionError("an undocumented target kind was accepted")
    kinds = {"xtask runtime", "runtime (macos-tgt)", "dev-only", "runtime", "runtime (unix-tgt)"}
    assert " + ".join(sorted(kinds, key=module.KIND_ORDER.__getitem__)) == "runtime + runtime (unix-tgt) + runtime (macos-tgt) + dev-only + xtask runtime"
    print("dependency inventory regression tests passed")


if __name__ == "__main__":
    main()
