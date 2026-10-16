#!/usr/bin/env python3
"""Static LayerManifest gate for OriEra (manifests/*.yaml).

Checks what CI can check without running code:
  1. every manifest parses, has kind LayerManifest and the required fields
  2. layer IDs are unique
  3. every spec.requires id is provided by some layer's spec.provides;
     non-optional requires must resolve to a `stable` contract
  4. contract references resolve:
       - `rust:` prefixed refs are code references, skipped here
       - `proto/<path>` refs must exist under contracts/

Exit code 1 with a report on any violation. Stdlib + PyYAML only.
"""
from __future__ import annotations

import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
MANIFEST_DIR = ROOT / "manifests"
CONTRACTS_DIR = ROOT / "contracts"

REQUIRED_FIELDS = ("metadata", "spec")
REQUIRED_SPEC_FIELDS = ("provides",)


def load_manifests() -> list[tuple[Path, dict]]:
    files = sorted(MANIFEST_DIR.glob("*.yaml"))
    if not files:
        raise SystemExit(f"no manifests found under {MANIFEST_DIR}")
    out = []
    for path in files:
        try:
            doc = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as exc:
            raise SystemExit(f"{path.name}: invalid YAML\n{exc}")
        out.append((path, doc or {}))
    return out


def main() -> int:
    errors: list[str] = []
    manifests = load_manifests()

    seen_layers: dict[str, str] = {}
    provides_pool: dict[str, tuple[str, str]] = {}  # id -> (file, stability)

    for path, doc in manifests:
        name = path.name
        for field in REQUIRED_FIELDS:
            if field not in doc:
                errors.append(f"{name}: missing required field `{field}`")
        if doc.get("kind") != "LayerManifest":
            errors.append(f"{name}: kind must be LayerManifest, got {doc.get('kind')!r}")
        meta = doc.get("metadata") or {}
        layer = meta.get("layer")
        if not layer:
            errors.append(f"{name}: metadata.layer is required")
        elif layer in seen_layers:
            errors.append(f"{name}: duplicate layer id {layer} (also in {seen_layers[layer]})")
        else:
            seen_layers[layer] = name

        spec = doc.get("spec") or {}
        for field in REQUIRED_SPEC_FIELDS:
            if field not in spec:
                errors.append(f"{name}: spec.{field} is required")

        for prov in spec.get("provides") or []:
            pid = prov.get("id")
            stab = prov.get("stability", "unstable")
            if not pid:
                errors.append(f"{name}: provides entry without id")
                continue
            if pid in provides_pool:
                other = provides_pool[pid][0]
                errors.append(f"{name}: provides id {pid} already declared in {other}")
            provides_pool[pid] = (name, stab)

            contract = prov.get("contract", "")
            if contract.startswith("rust:"):
                continue
            if contract.startswith("proto/"):
                if not (CONTRACTS_DIR / contract[len("proto/"):]).is_file():
                    errors.append(
                        f"{name}: provides {pid} references missing contract file "
                        f"{contract} (searched under contracts/)"
                    )
            elif contract:
                errors.append(
                    f"{name}: provides {pid} has unrecognized contract ref {contract!r} "
                    "(expected `rust:` or `proto/` form)"
                )

    for path, doc in manifests:
        name = path.name
        for req in (doc.get("spec") or {}).get("requires") or []:
            rid = req.get("id")
            if not rid:
                errors.append(f"{name}: requires entry without id")
                continue
            if rid not in provides_pool:
                errors.append(f"{name}: requires {rid} but no layer provides it")
                continue
            provider, stability = provides_pool[rid]
            if not req.get("optional", False) and stability != "stable":
                errors.append(
                    f"{name}: requires {rid} non-optionally, but {provider} "
                    f"declares it {stability!r} (must be stable)"
                )

    if errors:
        print("manifest gate FAILED:")
        for err in errors:
            print(f"  - {err}")
        return 1
    print(f"manifest gate OK: {len(manifests)} manifests, "
          f"{len(provides_pool)} provided contracts, all requires resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main())
