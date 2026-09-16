from __future__ import annotations

import argparse
import hashlib
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import struct
import sys
import tomllib
from typing import Any
import zipfile

from scripts.common import (
    ROOT, ToolError, bounded_integer, error_json, exact_keys, json_bytes,
    parse_json, read_json, sha256_file, write_new,
)

SCHEMA = 'docsight.release/v1'
VERSION_PATTERN = r'(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?'
TARGETS = {
    'x86_64-pc-windows-msvc': ('docsight.exe', 'pe', 0x8664),
    'x86_64-unknown-linux-gnu': ('docsight', 'elf', 62),
    'aarch64-unknown-linux-gnu': ('docsight', 'elf', 183),
    'x86_64-apple-darwin': ('docsight', 'macho', 0x01000007),
    'aarch64-apple-darwin': ('docsight', 'macho', 0x0100000C),
}
DOCUMENTS = ('README.md', 'INSTALL.md', 'PRODUCT_SCOPE.md', 'AGENT_PROTOCOL.md',
             'PERFORMANCE.md', 'CHANGELOG.md', 'LICENSE-MIT', 'LICENSE-APACHE')
EXAMPLES = ('sample_headings.docx', 'sample_semantic.pdf')
MAX_FILE_BYTES = 268_435_456
MAX_TOTAL_BYTES = 536_870_912


def checked_version(version: Any) -> str:
    if not isinstance(version, str) or re.fullmatch(VERSION_PATTERN, version) is None:
        raise ToolError('INVALID_VERSION', 'Version must be a canonical semantic release version')
    return version


def workspace_version(root: Path = ROOT) -> str:
    manifest = tomllib.loads((root / 'Cargo.toml').read_text())
    return checked_version(manifest['workspace']['package']['version'])


def checked_revision(revision: Any) -> str:
    if not isinstance(revision, str) or re.fullmatch(r'[0-9a-f]{40}', revision) is None:
        raise ToolError('INVALID_REVISION', 'Revision must be a full lowercase Git commit SHA')
    return revision


def checked_target(target: str) -> tuple[str, str, int]:
    if not isinstance(target, str) or target not in TARGETS:
        raise ToolError('UNSUPPORTED_TARGET', 'Target is not part of the supported release matrix')
    return TARGETS[target]


def matrix(root: Path = ROOT) -> dict[str, Any]:
    value = exact_keys(read_json(root / 'release/targets.json'), {'include'}, 'release matrix')
    entries = value['include']
    if not isinstance(entries, list):
        raise ToolError('INVALID_MATRIX', 'Release matrix must contain a target list')
    seen = set()
    for entry in entries:
        exact_keys(entry, {'target', 'runner', 'binary'}, 'matrix entry')
        binary, _, _ = checked_target(entry['target'])
        if entry['target'] in seen or entry['binary'] != binary:
            raise ToolError('INVALID_MATRIX', 'Matrix contains a duplicate or mismatched target')
        if not isinstance(entry['runner'], str) or re.fullmatch(r'[a-z0-9.-]+', entry['runner']) is None:
            raise ToolError('INVALID_RUNNER', 'Runner label must be a literal supported platform label')
        seen.add(entry['target'])
    if seen != set(TARGETS):
        raise ToolError('INCOMPLETE_MATRIX', 'All five release targets are required')
    return value


def check_binary(path: Path, target: str) -> None:
    _, kind, machine = checked_target(target)
    if path.is_symlink() or not path.is_file():
        raise ToolError('INVALID_BINARY', 'Binary must be a regular file, not a symbolic link')
    bounded_integer(path.stat().st_size, 64, MAX_FILE_BYTES, 'binary size')
    with path.open('rb') as source:
        header = source.read(64)
        if kind == 'elf':
            valid = (header[:6] == b'\x7fELF\x02\x01'
                     and struct.unpack_from('<H', header, 16)[0] in (2, 3)
                     and struct.unpack_from('<H', header, 18)[0] == machine)
        elif kind == 'macho':
            valid = (header[:4] == b'\xcf\xfa\xed\xfe'
                     and struct.unpack_from('<I', header, 4)[0] == machine
                     and struct.unpack_from('<I', header, 12)[0] == 2)
        else:
            offset = struct.unpack_from('<I', header, 60)[0]
            bounded_integer(offset, 64, 4_194_304, 'PE header offset')
            source.seek(offset)
            pe = source.read(26)
            valid = (header[:2] == b'MZ' and len(pe) == 26 and pe[:4] == b'PE\0\0'
                     and struct.unpack_from('<H', pe, 4)[0] == machine
                     and struct.unpack_from('<H', pe, 24)[0] == 0x20B
                     and not struct.unpack_from('<H', pe, 22)[0] & 0x2000)
    if not valid:
        raise ToolError('BINARY_TARGET_MISMATCH', 'Executable format or architecture does not match the target')


