#!/usr/bin/env python3
"""Create the v0.4.0-beta.3 pre-release with the portable exe (recreated after VM wipe)."""
import json, subprocess, sys, urllib.request

REPO = "nend-x/Hush_UI"
API = f"https://api.github.com/repos/{REPO}"
REPO_DIR = "/home/z/my-project/repo/Hush_UI"
ASSET = "/home/z/my-project/download/Hush_UI-0.4.0-beta.3-portable.exe"


def token() -> str:
    url = subprocess.check_output(
        ["git", "remote", "get-url", "origin"], cwd=REPO_DIR
    ).decode()
    return url.split("//")[1].split("@")[0].split(":")[-1].strip()


def req(method, path, body=None, raw=None, ct=None):
    headers = {
        "Authorization": f"token {token()}",
        "Accept": "application/vnd.github+json",
        "User-Agent": "hush-ui-agent",
    }
    if ct:
        headers["Content-Type"] = ct
    data = json.dumps(body).encode() if body is not None else raw
    r = urllib.request.Request(API + path, method=method, data=data, headers=headers)
    with urllib.request.urlopen(r) as resp:
        payload = resp.read()
        return json.loads(payload) if payload else {}


def main():
    sha = subprocess.check_output(["git", "rev-parse", "main"], cwd=REPO_DIR).decode().strip()
    print(f"tagging v0.4.0-beta.3 at {sha}")

    # 1. tag (full 40-char sha required)
    try:
        req("POST", "/git/refs", {"ref": "refs/tags/v0.4.0-beta.3", "sha": sha})
        print("tag created")
    except urllib.error.HTTPError as e:
        print(f"tag: {e} (may already exist)")

    body = """**Portable single-file build — no setup, ever.** Drop the exe anywhere and run it.

## Changes (0.4.0-beta.2 → 0.4.0-beta.3)

### Tray feature rolled back end to end
The beta tray widget never proved itself on real hardware (enumeration gaps on some Windows builds, click/icon races). Removed everything it brought:

- win32 tray backend (`win32/tray.rs`), `get_tray_items`/`tray_click` commands, `Settings.tray_enabled`, the tray-only windows feature
- Settings' Beta section + toggle; the tray widget in the widgets table (markup, poll, listeners); its CSS
- Widgets table window back to the pre-tray **340×560** (the 720 bump only existed to fit the tray widget; the 0.3.5 five-widget layout fits again)
- The notes-widget alignment fix from #40 is kept (launcher-canvas offset reset — independent of the tray)
- Old `settings.json` files keep loading (the stale `tray_enabled` key is simply ignored)

### Picker spawn-position flash — fixed for good
#28 (0.3.3) fixed "pie spawns where it disappeared, then teleports to the cursor" with a render handshake on the *show* path, but the flash could still survive through the *hide* end: the instant-dismiss path suspended the WebView2 controller before the page could render anything, freezing the full pie (at the old anchor) as the surface's last frame — and the show handshake's rAF confirm could win its race before the compositor presented the re-laid-out frame. One-frame flash, still.

Now the stale frame is **blank by construction**:
- the frontend blanks the picker (layout-preserving `visibility:hidden`) and confirms with a seq-guarded `tables_hidden` after a double-rAF
- the backend waits (bounded: 120ms instant / 280–340ms animated) for that confirm before cloak+suspend — the frozen surface is fully transparent, so the next open's resume re-presents *nothing*, regardless of timing
- the show handshake stays (latency optimization); rapid hide→re-open cancels a pending hide blank so the fresh pie can never be blanked or teleported

---

Shas: #42 (rollback + flash fix), #43 (version bump). `cargo xwin check`, `tsc --noEmit`, vite — all clean.
"""

    # 2. release
    rel = req("POST", "/releases", {
        "tag_name": "v0.4.0-beta.3",
        "target_commitish": sha,
        "name": "v0.4.0-beta.3",
        "body": body,
        "draft": False,
        "prerelease": True,
    })
    print(f"release id {rel['id']}: {rel['html_url']}")

    # 3. asset
    up = urllib.request.Request(
        rel["upload_url"].split("{")[0] + f"?name=Hush_UI-0.4.0-beta.3-portable.exe",
        method="POST",
        data=open(ASSET, "rb").read(),
        headers={
            "Authorization": f"token {token()}",
            "Content-Type": "application/octet-stream",
            "User-Agent": "hush-ui-agent",
        },
    )
    with urllib.request.urlopen(up) as resp:
        a = json.loads(resp.read())
    print(f"asset: {a['name']} ({a['size']} bytes)")


if __name__ == "__main__":
    main()
