#!/usr/bin/env python3
"""Verify and atomically publish one retained source-bundle candidate."""

import contextlib
import ctypes
import errno
import hashlib
import os
import signal
import stat
import subprocess
import sys
import time


class PublicationInstalledError(RuntimeError):
    """The final name was installed, but its identity or directory durability is unconfirmed."""


def close_quietly(fd: int) -> None:
    """Close a retained descriptor without turning post-publication cleanup into failure."""
    try:
        os.close(fd)
    except OSError:
        pass


SOURCE_BUNDLE_MAX_BYTES = 128 * 1024 * 1024
READ_CHUNK_BYTES = 1024 * 1024


def checked_size(fd: int) -> None:
    """Reject an already-oversized file before reading any of its content."""
    size = os.fstat(fd).st_size
    if size < 0 or size > SOURCE_BUNDLE_MAX_BYTES:
        raise RuntimeError("source bundle exceeds the 128 MiB (134217728-byte) limit")


def digest_fd(fd: int) -> str:
    checked_size(fd)
    os.lseek(fd, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    total = 0
    try:
        while chunk := os.read(
            fd, min(READ_CHUNK_BYTES, SOURCE_BUNDLE_MAX_BYTES + 1 - total)
        ):
            total += len(chunk)
            if total > SOURCE_BUNDLE_MAX_BYTES:
                raise RuntimeError("source bundle exceeds the 128 MiB (134217728-byte) limit")
            digest.update(chunk)
        return digest.hexdigest()
    finally:
        os.lseek(fd, 0, os.SEEK_SET)


def destination_exists(directory_fd: int, name: str) -> bool:
    try:
        os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        return False
    return True


def copy_anonymous_candidate(source_fd: int, directory_fd: int, expected: str) -> int:
    """Copy into a Linux unnamed inode, so no cleanup pathname ever exists."""
    flags = os.O_RDWR | os.O_TMPFILE | os.O_CLOEXEC
    target_fd = os.open(".", flags, 0o600, dir_fd=directory_fd)
    digest = hashlib.sha256()
    total = 0
    try:
        checked_size(source_fd)
        os.lseek(source_fd, 0, os.SEEK_SET)
        while chunk := os.read(
            source_fd, min(READ_CHUNK_BYTES, SOURCE_BUNDLE_MAX_BYTES + 1 - total)
        ):
            total += len(chunk)
            if total > SOURCE_BUNDLE_MAX_BYTES:
                raise RuntimeError("source bundle exceeds the 128 MiB (134217728-byte) limit")
            digest.update(chunk)
            view = memoryview(chunk)
            while view:
                written = os.write(target_fd, view)
                if written == 0:
                    raise RuntimeError("source-bundle candidate write made no progress")
                view = view[written:]
        if digest.hexdigest() != expected:
            raise RuntimeError("verified source bundle changed before publication")
        os.fchmod(target_fd, 0o644)
        os.fsync(target_fd)
        return target_fd
    except BaseException:
        close_quietly(target_fd)
        raise


def install_fd(source_fd: int, directory_fd: int, destination_name: str) -> None:
    """Install exactly ``source_fd`` at a new destination name."""
    libc = ctypes.CDLL(None, use_errno=True)
    destination_bytes = os.fsencode(destination_name)
    if sys.platform == "darwin":
        function = libc.fclonefileat
        function.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_char_p, ctypes.c_int]
        arguments = (source_fd, directory_fd, destination_bytes, 0)
    elif sys.platform.startswith("linux"):
        function = libc.linkat
        function.argtypes = [
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
            ctypes.c_char_p,
            ctypes.c_int,
        ]
        arguments = (source_fd, b"", directory_fd, destination_bytes, 0x1000)
    else:
        raise RuntimeError("descriptor-bound source publication is unsupported")
    function.restype = ctypes.c_int
    if function(*arguments) != 0:
        error_number = ctypes.get_errno()
        if error_number == errno.EEXIST:
            raise RuntimeError("publication destination already exists")
        raise OSError(error_number, os.strerror(error_number))


def open_installed(directory_fd: int, name: str) -> int:
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK
    fd = os.open(name, flags, dir_fd=directory_fd)
    if not stat.S_ISREG(os.fstat(fd).st_mode):
        close_quietly(fd)
        raise RuntimeError("installed source bundle is not a regular file")
    return fd