def safe_member(name: Any) -> str:
    if not isinstance(name, str) or re.fullmatch(r'[A-Za-z0-9._/-]+', name) is None:
        raise ToolError('UNSAFE_ARCHIVE_PATH', 'Archive paths must be canonical relative POSIX paths')
    path = PurePosixPath(name)
    if (not path.parts or path.is_absolute() or path.as_posix() != name or '..' in path.parts
            or any(ord(character) < 32 or ord(character) == 127 for character in name)):
        raise ToolError('UNSAFE_ARCHIVE_PATH', 'Archive paths must not escape their root')
    reserved = {'CON', 'PRN', 'AUX', 'NUL'} | {f'{prefix}{number}' for prefix in ('COM', 'LPT') for number in range(1, 10)}
    if any(part.endswith((' ', '.')) or part.split('.')[0].upper() in reserved for part in path.parts):
        raise ToolError('UNSAFE_ARCHIVE_PATH', 'Archive paths must be portable across target filesystems')
    return name


def archive_basename(version: str, target: str) -> str:
    checked_target(target)
    return f'docsight-{checked_version(version)}-{target}'


def _zip_info(name: str, executable: bool) -> zipfile.ZipInfo:
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.create_system = 3
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = (stat.S_IFREG | (0o755 if executable else 0o644)) << 16
    return info


def make_package(binary: Path, target: str, revision: str, notices: Path,
                 destination: Path, root: Path = ROOT) -> Path:
    check_binary(binary, target)
    checked_revision(revision)
    version = workspace_version(root)
    basename = archive_basename(version, target)
    toolchain = tomllib.loads((root / 'rust-toolchain.toml').read_text())['toolchain']['channel']
    payload = {name: root / name for name in DOCUMENTS}
    payload[TARGETS[target][0]] = binary
    payload['THIRD_PARTY_NOTICES.md'] = notices
    for example in EXAMPLES:
        payload[f'examples/{example}'] = root / 'fixtures/validation' / example
    schemas = sorted((root / 'schemas/v2').glob('*.json'))
    if not schemas:
        raise ToolError('MISSING_SCHEMAS', 'Release requires the versioned agent schemas')
    for schema in schemas:
        payload[f'schemas/v2/{schema.name}'] = schema
    records = []
    total_size = 0
    for name, path in sorted(payload.items()):
        safe_member(name)
        if path.is_symlink() or not path.is_file():
            raise ToolError('MISSING_RELEASE_FILE', 'A required release file is missing or is a symbolic link')
        size = bounded_integer(path.stat().st_size, 1, MAX_FILE_BYTES, 'release file size')
        total_size += size
        records.append({'path': name, 'size': size, 'sha256': sha256_file(path),
                        'executable': name == TARGETS[target][0]})
    bounded_integer(total_size, 1, MAX_TOTAL_BYTES, 'total release size')
    manifest = {'schema': SCHEMA, 'version': version, 'target': target,
                'revision': revision, 'toolchain': toolchain, 'files': records}
    destination.mkdir(parents=True, exist_ok=True)
    archive_path = destination / f'{basename}.zip'
    checksum_path = destination / f'{basename}.zip.sha256'
    if archive_path.exists() or checksum_path.exists():
        raise FileExistsError('Release output already exists')
    created = False
    try:
        with archive_path.open('xb') as raw:
            created = True
            with zipfile.ZipFile(raw, 'w') as archive:
                for record in records:
                    info = _zip_info(f"{basename}/{record['path']}", record['executable'])
                    with payload[record['path']].open('rb') as source, archive.open(info, 'w') as output:
                        shutil.copyfileobj(source, output, 1_048_576)
                archive.writestr(_zip_info(f'{basename}/release-manifest.json', False), json_bytes(manifest))
        verify_archive(archive_path)
        write_new(checksum_path, f'{sha256_file(archive_path)}  {archive_path.name}\n'.encode(), 0o644)
    except BaseException:
        if created:
            archive_path.unlink(missing_ok=True)
        raise
    return archive_path


