"""A sample watoots plugin, in Python.

    python3 -m venv .venv && ./.venv/bin/pip install componentize-py
    ./.venv/bin/componentize-py -d ../../wit -w lint-plugin \
        componentize app -o py_lint.wasm

Same world as the Rust and JavaScript samples, same host, same diagnostics.
The class name follows the world; `componentize-py bindings .` regenerates the
`wit_world` package below for editors and type checkers, and componentize-py
supplies it at build time either way.
"""

from typing import List

import wit_world
from wit_world.imports import log
from wit_world.imports.types import Diagnostic, Severity


class WitWorld(wit_world.WitWorld):
    """The exports of the `lint-plugin` world."""

    def name(self) -> str:
        return "py-lint"

    def lint(self, path: str, source: str) -> List[Diagnostic]:
        # An import crossing, exactly like the other two samples.
        log.emit(Severity.HINT, f"linting {path}")

        diagnostics: List[Diagnostic] = []

        for index, line in enumerate(source.split("\n")):
            line_number = index + 1

            if len(line) > 80:
                diagnostics.append(
                    Diagnostic(
                        line=line_number,
                        column=81,
                        severity=Severity.WARNING,
                        message=f"line is {len(line)} characters, over 80",
                    )
                )

            if line.endswith((" ", "\t")):
                diagnostics.append(
                    Diagnostic(
                        line=line_number,
                        column=len(line),
                        severity=Severity.HINT,
                        message="trailing whitespace",
                    )
                )

            todo = line.find("TODO")
            if todo != -1:
                diagnostics.append(
                    Diagnostic(
                        line=line_number,
                        column=todo + 1,
                        severity=Severity.ERROR,
                        message="unresolved TODO",
                    )
                )

        log.emit(Severity.HINT, f"{len(diagnostics)} diagnostic(s)")
        return diagnostics