ACTIVE_VERIFIER: subprocess.Popen[bytes] | None = None
ACTIVE_VERIFIER_CANCELLED = False
VERIFIER_TERMINATE_SECONDS = 1.0
VERIFIER_POLL_SECONDS = 0.01
DEFAULT_VERIFIER_TIMEOUT_SECONDS = 30 * 60
MAX_VERIFIER_TIMEOUT_SECONDS = 24 * 60 * 60


def verifier_timeout_seconds() -> float:
    """Read a finite verifier deadline without carrying an invalid value into process startup."""
    raw = os.environ.get(
        "LLXPRT_SOURCE_VERIFY_TIMEOUT_SECONDS", str(DEFAULT_VERIFIER_TIMEOUT_SECONDS)
    )
    try:
        timeout = float(raw)
    except ValueError as error:
        raise RuntimeError("source-bundle verifier timeout is invalid") from error
    if not 0.1 <= timeout <= MAX_VERIFIER_TIMEOUT_SECONDS:
        raise RuntimeError("source-bundle verifier timeout is outside its allowed range")
    return timeout


def terminate_verifier_group(
    process: subprocess.Popen[bytes], cancellation_forwarded: bool
) -> int:
    """Terminate the retained verifier group before reaping its reserved leader PID."""
    signalled = cancellation_forwarded
    if not cancellation_forwarded:
        try:
            os.killpg(process.pid, signal.SIGTERM)
            signalled = True
        except (ProcessLookupError, PermissionError):
            pass
    if signalled:
        time.sleep(VERIFIER_TERMINATE_SECONDS)
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
    try:
        return process.wait(timeout=VERIFIER_TERMINATE_SECONDS)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError("source-bundle verifier could not be reaped") from error


def verifier_signal(signum: int, _frame: object) -> None:
    """Forward cancellation; bounded waiting occurs after the interrupted wait unwinds."""
    global ACTIVE_VERIFIER_CANCELLED
    if ACTIVE_VERIFIER is not None:
        try:
            os.killpg(ACTIVE_VERIFIER.pid, signal.SIGTERM)
        except (ProcessLookupError, PermissionError):
            pass
        ACTIVE_VERIFIER_CANCELLED = True
    raise InterruptedError(f"source-bundle publisher interrupted by signal {signum}")


def run_verifier(command: list[str], environment: dict[str, str], source_fd: int) -> int:
    """Run verification in an owned process group and remove every surviving descendant."""
    global ACTIVE_VERIFIER, ACTIVE_VERIFIER_CANCELLED
    timeout = verifier_timeout_seconds()
    handled_signals = {signal.SIGTERM, signal.SIGINT, signal.SIGHUP}
    old_mask = signal.pthread_sigmask(signal.SIG_BLOCK, handled_signals)
    try:
        process = subprocess.Popen(
            command,
            env=environment,
            pass_fds=(source_fd,),
            start_new_session=True,
        )
    except BaseException:
        signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
        raise
    ACTIVE_VERIFIER = process
    ACTIVE_VERIFIER_CANCELLED = False
    reaped = False
    try:
        # A pending cancellation is delivered only after ownership is recorded and this cleanup
        # region is active, closing the spawn-to-registration leak window.
        signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
        # Observe completion without reaping the group leader. Keeping its PID reserved prevents
        # process-group reuse until every descendant has been terminated.
        deadline = time.monotonic() + timeout
        while True:
            info = os.waitid(
                os.P_PID, process.pid, os.WEXITED | os.WNOWAIT | os.WNOHANG
            )
            if info is not None:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("source-bundle verifier timed out")
            time.sleep(VERIFIER_POLL_SECONDS)
        result = terminate_verifier_group(process, ACTIVE_VERIFIER_CANCELLED)
        reaped = True
        return result
    finally:
        if not reaped:
            terminate_verifier_group(process, ACTIVE_VERIFIER_CANCELLED)
        ACTIVE_VERIFIER = None
        ACTIVE_VERIFIER_CANCELLED = False


