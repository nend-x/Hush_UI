# Release Codenames

Releases are identified by a lowercase codename plus a sequence number:
`<codename><n>` where `n` is the chronological release order (1-indexed, gapless).
Tags and release titles use this format; the internal app version stays semver
(`package.json` / `src-tauri/Cargo.toml` / `tauri.conf.json`) and the portable
exe filename is unchanged.

| Codename | Was | Notes |
|---|---|---|
| pineapple1 | v0.1.0 | first release |
| mango2 | v0.1.1 | |
| coconut3 | v0.1.2 | |
| papaya4 | v0.1.3 | |
| lychee5 | v0.1.4 | |
| guava6 | v0.1.5 | |
| dragonfruit7 | v0.2.0 | bridge into the mythology arc |
| phoenix8 | v0.2.1 | |
| griffin9 | v0.2.2 | |
| kraken10 | v0.2.4 | cleanup + flat panels |
| hydra11 | v0.2.5 | desktop table: vertical-only scrolling |
| basilisk12 | v0.3.0 | |
| sphinx13 | v0.3.1 | |
| cerberus14 | v0.3.2 | |
| chimera15 | v0.3.3 | |
| pegasus16 | v0.3.4 | |
| roc17 | v0.3.5 | |
| banshee18 | v0.4.0 | |
| valkyrie19 | v0.4.1 | notes widget rework |
| titan20 | v0.4.2 | tag only — release superseded by myth21 |
| myth21 | v0.4.3 | note windows off the main thread |
| milk22 | — | tag only, points at main HEAD (no release yet) |
| charley23 | v0.4.4 | current latest — frameless windows (no DWM ghost border), notes widget fits content |
| overdrive24 | v0.5.0 | pre-release — frosted glass light theme (Material 3 Dark stays default) |

## Convention for future releases

- Numbering continues from the last tag: next release is `oracle25`, then
  `echo26`, `atlas27`, ... Name pool: `oracle`, `echo`, `atlas`, `rune`,
  `fable`, `legend`, `saga`, `omen`, `relic`, `veil`.
- Keep semver in the manifests and the `Hush_UI-x.y.z-portable.exe` asset name;
  the codename+number goes in the tag, release title, and this table.
- After publishing, mark the new release as latest (or confirm GitHub picked it —
  with non-semver tags the automatic latest pick can be alphabetical, not
  chronological; PATCH the release with `{"make_latest": "true"}` if needed).
- Theme so far: tropical fruits (1-6) -> dragonfruit -> mythological beasts
  (7-21) -> milk. The pool continues the mystical one-word theme.
