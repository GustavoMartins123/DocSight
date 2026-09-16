from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
from typing import Any, Sequence

ROOT = Path(__file__).resolve().parents[1]


class ToolError(Exception):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


@dataclass(frozen=True)
class ProcessResult:
    returncode: int
    stdout: bytes
    stderr: bytes
    elapsed_ms: int
    termination: str | None = None


def _unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ToolError('DUPLICATE_JSON_KEY', 'JSON contains duplicate object keys')
        result[key] = value
    return result


def _reject_constant(value: str) -> None:
    raise ToolError('INVALID_JSON_NUMBER', 'JSON numbers must be finite')


def _finite_json_float(value: str) -> float:
    result = float(value)
    if not math.isfinite(result):
        raise ToolError('INVALID_JSON_NUMBER', 'JSON numbers must be finite')
    return result


def parse_json(data: bytes | str) -> Any:
    try:
        return json.loads(data, object_pairs_hook=_unique_object, parse_constant=_reject_constant,
                          parse_float=_finite_json_float)
    except (ValueError, UnicodeError, RecursionError) as error:
        raise ToolError('INVALID_JSON', 'Input is not valid bounded JSON') from error


def read_json(path: Path, maximum_bytes: int = 2_097_152) -> Any:
    with path.open('rb') as source:
        data = source.read(maximum_bytes + 1)
    if len(data) > maximum_bytes:
        raise ToolError('JSON_SIZE_LIMIT', 'JSON input exceeds the size limit')
    return parse_json(data)


def json_bytes(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True, allow_nan=False) + '\n').encode()


def write_new(path: Path, data: bytes, mode: int = 0o600) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    try:
        with os.fdopen(descriptor, 'wb') as destination:
            destination.write(data)
            destination.flush()
            os.fsync(destination.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1_048_576), b''):
            digest.update(chunk)
    return digest.hexdigest()


def exact_keys(value: Any, keys: set[str], name: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise ToolError('INVALID_FIELDS', f'{name} has missing or unknown fields')
    return value


def finite_number(value: Any, minimum: float, name: str) -> float:
    try:
        valid = type(value) in (int, float) and math.isfinite(value) and value >= minimum
    except OverflowError:
        valid = False
    if not valid:
        raise ToolError('INVALID_NUMBER', f'{name} must be finite and at least {minimum}')
    return value


def bounded_integer(value: Any, minimum: int, maximum: int, name: str) -> int:
    if type(value) is not int or not minimum <= value <= maximum:
        raise ToolError('INVALID_INTEGER', f'{name} is outside the permitted integer range')
    return value


def isolated_environment() -> dict[str, str]:
    preserved = ('SYSTEMROOT', 'WINDIR', 'COMSPEC', 'TEMP', 'TMP', 'TMPDIR')
    result = {key: os.environ[key] for key in preserved if key in os.environ}
    if os.name == 'nt':
        root = os.environ.get('SYSTEMROOT')
        if not root:
            raise ToolError('MISSING_SYSTEMROOT', 'Windows execution requires SYSTEMROOT')
        result['PATH'] = str(Path(root) / 'System32')
    else:
        result['PATH'] = '/usr/bin:/bin'
    result.update({'LANG': 'C', 'LC_ALL': 'C', 'NO_COLOR': '1'})
    return result


def _kill_process(process: subprocess.Popen[bytes]) -> None:
    if os.name == 'posix':
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    else:
        process.kill()
    process.wait()


def run_bounded(
    arguments: Sequence[str],
    *,
    cwd: Path,
    timeout: float = 30.0,
    output_limit: int = 4_194_304,
    env: dict[str, str] | None = None,
) -> ProcessResult:
    finite_number(timeout, 0.001, 'timeout')
    bounded_integer(output_limit, 1, 268_435_456, 'output limit')
    if not arguments or not all(isinstance(argument, str) for argument in arguments):
        raise ToolError('INVALID_COMMAND', 'A nonempty argument vector is required')
    started = time.monotonic()
    termination = None
    with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
        process = subprocess.Popen(
            list(arguments), cwd=cwd, stdin=subprocess.DEVNULL,
            stdout=stdout, stderr=stderr, env=env,
            start_new_session=os.name == 'posix',
        )
        try:
            while True:
                size = os.fstat(stdout.fileno()).st_size + os.fstat(stderr.fileno()).st_size
                if size > output_limit:
                    termination = 'output_limit'
                    _kill_process(process)
                    break
                if process.poll() is not None:
                    break
                if time.monotonic() - started >= timeout:
                    termination = 'timeout'
                    _kill_process(process)
                    break
                time.sleep(0.01)
        except BaseException:
            _kill_process(process)
            raise
        elapsed_ms = round((time.monotonic() - started) * 1000)
        stdout.seek(0)
        stderr.seek(0)
        out = stdout.read(output_limit)
        err = stderr.read(max(0, output_limit - len(out)))
    return ProcessResult(process.returncode, out, err, elapsed_ms, termination)


def error_json(error: Exception) -> bytes:
    if isinstance(error, ToolError):
        code, message = error.code, str(error)
    elif isinstance(error, FileExistsError):
        code, message = 'OUTPUT_EXISTS', 'Output already exists; choose a new destination'
    else:
        code, message = 'IO_ERROR', 'File or process operation failed; verify paths and permissions'
    return json_bytes({'schema': 'docsight.tooling-error/v1', 'code': code, 'message': message})
