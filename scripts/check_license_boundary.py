#!/usr/bin/env python3
"""Validate nab's mixed MIT / PolyForm Noncommercial licensing boundary."""

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
SPDX = "SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0"

EE_PATHS = (
    Path("src/auth"),
    Path("src/fingerprint"),
    Path("src/waf"),
    Path("src/site"),
    Path("src/security"),
    Path("crates/nab-yara-engine"),
)

EE_FILES = (Path("examples/scan_html.rs"),)

COMMENTABLE_EXTENSIONS = {".rs", ".toml", ".yar"}
NON_COMMENTABLE_EXTENSIONS = {".json"}


def rel(path: Path) -> Path:
    return path.relative_to(ROOT)


def is_under(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
    except ValueError:
        return False
    return True


def first_text(path: Path, max_lines: int = 8) -> str:
    lines: list[str] = []
    with path.open("r", encoding="utf-8") as handle:
        for _ in range(max_lines):
            line = handle.readline()
            if not line:
                break
            lines.append(line)
    return "".join(lines)


def check_required_files(errors: list[str]) -> None:
    required = [
        "LICENSE",
        "LICENSE-MIT",
        "LICENSE-EE.md",
        "NOTICE",
        "CITATION.cff",
        "CONTRIBUTING.md",
    ]
    for name in required:
        if not (ROOT / name).is_file():
            errors.append(f"missing required licensing file: {name}")

    checks = {
        "LICENSE": "mixed per-file licensing",
        "LICENSE-MIT": "MIT License",
        "LICENSE-EE.md": "Required Notice:",
        "NOTICE": "Required Notice:",
        "CONTRIBUTING.md": "separate commercial licenses",
    }
    for name, needle in checks.items():
        path = ROOT / name
        if path.is_file() and needle not in path.read_text(encoding="utf-8"):
            errors.append(f"{name} must contain {needle!r}")


def check_ee_paths(errors: list[str]) -> None:
    for ee_file in EE_FILES:
        path = ROOT / ee_file
        if not path.is_file():
            errors.append(f"missing EE file listed in license policy: {ee_file}")
            continue
        if path.suffix not in COMMENTABLE_EXTENSIONS:
            errors.append(f"{ee_file} must use a commentable file type or move under EE path scope")
        elif SPDX not in first_text(path):
            errors.append(f"{ee_file} must carry {SPDX}")

    for ee_path in EE_PATHS:
        abs_path = ROOT / ee_path
        if not abs_path.exists():
            errors.append(f"missing EE path listed in license policy: {ee_path}")
            continue

        for path in sorted(abs_path.rglob("*")):
            if not path.is_file():
                continue

            relative = rel(path)
            suffix = path.suffix
            if suffix in COMMENTABLE_EXTENSIONS:
                if SPDX not in first_text(path):
                    errors.append(f"{relative} must carry {SPDX}")
            elif suffix in NON_COMMENTABLE_EXTENSIONS:
                continue
            else:
                errors.append(
                    f"{relative} has unsupported EE file extension {suffix!r}; "
                    "add an SPDX marker or extend check_license_boundary.py"
                )


def check_no_polyform_outside_ee(errors: list[str]) -> None:
    for path in ROOT.rglob("*"):
        if not path.is_file():
            continue
        if any(part in {".git", "target"} for part in path.parts):
            continue
        if path.suffix not in COMMENTABLE_EXTENSIONS:
            continue
        relative = rel(path)
        if any(is_under(relative, ee_path) for ee_path in EE_PATHS) or relative in EE_FILES:
            continue
        if SPDX in first_text(path):
            errors.append(f"{relative} has PolyForm SPDX outside EE path scope")


def main() -> int:
    errors: list[str] = []
    check_required_files(errors)
    check_ee_paths(errors)
    check_no_polyform_outside_ee(errors)

    if errors:
        print("license boundary check failed:")
        for error in errors:
            print(f"- {error}")
        return 1

    print("license boundary check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
