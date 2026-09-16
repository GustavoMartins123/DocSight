from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys
import tomllib
from typing import Any

from scripts.common import ROOT, ToolError, error_json, read_json, write_new


def _license_files(package: dict[str, Any]) -> list[Path]:
    manifest = Path(package['manifest_path'])
    if not manifest.is_absolute() or not manifest.is_file():
        raise ToolError('INVALID_CARGO_METADATA', 'Package manifest must exist at an absolute path')
    directory = manifest.parent.resolve()
    paths = set()
    declared = package.get('license_file')
    if declared is not None:
        if not isinstance(declared, str):
            raise ToolError('INVALID_LICENSE_PATH', 'Declared license file must be a path')
        license_path = directory / declared
        if license_path.is_symlink() or not license_path.is_file() or not license_path.resolve().is_relative_to(directory):
            raise ToolError('INVALID_LICENSE_PATH', 'Declared license file must remain inside the package')
        paths.add(license_path.resolve())
    inspected = 0
    for path in directory.iterdir():
        name = path.name.upper()
        if name.startswith(('LICENSE', 'LICENCE', 'COPYING', 'UNLICENSE', 'NOTICE')):
            if path.is_symlink():
                raise ToolError('INVALID_LICENSE_PATH', 'License resources cannot follow symbolic links')
            candidates = path.rglob('*') if path.is_dir() else [path]
            for candidate in candidates:
                inspected += 1
                if inspected > 512:
                    raise ToolError('LICENSE_RESOURCE_LIMIT', 'License directories exceed the resource traversal limit')
                if candidate.is_symlink() or not candidate.resolve().is_relative_to(directory):
                    raise ToolError('INVALID_LICENSE_PATH', 'License resources cannot follow symbolic links')
                if candidate.is_file():
                    paths.add(candidate.resolve())
    if not paths or len(paths) > 128:
        raise ToolError('MISSING_LICENSE_TEXT', 'Package must contain a bounded set of distributable license texts')
    return sorted(paths, key=lambda path: path.relative_to(directory).as_posix())


def generate_notices(metadata: dict[str, Any], lock_path: Path) -> str:
    if not isinstance(metadata, dict) or metadata.get('version') != 1 or not isinstance(metadata.get('packages'), list):
        raise ToolError('INVALID_CARGO_METADATA', 'Cargo metadata format version 1 is required')
    if not isinstance(metadata.get('resolve'), dict):
        raise ToolError('INCOMPLETE_CARGO_METADATA', 'Cargo metadata must include the resolved dependency graph')
    members = metadata.get('workspace_members')
    if not isinstance(members, list) or not all(isinstance(item, str) for item in members):
        raise ToolError('INVALID_CARGO_METADATA', 'Cargo workspace members are missing')
    lock = tomllib.loads(lock_path.read_text())
    locked = {(package['name'], package['version']) for package in lock['package']}
    packages = []
    seen = set()
    for package in metadata['packages']:
        if not isinstance(package, dict) or not isinstance(package.get('id'), str) or package['id'] in seen:
            raise ToolError('INVALID_CARGO_METADATA', 'Cargo package identities must be unique')
        seen.add(package['id'])
        if package['id'] in members:
            continue
        for key in ('name', 'version', 'manifest_path'):
            if not isinstance(package.get(key), str):
                raise ToolError('INVALID_CARGO_METADATA', 'Cargo package identity fields are missing')
        if (package['name'], package['version']) not in locked:
            raise ToolError('UNLOCKED_DEPENDENCY', 'Every dependency notice must match Cargo.lock')
        if re.fullmatch(r'[A-Za-z0-9_-]+', package['name']) is None:
            raise ToolError('INVALID_PACKAGE_NAME', 'Dependency names must be valid Cargo identifiers')
        license_expression = package.get('license')
        if license_expression is not None and (not isinstance(license_expression, str) or '\n' in license_expression):
            raise ToolError('INVALID_LICENSE_METADATA', 'License expression must be a single line')
        if not license_expression and not package.get('license_file'):
            raise ToolError('MISSING_LICENSE_METADATA', 'Dependency must declare its license or license file')
        packages.append(package)
    if not packages:
        raise ToolError('EMPTY_DEPENDENCY_NOTICES', 'Resolved dependency notices cannot be empty')
    output = ['# Third-party notices', '',
              'Generated from locked Cargo workspace metadata, including enabled build and test dependencies.',
              'This inventory is not a claim that every listed package is linked into the runtime binary.', '']
    total = 0
    for package in sorted(packages, key=lambda package: (package['name'], package['version'], package['id'])):
        output += [f"## {package['name']} {package['version']}", '',
                   f"Declared license: {package['license'] if package.get('license') else 'license-file'}", '']
        directory = Path(package['manifest_path']).parent.resolve()
        for path in _license_files(package):
            size = path.stat().st_size
            total += size
            if not 0 < size <= 2_097_152 or total > 16_777_216:
                raise ToolError('LICENSE_SIZE_LIMIT', 'License text exceeds the distribution limit')
            text = path.read_text(encoding='utf-8')
            output += [f'### {path.relative_to(directory).as_posix()}', '', text.rstrip(), '']
    return '\n'.join(output) + '\n'


def main() -> int:
    parser = argparse.ArgumentParser(description='Collect dependency license texts from locked Cargo metadata')
    parser.add_argument('--metadata', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    arguments = parser.parse_args()
    try:
        output = generate_notices(read_json(arguments.metadata, 33_554_432), ROOT / 'Cargo.lock')
        write_new(arguments.out, output.encode(), 0o644)
        return 0
    except (ToolError, OSError, ValueError, KeyError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