def publish(
    source: str,
    destination: str,
    command: list[str],
    retained_destination_fd: int | None = None,
) -> None:
    """Retain source and destination capabilities across verification, then publish."""
    if not command:
        raise RuntimeError("a verification command is required")
    source = os.path.abspath(source)
    source_parent, source_name = os.path.split(source)
    source_parent = os.path.realpath(source_parent)
    source = os.path.join(source_parent, source_name)
    destination_parent, destination_name = os.path.split(os.path.abspath(destination))
    destination_parent = os.path.realpath(destination_parent)
    destination = os.path.join(destination_parent, destination_name)
    invalid_names = {"", ".", ".."}
    if source_name in invalid_names or destination_name in invalid_names:
        raise RuntimeError("invalid publication path")

    source_flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC
    directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    with contextlib.ExitStack() as stack:
        source_directory_fd = os.open(source_parent, directory_flags)
        stack.callback(close_quietly, source_directory_fd)
        if retained_destination_fd is not None:
            destination_directory_fd = os.dup(retained_destination_fd)
        elif source_parent == destination_parent:
            destination_directory_fd = os.dup(source_directory_fd)
        else:
            destination_directory_fd = os.open(destination_parent, directory_flags)
        stack.callback(close_quietly, destination_directory_fd)
        if not stat.S_ISDIR(os.fstat(destination_directory_fd).st_mode):
            raise RuntimeError("publication destination parent is not a directory")
        source_fd = os.open(source_name, source_flags, dir_fd=source_directory_fd)
        stack.callback(close_quietly, source_fd)

        source_identity = os.fstat(source_fd)
        if not stat.S_ISREG(source_identity.st_mode):
            raise RuntimeError("source bundle is not a regular file")
        if destination_exists(destination_directory_fd, destination_name):
            raise RuntimeError("publication destination already exists")
        os.fchmod(source_fd, 0o400)
        expected = digest_fd(source_fd)
        linked_identity = os.stat(
            source_name, dir_fd=source_directory_fd, follow_symlinks=False
        )
        if (linked_identity.st_dev, linked_identity.st_ino) != (
            source_identity.st_dev,
            source_identity.st_ino,
        ):
            raise RuntimeError("source bundle path changed before verification")
        os.unlink(source_name, dir_fd=source_directory_fd)

        environment = os.environ.copy()
        environment["LLXPRT_BUNDLE_SOURCE_FD"] = str(source_fd)
        returncode = run_verifier(command, environment, source_fd)
        if returncode != 0:
            raise RuntimeError(f"source-bundle verification exited {returncode}")

        if digest_fd(source_fd) != expected:
            raise RuntimeError("verified source bundle changed before publication")
        if sys.platform.startswith("linux"):
            candidate_fd = copy_anonymous_candidate(
                source_fd, destination_directory_fd, expected
            )
            stack.callback(close_quietly, candidate_fd)
        elif sys.platform == "darwin":
            candidate_fd = source_fd
            os.fchmod(candidate_fd, 0o644)
            os.fsync(candidate_fd)
        else:
            raise RuntimeError("descriptor-bound source publication is unsupported")
        install_fd(candidate_fd, destination_directory_fd, destination_name)
        try:
            installed_fd = open_installed(destination_directory_fd, destination_name)
            stack.callback(close_quietly, installed_fd)
            installed_identity = os.stat(
                destination_name, dir_fd=destination_directory_fd, follow_symlinks=False
            )
            installed_fd_identity = os.fstat(installed_fd)
            if (installed_identity.st_dev, installed_identity.st_ino) != (
                installed_fd_identity.st_dev,
                installed_fd_identity.st_ino,
            ):
                raise RuntimeError("the final name no longer identifies the verified candidate")
            if digest_fd(installed_fd) != expected:
                raise RuntimeError("the installed source bundle digest is incorrect")
            os.fsync(destination_directory_fd)
            rechecked_fd = open_installed(destination_directory_fd, destination_name)
            stack.callback(close_quietly, rechecked_fd)
            rechecked_identity = os.fstat(rechecked_fd)
            linked_identity = os.stat(
                destination_name, dir_fd=destination_directory_fd, follow_symlinks=False
            )
            if (rechecked_identity.st_dev, rechecked_identity.st_ino) != (
                installed_fd_identity.st_dev,
                installed_fd_identity.st_ino,
            ) or (linked_identity.st_dev, linked_identity.st_ino) != (
                rechecked_identity.st_dev,
                rechecked_identity.st_ino,
            ):
                raise RuntimeError("the final name changed during durability verification")
            if digest_fd(rechecked_fd) != expected:
                raise RuntimeError("the durable source bundle digest is incorrect")
        except (OSError, RuntimeError) as error:
            raise PublicationInstalledError(
                "publication state installed-durability-unconfirmed: "
                f"destination={destination}; expected_sha256={expected}; cause={error}; "
                "do not distribute it until the exact destination is inspected and durability "
                "is confirmed or the entry is removed"
            ) from error


