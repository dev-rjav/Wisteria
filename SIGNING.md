# Code signing & auto-updates

Wisteria's releases are **Authenticode code-signed** (so Windows SmartScreen and browsers stop
warning users) and ship an **in-app auto-updater**. This document explains the one-time setup.

There are **two independent signatures**, don't confuse them:

| Signature | Purpose | Key |
| --- | --- | --- |
| **Authenticode** (SignPath) | Windows trusts the installer's publisher (SmartScreen/AV) | SignPath-managed cert |
| **Tauri updater** (minisign) | The app verifies an update payload is genuinely ours before self-replacing | our `updater.key` (public key baked into `tauri.conf.json`) |

The Windows CI signs Authenticode **first**, then regenerates the updater signature over the signed
bytes — because Authenticode changes the file, which would otherwise invalidate the updater `.sig`.

---

## 1. The Tauri updater key

Generated once with `cargo tauri signer generate`. The **public** key already lives in
`crates/wisteria-gui/tauri.conf.json` under `plugins.updater.pubkey`. The **private** key must
**never** be committed — it is stored locally at `~/.wisteria/updater.key` and as GitHub secrets.

Add these repository **secrets** (Settings → Secrets and variables → Actions):

| Secret | Value |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | contents of `~/.wisteria/updater.key` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the key's password (empty string if none) |

To rotate the key: `cargo tauri signer generate -w ~/.wisteria/updater.key -f`, paste the new
public key into `tauri.conf.json`, and update the secret. Note: clients on the **old** pubkey can't
verify updates signed by the new key, so ship a rotation as a manual-download release.

## 2. SignPath (Authenticode, free for OSS)

1. Apply for the free **[SignPath.io](https://signpath.io) open-source plan** with the repo URL.
2. In SignPath, create a **Project**, an **Artifact configuration** (type: *ZIP/binary*, signing a
   single `.exe`), and a **Signing policy** (e.g. `release-signing`).
3. Install the **SignPath GitHub App** on the `dev-rjav/Wisteria` repo.
4. Add the CI wiring:

   Repository **secrets**:
   | Secret | Value |
   | --- | --- |
   | `SIGNPATH_API_TOKEN` | a SignPath CI user API token |

   Repository **variables**:
   | Variable | Value |
   | --- | --- |
   | `SIGNPATH_ORGANIZATION_ID` | your SignPath organization id (GUID) |
   | `SIGNPATH_PROJECT_SLUG` | the project slug you created |
   | `SIGNPATH_SIGNING_POLICY_SLUG` | e.g. `release-signing` |

Until SignPath is approved and configured, the `windows` job's *Sign with SignPath* step will fail.
For an interim unsigned build you can comment that step out (users will still see SmartScreen).

## 3. Cutting a release

```bash
# bump version in Cargo.toml + tauri.conf.json, commit, then:
git tag v0.1.5
git push origin v0.1.5
```

The `Release` workflow then:
1. builds Linux (deb + AppImage) and Windows (NSIS) installers,
2. Authenticode-signs the Windows installer via SignPath,
3. regenerates the updater signature over the signed installer,
4. publishes `latest.json` (the update manifest) + all installers to the GitHub Release.

Running apps poll `releases/latest/download/latest.json` on startup and offer an **Update &
restart** prompt when a newer signed build exists. Updates are always opt-in — never forced.

## Why the warnings happened before

Unsigned + zero-reputation binaries trip SmartScreen ("unknown publisher"), browser Safe-Browsing
("not commonly downloaded"), and heuristic AV (Wisteria legitimately uses global hotkeys, simulated
paste, clipboard, and mic capture). Authenticode signing + accruing reputation clears all three;
SignPath provides the signing without needing a hardware token.
