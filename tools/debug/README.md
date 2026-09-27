# Debug and Maintenance Utilities

This directory contains standalone scripts used during development, testing, and debugging remote Vast.ai instances.

## Directory Structure

- `remote/`: Expect scripts (`.exp`) for interactive SSH automation, status checks, and remote service diagnostics on rented Vast.ai instances.
- `patches/`: Helper Python scripts used for AST modifications, GStreamer pipeline adjustments, and one-off refactorings.

> **Note:** These scripts are for manual testing and local developer workflows. Automated application logic is contained in `src/` and `src-tauri/`.

Remote deployment scripts expect `tools/debug/artifacts/agent.tar.gz`. Load
the SSH key into `ssh-agent` before running them; passphrases are not stored
in the scripts.