def read_frame(fd: int, label: str) -> bytes:
    """Read one bounded NUL-terminated frame from an inherited pipe."""
    encoded = bytearray()
    while len(encoded) <= 16 * 1024:
        chunk = os.read(fd, 1024)
        if not chunk:
            raise RuntimeError(f"source-bundle builder closed before providing {label}")
        terminator = chunk.find(b"\0")
        if terminator >= 0:
            encoded.extend(chunk[:terminator])
            if not encoded:
                raise RuntimeError(f"source-bundle builder provided an empty {label}")
            return bytes(encoded)
        encoded.extend(chunk)
    raise RuntimeError(f"source-bundle {label} exceeds its byte limit")


def retain_destination_ancestor(destination_parent: str) -> tuple[int, list[str]]:
    """Walk an absolute parent no-follow and retain its deepest existing directory."""
    components = [part for part in destination_parent.split(os.sep) if part]
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    current_fd = os.open(os.sep, flags)
    for index, component in enumerate(components):
        if component in {"", ".", ".."}:
            close_quietly(current_fd)
            raise RuntimeError("invalid publication path")
        try:
            next_fd = os.open(component, flags, dir_fd=current_fd)
        except FileNotFoundError:
            return current_fd, components[index:]
        except BaseException:
            close_quietly(current_fd)
            raise
        close_quietly(current_fd)
        current_fd = next_fd
    return current_fd, []


def prepare_destination_parent(ancestor_fd: int, missing: list[str]) -> int:
    """Create only components absent during setup and return the retained final parent."""
    current_fd = os.dup(ancestor_fd)
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    try:
        for component in missing:
            try:
                os.mkdir(component, 0o755, dir_fd=current_fd)
            except FileExistsError as error:
                raise RuntimeError(
                    "publication output path appeared after initial setup"
                ) from error
            os.fsync(current_fd)
            next_fd = os.open(component, flags, dir_fd=current_fd)
            close_quietly(current_fd)
            current_fd = next_fd
        return current_fd
    except BaseException:
        close_quietly(current_fd)
        raise


def await_source_and_publish(
    destination: str, ready_fd: int, command: list[str]
) -> None:
    """Pin output setup, await commit approval and the candidate, then publish."""
    destination_parent, destination_name = os.path.split(os.path.abspath(destination))
    destination_parent = os.path.realpath(destination_parent)
    destination = os.path.join(destination_parent, destination_name)
    if destination_name in {"", ".", ".."}:
        os.write(ready_fd, b"ERROR\n")
        raise RuntimeError("invalid publication path")
    try:
        ancestor_fd, missing = retain_destination_ancestor(destination_parent)
    except (OSError, RuntimeError):
        os.write(ready_fd, b"ERROR\n")
        raise
    try:
        os.write(ready_fd, b"READY\n")
        if read_frame(sys.stdin.fileno(), "setup command") != b"PREPARE":
            raise RuntimeError("invalid source-bundle setup command")
        try:
            destination_directory_fd = prepare_destination_parent(ancestor_fd, missing)
        except (OSError, RuntimeError):
            os.write(ready_fd, b"ERROR\n")
            raise
        try:
            try:
                os.stat(
                    destination_name,
                    dir_fd=destination_directory_fd,
                    follow_symlinks=False,
                )
            except FileNotFoundError:
                pass
            else:
                os.write(ready_fd, b"ERROR\n")
                raise RuntimeError("publication destination already exists")
            os.write(ready_fd, b"PARENT_READY\n")
            source = os.fsdecode(read_frame(sys.stdin.fileno(), "candidate path"))
            source_command = [source if item == "{SOURCE}" else item for item in command]
            publish(source, destination, source_command, destination_directory_fd)
        finally:
            close_quietly(destination_directory_fd)
    finally:
        close_quietly(ancestor_fd)


def main() -> None:
    signal.signal(signal.SIGTERM, verifier_signal)
    signal.signal(signal.SIGINT, verifier_signal)
    signal.signal(signal.SIGHUP, verifier_signal)
    try:
        if len(sys.argv) >= 6 and sys.argv[1] == "--await-source" and sys.argv[4] == "--":
            await_source_and_publish(sys.argv[2], int(sys.argv[3]), sys.argv[5:])
        elif len(sys.argv) >= 5 and sys.argv[3] == "--":
            publish(sys.argv[1], sys.argv[2], sys.argv[4:])
        else:
            raise RuntimeError(
                "usage: source-bundle-publish.py SOURCE DESTINATION -- COMMAND [ARG ...]; "
                "or source-bundle-publish.py --await-source DESTINATION READY_FD -- "
                "COMMAND [ARG ...]"
            )
    except (OSError, RuntimeError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
