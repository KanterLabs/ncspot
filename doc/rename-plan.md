# Resonance rename completion plan

Finish the confirmed **ncspot → Resonance** rename across the application, developer tooling, installation, releases, and project identity. The app already uses Resonance in its primary executable, Cargo package, frontend package, logo, desktop entry, and Spotify environment variables. Implementation was authorized after this plan was reviewed. The work units below define the completion gates; the implementation status records verified results and remaining access limits.

## Naming contract

| Surface | Final name |
| --- | --- |
| Product | Resonance, maintained by KanterLabs |
| Rust package and library crate | `resonance` |
| Primary executable | `resonance` |
| Frontend executable | `resonance-opentui` |
| Update command | `resonance-update` |
| Repository slug | `resonance`, after checking the existing canonical repository and destination availability |
| New installation directories | Platform directories for `resonance` |
| Existing installations | Continue using populated legacy directories under the documented selection rules |

Keep `ncspot` only where it has a specific purpose: original authorship and license credit, historical changelog/distribution information, legacy executable/update aliases, legacy directory lookup, and compatibility fixtures. Keep the upstream remote pointing to the original project. Do not rewrite upstream history or describe upstream packages as Resonance packages.

## Initial audit evidence (before implementation)

- [Cargo.toml](../Cargo.toml) names the package and primary binary `resonance`, but deliberately retains the library crate name `ncspot`. [xtask/Cargo.toml](../xtask/Cargo.toml) also aliases the dependency as `ncspot`.
- Settings and Help still say “ncspot config” and “Run ncspot command” in [the frontend screens](../prototype/opentui/src/screens/). [app.ts](../prototype/opentui/src/app.ts) also includes an “ncspot IPC” status message.
- [The updater](../scripts/ncspot-update.sh) exposes `ncspot-update`, queries `KanterLabs/ncspot`, and uses the legacy binary to report the installed version.
- [Package maintainer documentation](package_maintainers.md) describes old executable and asset names, including `misc/ncspot.desktop`, which is absent from the checkout. User/developer documentation and the bug template retain upstream instructions.
- [User IPC examples](users.md) still use `$NCSPOT_CACHE_DIRECTORY/ncspot.sock`; the current socket belongs under the runtime directory reported by `resonance info`. Debugging examples must likewise use the effective cache path, rather than assuming either directory name.
- [The Fedora workflow](../.github/workflows/fedora.yml) packages all three executables and notices. [The tag release workflow](../.github/workflows/cd.yml) and Debian assets in Cargo metadata omit the frontend required by the default Unix launcher.
- [Configuration path selection](../src/config.rs) already prefers a Resonance configuration when present, otherwise reuses an existing ncspot configuration and its associated paths. [legacy_main.rs](../src/legacy_main.rs) already runs the same application through the `ncspot` executable alias.
- [MPRIS](../src/mpris.rs) uses the Resonance bus name but still emits track object paths rooted at `/org/ncspot`.
- The local origin is the public GitHub `KanterLabs/ncspot` repository. The canonical Gitea repository and its mirror configuration need inspection before a cutover is specified or executed.

## Implementation work units

Implementation, repository cutover, and release verification are published and verified. Two administrative checks remain below. No matching Helm project existed, and the configured agent token rejected project creation because it lacks `projects:write`; this document carries the checkpoints until the matching project can be created with authorized access.

### 1. Complete internal and runtime naming

**Scope:** Cargo library name, Rust imports, xtask dependency name, internal comments/thread names, and MPRIS track object paths. Keep the existing executable alias and legacy directory lookup.

**Acceptance:** the library and xtask use `resonance`; workspace builds and existing tests pass; both binaries identify as Resonance; logging still works after the crate rename; MPRIS identity, track IDs, playback, and seeking work. Document the library import change for any source consumers. Every remaining `ncspot` occurrence has a documented attribution, history, or compatibility purpose. Update dependency metadata and the lockfile through Cargo, without unrelated dependency upgrades.

### 2. Finish product wording and documentation

**Scope:** frontend Settings/Help/status copy, current Rust UI messages, README, user/developer/package guides, bug templates, and a Resonance changelog entry. Explain the retained legacy interface and aliases. Replace current install/build/reporting examples with Resonance commands. Distinguish historical upstream distribution channels from this fork's releases.

