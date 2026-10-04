# Release Codenames

Releases are identified by codenames instead of version numbers. Tags and release
titles use a single lowercase word; the internal app version stays semver
(`package.json` / `src-tauri/Cargo.toml` / `tauri.conf.json`) and keeps the
portable exe filename unchanged.

| Codename | Was | Notes |
|---|---|---|
| pineapple | v0.1.0 | first release |
| mango | v0.1.1 | |
| coconut | v0.1.2 | |
| papaya | v0.1.3 | |
| lychee | v0.1.4 | |
| guava | v0.1.5 | |
| dragonfruit | v0.2.0 | bridge into the mythology arc |
| phoenix | v0.2.1 | |
| griffin | v0.2.2 | |
| kraken | v0.2.4 | cleanup + flat panels |
| hydra | v0.2.5 | desktop table: vertical-only scrolling |
| basilisk | v0.3.0 | |
| sphinx | v0.3.1 | |
| cerberus | v0.3.2 | |
| chimera | v0.3.3 | |
| pegasus | v0.3.4 | |
| roc | v0.3.5 | |
| banshee | v0.4.0 | |
| valkyrie | v0.4.1 | notes widget rework |
| titan | v0.4.2 | tag only — release superseded by myth |
| myth | v0.4.3 | current — note windows off the main thread |

## Convention for future releases

- Tag the release with the next codename from the pool (lowercase, one word):
  `oracle`, `echo`, `atlas`, `rune`, `fable`, `legend`, `saga`, `omen`, `relic`, `veil`.
- Keep semver in the manifests and the `Hush_UI-x.y.z-portable.exe` asset name;
  the codename lives in the tag, release title, and this table.
- Theme so far: tropical fruits (0.1.x) -> dragonfruit -> mythological beasts
  (0.2.x-0.4.x) -> myth. The pool continues the mystical one-word theme.
