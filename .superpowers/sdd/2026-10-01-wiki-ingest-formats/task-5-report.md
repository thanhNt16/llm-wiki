# Task 5 Report

## Status
Complete.

## Change
Added the exact markitdown optional-install block to `install.sh` before the final verification output. Installation order: `uv tool`, `pipx`, then `pip3 install --user`; failures emit warnings without aborting the install.

## Verification
- `bash -n install.sh` — passed; no output, exit 0.

## Commit
Pending commit in this worktree.

## Concerns
None.