**Acceptance:** visible current product copy says Resonance; documented filenames and commands exist; fresh-install instructions include the frontend; Settings directs users to the effective configuration path or `resonance info`; IPC examples use the actual runtime/socket path; bug reports reference this project's debugging instructions. Original authorship, licenses, upstream links, and historical changelog entries remain accurate. Retain the old resource benchmark as a labeled historical measurement unless Resonance is measured again; changing the row's name alone would misrepresent the result.

### 3. Introduce the Resonance updater

**Scope:** add `scripts/resonance-update.sh` and `resonance-update`; retain `ncspot-update` as a forwarding compatibility entry point. Use the primary Resonance executable for version checks. Audit snapshot coverage for every mutable config/cache/data/state path actually used, custom paths, and frontend preferences. Coordinate the final repository target with unit 6.

**Acceptance:** both update commands reach the same implementation; installed commit checks, artifact checksums, frontend installation, and verified pre-update snapshots still work. An invalid or incomplete archive leaves installed executables untouched. Verify rollback executables and populated user data remain usable; rollback must not automatically restore an older data snapshot. Test shell behavior with controlled fixtures before dispatching a real build or installing anything.

### 4. Make release packages complete

**Scope:** tag releases, Debian metadata, desktop/icon assets, generated man pages/completions, and dependency notices. Use Fedora packaging as evidence of the current complete Unix bundle. Generate assets with xtask and package the resulting files. Include the terminal launcher if the documented install flow offers it.

**Acceptance:** every advertised default Unix installation includes compatible `resonance`, `resonance-opentui`, the `ncspot` alias, checksums/notices, and the assets promised by its documentation. A clean unpack/install can launch the default interface without Bun. State platform support clearly: Windows currently uses the Rust legacy interface. Validate each advertised platform independently. Linux builds use `homelab-heavy`; short metadata checks use `homelab`. Retain other runner types only for demonstrable architecture/OS incompatibility and document those exceptions.

### 5. Prove upgrade compatibility and data preservation

**Scope:** integration fixtures covering fresh installs, legacy-only installs, both directory names present, explicit base paths, and separate frontend preferences. Prefer the current reuse-in-place policy; moving existing directories is unnecessary to finish branding.

**Acceptance:** a populated legacy fixture retains configuration, credentials, library contents, queue/playback state, local history, and preferences. Verify content, identifiers, and counts before and after launching/updating through both executable names. New installs choose Resonance paths; both-directory cases follow documented precedence; explicit base paths work. Test a retained rollback binary against post-upgrade data. Any proposed storage migration requires a verified backup, populated-data migration checks, and retained-binary compatibility before release. Save repeatable commands and sanitized results as review artifacts.

### 6. Cut over project identity

**Scope:** inspect canonical Gitea and GitHub mirror configuration, destination slug availability, repository descriptions, remotes, README/Cargo URLs, updater URLs, release/download links, badges, and any existing project integrations. Check public Spotify developer-app display branding while retaining the established client identity and redirect configuration.

**Acceptance:** canonical Gitea and public GitHub mirror both present Resonance; history, issues, pull requests, tags, releases, permissions, and integrations are preserved. Verify old URLs and update paths; document or provide a fallback where redirects are unavailable. Confirm mirror synchronization and CI with a harmless change. Update local checkout paths and thread/workspace bindings only after active work finishes. A matching Roadmap project displays Resonance. The subsequent implementation instruction authorizes the repository cutover; preserve account credentials and existing Spotify client identity.

## Execution order and release gate

Start with units 1 and 2, which can run independently with explicit file ownership. Implement units 3 and 4 once the naming contract is fixed, then complete unit 5 against the candidate artifacts. Serialize shared builds, generated assets, and lockfile edits. Prepare the unit 6 cutover checklist early; perform the external cutover after compatibility checks pass, update the final URLs together, and publish the verified release afterward.

The rename is complete when a new user can find, install, launch, configure, update, and report an issue for **Resonance**, and an existing ncspot user can upgrade without losing data. Required evidence includes clean-install and populated-upgrade results, executable/frontend smoke results, package contents and checksums, working documentation links, successful release jobs, repository/mirror verification, and a reviewed inventory of justified legacy references. A text search returning zero `ncspot` matches is not the completion criterion.

## Implementation status

