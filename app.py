"""CLI-first packaged entry point; `gui` opens the optional native frontend."""
import sys

from skyrim_mod_sync.cli import main
arguments = sys.argv[2:] if sys.argv[1:2] == ["--cli"] else sys.argv[1:]
raise SystemExit(main(arguments))
