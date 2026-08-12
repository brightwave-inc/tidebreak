"""Shared plumbing for driving a headless LibreOffice Calc over UNO.

`import uno` binds to the *system* python3 that LibreOffice's `python3-uno`
package installs into, so every helper built on this module has to run under
`python3` rather than a virtual environment. Outside the sandbox image there
is usually no UNO bridge at all; the loaders below say that plainly instead
of failing with an import traceback.

Each session owns a private soffice process with an isolated user profile,
reached over a uniquely named pipe. That keeps concurrent helper runs — and
any LibreOffice the host started for its own conversions — from sharing a
profile lock, and it means the process can always be terminated on the way
out rather than lingering as a document server.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
import uuid
from contextlib import contextmanager
from pathlib import Path

from _tidebreak_preview import HelperError

# Cell strings LibreOffice renders for the spreadsheet error codes worth
# reporting; the numeric codes differ between engines, the strings do not.
ERROR_VALUES = (
    "#REF!",
    "#DIV/0!",
    "#VALUE!",
    "#NAME?",
    "#N/A",
    "#NUM!",
    "#NULL!",
)

DEFAULT_TIMEOUT = 120


def require_uno():
    """Return the `uno` module, or explain that this needs the sandbox."""

    try:
        import uno
    except ImportError as error:
        raise HelperError(
            "Calc automation needs LibreOffice's Python bridge (python3-uno), which "
            "exists in the sandbox document image but not in this environment. Run "
            "the command in a sandboxed exec workspace, or tell the user the "
            "workbook cannot be edited here — do not rebuild it with openpyxl."
        ) from error
    return uno


def require_soffice() -> str:
    executable = shutil.which("soffice") or shutil.which("libreoffice")
    if executable is None:
        raise HelperError(
            "LibreOffice is not available in this environment; Calc automation needs "
            "the sandbox document image."
        )
    return executable


def _property_values(uno_module, values: dict):
    from com.sun.star.beans import PropertyValue

    properties = []
    for name, value in values.items():
        prop = PropertyValue()
        prop.Name = name
        prop.Value = value
        properties.append(prop)
    return tuple(properties)


def _connect(uno_module, pipe: str, process, deadline: float):
    context = uno_module.getComponentContext()
    resolver = context.ServiceManager.createInstanceWithContext(
        "com.sun.star.bridge.UnoUrlResolver", context
    )
    target = f"uno:pipe,name={pipe};urp;StarOffice.ComponentContext"
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise HelperError(
                f"LibreOffice exited before accepting connections (status {process.returncode})"
            )
        try:
            return resolver.resolve(target)
        except Exception as error:  # NoConnectException until the pipe is up.
            last_error = error
            time.sleep(0.25)
    raise HelperError(
        f"timed out after {DEFAULT_TIMEOUT}s waiting for LibreOffice to accept a "
        f"connection ({last_error})"
    )


@contextmanager
def calc_document(path: Path, *, timeout: int = DEFAULT_TIMEOUT, read_only: bool = False):
    """Open `path` in a private headless Calc and yield the loaded document.

    The document is closed and the soffice process terminated on the way out,
    including on failure. Callers that changed something call `store()` on the
    yielded document; storing through the original media descriptor keeps the
    file's own format and everything the format carries.
    """

    uno_module = require_uno()
    executable = require_soffice()
    pipe = f"tidebreak-calc-{uuid.uuid4().hex}"
    with tempfile.TemporaryDirectory(prefix="tidebreak-calc-") as temporary:
        root = Path(temporary)
        profile = root / "profile"
        profile.mkdir()
        environment = os.environ.copy()
        environment["HOME"] = str(root)
        process = subprocess.Popen(
            [
                executable,
                "--headless",
                "--invisible",
                "--nologo",
                "--nodefault",
                "--norestore",
                "--nolockcheck",
                f"-env:UserInstallation={profile.resolve().as_uri()}",
                f"--accept=pipe,name={pipe};urp;",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env=environment,
        )
        document = None
        try:
            deadline = time.monotonic() + timeout
            context = _connect(uno_module, pipe, process, deadline)
            desktop = context.ServiceManager.createInstanceWithContext(
                "com.sun.star.frame.Desktop", context
            )
            options = {"Hidden": True, "UpdateDocMode": 1}
            if read_only:
                options["ReadOnly"] = True
            document = desktop.loadComponentFromURL(
                uno_module.systemPathToFileUrl(str(path.resolve())),
                "_blank",
                0,
                _property_values(uno_module, options),
            )
            if document is None:
                raise HelperError(f"LibreOffice could not open {path.name}")
            if not hasattr(document, "Sheets"):
                raise HelperError(f"{path.name} did not open as a spreadsheet")
            yield document
        finally:
            if document is not None:
                try:
                    document.close(False)
                except Exception:
                    pass
            _terminate(process)


def _terminate(process: subprocess.Popen) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=20)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=10)


def sheet_by_name(document, name: str):
    sheets = document.Sheets
    if not sheets.hasByName(name):
        available = ", ".join(sheets.getElementNames())
        raise HelperError(f"sheet {name!r} does not exist; sheets are: {available}")
    return sheets.getByName(name)


def cell_by_reference(sheet, reference: str):
    try:
        return sheet.getCellRangeByName(reference)
    except Exception as error:
        raise HelperError(f"invalid cell reference {reference!r}: {error}") from error


def used_area(sheet):
    """Return the inclusive `(columns, rows)` extent of a sheet's used area."""

    cursor = sheet.createCursor()
    cursor.gotoEndOfUsedArea(False)
    address = cursor.RangeAddress
    return address.EndColumn, address.EndRow


def column_letters(index: int) -> str:
    letters = ""
    index += 1
    while index:
        index, remainder = divmod(index - 1, 26)
        letters = chr(ord("A") + remainder) + letters
    return letters


def cell_reference(column: int, row: int) -> str:
    return f"{column_letters(column)}{row + 1}"


def apply_cell_input(cell, value: str) -> str:
    """Set `value` on `cell`, and report how it was interpreted.

    A leading `=` means a formula. Anything else is stored as a number when it
    parses as one and as text otherwise, which is what a person typing into
    the cell would get.
    """

    if value.startswith("="):
        cell.setFormula(value)
        return "formula"
    try:
        cell.setValue(float(value))
    except ValueError:
        cell.setString(value)
        return "text"
    return "number"


def content_type(cell) -> str:
    """The cell's content kind as a lowercase word: empty/number/text/formula."""

    kind = cell.getType()
    # UNO enums carry their name in `.value`; older bridges hand back an int.
    name = getattr(kind, "value", None)
    if name is None:
        name = {0: "EMPTY", 1: "VALUE", 2: "TEXT", 3: "FORMULA"}.get(int(kind), "UNKNOWN")
    return {"EMPTY": "empty", "VALUE": "number", "TEXT": "text", "FORMULA": "formula"}.get(
        name, "unknown"
    )


def looks_like_number(text: str) -> bool:
    """Whether a text cell's contents are really a number in disguise."""

    candidate = text.strip().replace(",", "")
    if not candidate or candidate in ERROR_VALUES:
        return False
    if candidate.endswith("%"):
        candidate = candidate[:-1]
    if candidate and candidate[0] in "$€£":
        candidate = candidate[1:]
    if candidate.startswith("(") and candidate.endswith(")"):
        candidate = candidate[1:-1]
    try:
        float(candidate)
    except ValueError:
        return False
    return True