- Internal crate/runtime names, frontend copy, documentation, and update command are implemented. The old executable/update commands and populated legacy directory lookup remain supported.
- The Rust workspace suite passed: 323 tests, with three existing opt-in fixtures excluded. All 160 frontend tests and the real Rust/OpenTUI engine contract passed. The compiled standalone frontend launched the Settings demo in an 80-column terminal; its configuration guidance was shortened to fit.
- The updater passed nine isolated E2E fixtures covering primary/legacy commands, checksums, incomplete archives, populated snapshots, private snapshot permissions, symlink-backed data and executables, directly executable rollback copies, failed path discovery, and the up-to-date shortcut.
- The final downloaded Linux and Fedora archives and actual Debian package passed eight installation checks each, including fresh/legacy/both-directory/custom-base cases and retained rollback executable checks. Existing Rust tests also verified populated legacy CBOR decoding and rollback compatibility. Reports are retained under `/tmp/resonance-rename-verification` and `/tmp/resonance-update-e2e-report.json` on the development machine.
- GitHub was renamed in place to `KanterLabs/resonance`; its repository ID and history were retained, and the old API URL resolves to the renamed repository. Gitea now hosts the canonical `KanterLabs/resonance` repository, with a push-on-commit GitHub mirror. The implementation was published through [PR #1](https://github.com/KanterLabs/resonance/pull/1), which is merged. Canonical and public main commits were verified equal, and the push mirror reported no error.
- CI uses `homelab-heavy` for Linux Rust/frontend builds and `homelab` for updater checks. Native ARM, macOS, and Windows retain hosted runners because the homelab pool is Linux x86_64. Release workflows package the frontend on Unix and build Windows with the legacy interface.
- T3 displays the project as Resonance at `/home/shane/projects/resonance`. The old workspace path remains a compatibility symlink. PR CI and final main CI passed all seven jobs. Final Fedora and CD builds passed for Linux x86_64/ARM, Intel/Apple Silicon macOS, and Windows. Every downloaded archive passed its checksum, portable manifest path, file-hash, and required-asset audit; Unix archives contain the current updater.
- Helm project creation is blocked by the current agent token's missing project-write scope. Hark lifecycle publishing timed out. Spotify dashboard display branding awaits confirmation because no connected browser is available; the established client ID and callback were preserved.

## Final verification record

The verified code commit is `5c19ecbbb25f10effbbd6cfa707e21d107601697`. This status-only document update follows it without changing runtime code or package inputs. The workflows published commit-based build artifacts; the existing `1.4.0` package version was retained and no new stable release tag was created.

| Gate | Result |
| --- | --- |
| [Final CI](https://github.com/KanterLabs/resonance/actions/runs/37131878140) | Seven jobs passed, including Rust, frontend/engine integration, updater E2E, format, and clippy |
| [Final Fedora build](https://github.com/KanterLabs/resonance/actions/runs/37131878136) | Complete package passed hosted and downloaded installation verification |
| [Final native release build](https://github.com/KanterLabs/resonance/actions/runs/37131905941) | All five platform jobs passed, including the actual Debian package |
| Data preservation | Populated path/CBOR fixtures, byte-preservation checks, verified snapshots, and retained rollback executables passed |
| Repository identity | In-place GitHub rename, canonical Gitea publication, synchronized main, merged/linked PR, and migrated workspace verified |
| Runner policy | Linux x86_64 builds used `homelab-heavy`; short checks used `homelab`; ARM/macOS/Windows used native hosted runners for the documented OS/architecture exceptions |

The GitHub runner API confirmed the homelab assignments and an empty runner registration list after the final jobs. Direct K3s controller/pod cleanup was not inspected because this workspace has no kubectl and read-only SSH was unavailable. Sanitized runner records and package reports remain under `/tmp/resonance-rename-verification`; obsolete duplicate candidate archives were removed after verification, while the pre-cutover Git bundle and reports were retained.

## Remaining administrative checks

- **Spotify dashboard display name:** confirm or change the existing developer app's display branding to Resonance. The connected browser is unavailable, so this could not be inspected. The established client ID and callback are unchanged.
- **Matching Helm project:** create the Resonance project and record these verified checkpoints once authorized project-write access is available. The current agent token returned HTTP 403 because it lacks `projects:write`; no unrelated project was used.

Hark lifecycle reporting also timed out. These access/reporting limits do not invalidate the code or package checks, but the two administrative checks above remain open.