def verify_archive(path: Path) -> dict[str, Any]:
    if path.is_symlink() or not path.is_file():
        raise ToolError('INVALID_ARCHIVE', 'Archive must be a regular file')
    bounded_integer(path.stat().st_size, 1, MAX_TOTAL_BYTES, 'archive size')
    try:
        with zipfile.ZipFile(path) as archive:
            members = archive.infolist()
            bounded_integer(len(members), 1, 256, 'archive member count')
            names: set[str] = set()
            total = 0
            for info in members:
                safe_member(info.filename)
                mode = info.external_attr >> 16
                if (info.filename.casefold() in names or info.is_dir()
                        or stat.S_IFMT(mode) != stat.S_IFREG or info.flag_bits & 1):
                    raise ToolError('INVALID_ARCHIVE_MEMBER', 'Archive contains duplicate, special or encrypted members')
                names.add(info.filename.casefold())
                total += bounded_integer(info.file_size, 1, MAX_FILE_BYTES, 'archive member size')
            bounded_integer(total, 1, MAX_TOTAL_BYTES, 'expanded archive size')
            manifests = [info for info in members if info.filename.endswith('/release-manifest.json')]
            if len(manifests) != 1 or manifests[0].file_size > 2_097_152:
                raise ToolError('INVALID_MANIFEST', 'Exactly one bounded release manifest is required')
            manifest = exact_keys(parse_json(archive.read(manifests[0])),
                                  {'schema', 'version', 'target', 'revision', 'toolchain', 'files'}, 'release manifest')
            if manifest['schema'] != SCHEMA:
                raise ToolError('INVALID_MANIFEST_SCHEMA', 'Release manifest schema is not supported')
            basename = archive_basename(manifest['version'], manifest['target'])
            checked_revision(manifest['revision'])
            if re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', str(manifest['toolchain'])) is None:
                raise ToolError('INVALID_TOOLCHAIN', 'Release must record a pinned Rust toolchain')
            if manifests[0].filename != f'{basename}/release-manifest.json' or path.name != f'{basename}.zip':
                raise ToolError('ARCHIVE_IDENTITY_MISMATCH', 'Archive name, root and manifest identity must agree')
            records = manifest['files']
            if not isinstance(records, list) or not records:
                raise ToolError('INVALID_MANIFEST', 'Manifest must declare its files')
            expected = {f'{basename}/release-manifest.json'}
            seen_paths: list[str] = []
            executable_name = TARGETS[manifest['target']][0]
            for record in records:
                exact_keys(record, {'path', 'size', 'sha256', 'executable'}, 'manifest file')
                relative = safe_member(record['path'])
                full = f'{basename}/{relative}'
                if full in expected or not isinstance(record['sha256'], str) or re.fullmatch(r'[0-9a-f]{64}', record['sha256']) is None:
                    raise ToolError('INVALID_MANIFEST', 'Manifest paths and SHA-256 digests must be unique and valid')
                bounded_integer(record['size'], 1, MAX_FILE_BYTES, 'manifest file size')
                if type(record['executable']) is not bool or record['executable'] != (relative == executable_name):
                    raise ToolError('INVALID_EXECUTABLE_MODE', 'Only the target binary may be executable')
                expected.add(full)
                seen_paths.append(relative)
                info = archive.getinfo(full)
                expected_mode = stat.S_IFREG | (0o755 if record['executable'] else 0o644)
                if info.file_size != record['size'] or info.external_attr >> 16 != expected_mode:
                    raise ToolError('RELEASE_METADATA_MISMATCH', 'File size or permission differs from the manifest')
                digest = hashlib.sha256()
                with archive.open(info) as source:
                    for chunk in iter(lambda: source.read(1_048_576), b''):
                        digest.update(chunk)
                if digest.hexdigest() != record['sha256']:
                    raise ToolError('RELEASE_DIGEST_MISMATCH', 'A release file differs from its declared digest')
            if seen_paths != sorted(seen_paths) or expected != {info.filename for info in members}:
                raise ToolError('RELEASE_CONTENT_MISMATCH', 'Archive contains missing, extra or unsorted manifest entries')
            required = set(DOCUMENTS) | {executable_name, 'THIRD_PARTY_NOTICES.md'} | {f'examples/{name}' for name in EXAMPLES}
            if not required <= set(seen_paths) or not any(name.startswith('schemas/v2/') for name in seen_paths):
                raise ToolError('INCOMPLETE_RELEASE', 'Archive lacks required documentation, examples, licenses or schemas')
            return manifest
    except (zipfile.BadZipFile, KeyError, EOFError, RuntimeError) as error:
        raise ToolError('CORRUPT_ARCHIVE', 'Release archive is corrupt or missing declared content') from error


