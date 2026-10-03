#!/usr/bin/env python3
"""Minimal GitHub PR helper for Hush_UI (recreated after VM wipe).
Usage:
  gh.py create <head> <title> <body-file>
  gh.py merge <pr-number>
  gh.py pr-info <pr-number>
"""
import json, subprocess, sys, urllib.request

REPO = "nend-x/Hush_UI"
API = f"https://api.github.com/repos/{REPO}"


def token() -> str:
    try:
        out = subprocess.check_output(
            ["git", "config", "--get", "http.extraheader"],
            cwd="/home/z/my-project/repo/Hush_UI", stderr=subprocess.DEVNULL,
        ).decode()
        if "x-access-token:" in out:
            return out.split("x-access-token:")[1].strip()
    except subprocess.CalledProcessError:
        pass
    url = subprocess.check_output(
        ["git", "remote", "get-url", "origin"], cwd="/home/z/my-project/repo/Hush_UI"
    ).decode()
    cred = url.split("//")[1].split("@")[0]  # e.g. "x-access-token:ghp_..."
    return cred.split(":")[-1].strip()


def req(method: str, path: str, body: dict | None = None):
    r = urllib.request.Request(
        API + path,
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Authorization": f"token {token()}",
            "Accept": "application/vnd.github+json",
            "User-Agent": "hush-ui-agent",
        },
    )
    with urllib.request.urlopen(r) as resp:
        data = resp.read()
        return json.loads(data) if data else {}


def main():
    cmd = sys.argv[1]
    if cmd == "create":
        head, title = sys.argv[2], sys.argv[3]
        body = open(sys.argv[4], encoding="utf-8").read() if len(sys.argv) > 4 else ""
        pr = req("POST", "/pulls", {"title": title, "head": head, "base": "main", "body": body})
        print(f"PR #{pr['number']}: {pr['html_url']}")
        # best-effort auto-merge enable
        try:
            req("PUT", f"/pulls/{pr['number']}/merge", {"merge_method": "merge"})
            print("merged immediately")
        except Exception as e:
            print(f"auto-merge not instant ({e}); enable via API")
            try:
                req("PUT", f"/pulls/{pr['number']}/auto-merge", {"merge_method": "merge"})
                print("auto-merge enabled")
            except Exception as e2:
                print(f"auto-merge enable failed: {e2}")
    elif cmd == "merge":
        n = int(sys.argv[2])
        res = req("PUT", f"/pulls/{n}/merge", {"merge_method": "merge"})
        print(res.get("message", "merged"))
    elif cmd == "pr-info":
        n = int(sys.argv[2])
        pr = req("GET", f"/pulls/{n}")
        print(pr["state"], pr.get("merged"), pr["merge_commit_sha"])
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
