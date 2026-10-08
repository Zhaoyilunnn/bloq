"""Console entry point for the shared Rust CLI."""

import sys

from ._core import _run_cli


def main() -> int:
    return _run_cli(sys.argv)
