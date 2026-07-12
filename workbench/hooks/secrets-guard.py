#!/usr/bin/env python3
"""PreToolUse hook: hard-block access to secret files (.env, SSH keys, creds).

Exit 2 = block (unbypassable, even with --dangerously-skip-permissions).
Allowed: .env.example / .env.sample / .env.template
"""
import json
import re
import sys

ALLOW = re.compile(r"\.env\.(example|sample|template)$", re.I)
DENY = re.compile(
    r"(\.env(\.\w+)?$"
    r"|(^|/)\.ssh/"
    r"|id_rsa|id_ed25519|id_ecdsa"
    r"|\.pem$"
    r"|\.key$"
    r"|(^|/)credentials(\.json)?$"
    r"|\.aws/credentials"
    r"|\.netrc"
    r"|secring|keyring)",
    re.I,
)


def blocked(path: str) -> bool:
    return bool(path) and not ALLOW.search(path) and bool(DENY.search(path))


def main() -> None:
    try:
        data = json.loads(sys.stdin.read())
    except Exception:
        sys.exit(0)  # malformed input: never block

    tool = data.get("tool_name", "")
    ti = data.get("tool_input") or {}

    if tool in ("Read", "Edit", "Write"):
        if blocked(ti.get("file_path", "")):
            print(f"secrets-guard: blocked {tool} on secret file: {ti.get('file_path')}", file=sys.stderr)
            sys.exit(2)
    elif tool == "Bash":
        cmd = ti.get("command", "")
        # check each shell word that looks like a path
        for tok in re.split(r"[\s;|&<>()]+", cmd):
            tok = tok.strip("\"'")
            if blocked(tok):
                print(f"secrets-guard: blocked Bash touching secret path: {tok}", file=sys.stderr)
                sys.exit(2)

    sys.exit(0)


if __name__ == "__main__":
    main()