def collect_archives(directory: Path) -> dict[str, Any]:
    archives = sorted(directory.glob('*.zip'))
    if len(archives) != len(TARGETS):
        raise ToolError('INCOMPLETE_RELEASE_SET', 'A release requires exactly five platform archives')
    targets = set()
    identities = set()
    lines = []
    for path in archives:
        manifest = verify_archive(path)
        targets.add(manifest['target'])
        identities.add((manifest['version'], manifest['revision'], manifest['toolchain']))
        expected = f'{sha256_file(path)}  {path.name}\n'
        if path.with_name(path.name + '.sha256').read_text() != expected:
            raise ToolError('ARCHIVE_CHECKSUM_MISMATCH', 'Archive checksum differs from its sidecar')
        lines.append(expected)
    if targets != set(TARGETS) or len(identities) != 1:
        raise ToolError('INCONSISTENT_RELEASE_SET', 'All targets must have the same version, commit and toolchain')
    version, revision, toolchain = identities.pop()
    write_new(directory / 'SHA256SUMS', ''.join(lines).encode(), 0o644)
    return {'version': version, 'revision': revision, 'toolchain': toolchain, 'targets': sorted(targets)}


def main() -> int:
    parser = argparse.ArgumentParser(description='Build and verify offline DocSight release archives')
    commands = parser.add_subparsers(dest='command', required=True)
    commands.add_parser('matrix')
    version = commands.add_parser('version')
    version.add_argument('--tag')
    package = commands.add_parser('package')
    package.add_argument('--binary', type=Path, required=True)
    package.add_argument('--target', required=True, choices=sorted(TARGETS))
    package.add_argument('--revision', required=True)
    package.add_argument('--notices', type=Path, required=True)
    package.add_argument('--out', type=Path, required=True)
    verify = commands.add_parser('verify')
    verify.add_argument('archive', type=Path)
    collect = commands.add_parser('collect')
    collect.add_argument('directory', type=Path)
    arguments = parser.parse_args()
    try:
        if arguments.command == 'matrix':
            result = matrix()
        elif arguments.command == 'version':
            value = workspace_version()
            if arguments.tag is not None and arguments.tag != f'v{value}':
                raise ToolError('TAG_VERSION_MISMATCH', 'Release tag must exactly match the workspace version')
            result = {'version': value, 'prerelease': value.startswith('0.') or '-' in value}
        elif arguments.command == 'package':
            path = make_package(arguments.binary, arguments.target, arguments.revision, arguments.notices, arguments.out)
            result = {'archive': str(path), 'sha256': sha256_file(path)}
        elif arguments.command == 'verify':
            result = verify_archive(arguments.archive)
        else:
            result = collect_archives(arguments.directory)
        sys.stdout.buffer.write(json_bytes(result))
        return 0
    except (ToolError, OSError, ValueError) as error:
        sys.stderr.buffer.write(error_json(error))
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
