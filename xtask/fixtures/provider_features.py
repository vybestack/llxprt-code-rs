#!/usr/bin/env python3
"""Resolved provider graph and compile-surface adversarial cases, driven by xtask."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    with tempfile.TemporaryDirectory(prefix="llxprt-provider-features.") as temporary:
        tmp = Path(temporary)
        (tmp / "bin").mkdir()
        cargo = tmp / "bin/cargo"
        cargo.write_text("#!/usr/bin/env python3\nimport os,pathlib\nprint(pathlib.Path(os.environ['LLXPRT_TEST_METADATA']).read_text())\n")
        cargo.chmod(0o755)
        for name, features, duplicate, accepted in [
            ("openai", ["openai"], False, True),
            ("default", ["default", "anthropic", "google", "openai"], False, False),
            ("duplicate", ["openai"], True, False),
        ]:
            packages = [{"id": "provider-a", "name": "serdes-ai-providers"}]
            nodes = [{"id": "provider-a", "features": features}]
            if duplicate:
                packages.append({"id": "provider-b", "name": "serdes-ai-providers"})
                nodes.append({"id": "provider-b", "features": ["openai"]})
            metadata = tmp / f"{name}.json"
            metadata.write_text(json.dumps({"packages": packages, "resolve": {"nodes": nodes}}))
            result = subprocess.run(["python3", ROOT / "scripts/verify-provider-features.py"], env={**os.environ, "PATH": f"{tmp}/bin:{os.environ['PATH']}", "LLXPRT_TEST_METADATA": str(metadata)}, capture_output=True)
            assert (result.returncode == 0) == accepted, (name, result.stderr)
        spec = importlib.util.spec_from_file_location("provider_gate", ROOT / "scripts/verify-provider-features.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        source = ROOT / "vendor/serdes-ai-providers"
        for name in ["alternate-feature", "alternate-module", "alternate-registration"]:
            fixture = tmp / name / "vendor/serdes-ai-providers"
            (fixture / "src").mkdir(parents=True)
            shutil.copy2(source / "Cargo.toml", fixture / "Cargo.toml")
            shutil.copy2(source / "src/lib.rs", fixture / "src/lib.rs")
            if name == "alternate-feature":
                path = fixture / "Cargo.toml"
                path.write_text(path.read_text().replace("[features]\n", "[features]\nanthropic = []\n", 1))
            else:
                path = fixture / "src/lib.rs"
                path.write_text(path.read_text() + ("\nmod anthropic;\n" if name == "alternate-module" else "\nfn decoy() { let _ = AnthropicProvider::from_env(); }\n"))
            module.ROOT = fixture.parents[1]
            try:
                module.verify_compile_surface()
            except SystemExit:
                continue
            raise AssertionError(f"provider compile-surface gate accepted {name}")
    print("provider feature adversarial tests passed")


if __name__ == "__main__":
    main()
