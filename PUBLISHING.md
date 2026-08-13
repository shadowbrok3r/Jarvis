# Publishing the Jarvis APK to the app store

Builds are distributed through a self-hosted app store (set `AS_URL` /
`AS_KEY`), slug **`jarvis`**, package `com.kingsofalchemy.jarvis`.

## ⚠ This APK is too big for the normal upload path

`jarvis_android.apk` is ~127 MB. If the public hostname is behind Cloudflare,
request bodies are hard-capped at **100 MB** and the upload answers
`413 Payload Too Large` before the request ever reaches the server. Publishing
over that hostname **will fail**, and no amount of retrying changes that.

Publish straight to the pod instead, from a machine with cluster access:

```bash
kubectl -n rust port-forward deploy/appstore 18080:8080     # leave running

export AS_URL=http://localhost:18080
export AS_KEY=<admin API key>
scripts/publish-appstore.sh jarvis JarvisAndroid/rust "what changed"
```

API-key auth works fine over the tunnel, and phone-side downloads are unaffected
(Cloudflare caps uploads only). The server's own limit is `AS_MAX_APK_BYTES`,
default 512 MiB.

The lasting fixes, if this gets annoying: point a DNS-only (grey-cloud) hostname at
the ingress for uploads, or shrink the APK — most of the size is the ~21 MB of
staged bootstrap assets plus native libs.

## Bump the version FIRST — every time

Android only offers an update whose `versionCode` is **strictly greater** than the
installed one, derived from the crate version:

```
versionCode = (1<<24) | (major<<16) | (minor<<8) | patch
```

so `0.1.0` → 16777472 and `0.1.1` → 16777473. **Shipping twice at the same version
is a silent no-op** — the upload succeeds and the phone simply never offers it.

Bump `JarvisAndroid/rust/Cargo.toml` (its own workspace, separate from the
jarvis-avatar root). Order is always: **bump → build → publish.**

## Build

```bash
./JarvisAndroid/scripts/stage-assets.sh          # mandatory first step (~21 MB of assets)
cd JarvisAndroid/rust
cargo egui-mobile build -a --release             # add --features ime-bridge for that variant
# APK: JarvisAndroid/rust/target/release/apk/jarvis_android.apk
```

## Publish

See the port-forward block above — passing the crate directory
(`JarvisAndroid/rust`) lets the script read the version from `Cargo.toml`, locate
the APK, and verify with `aapt2` that the APK's embedded `versionCode` matches the
version being claimed. That check is what stops a stale build from being offered as
a new one.

## Icons and changelog

The script also **extracts the launcher icon** from the APK and uploads it, so the
store and the phone app show it instead of a letter tile. Apps built without an
icon resource (plain `NativeActivity`, no `res/` mipmaps) simply keep the letter
tile — nothing to do.

The notes argument becomes a **changelog entry** in two places: recorded on the
server (`/api/apps/<slug>/changelog`, shown under "Changelog" in the store and the
phone app, and kept even after old APKs are pruned), and prepended to the repo's
`CHANGELOG.md`.

```bash
# also git-commit CHANGELOG.md (and Cargo.toml, in crate-dir mode)
scripts/publish-appstore.sh --commit <slug> <crate-dir> "what changed"

# other flags
--changelog PATH   write somewhere other than the default CHANGELOG.md
--no-changelog     server-side entry only, leave the repo file alone
--no-icon          skip icon extraction
```

## Notes

- The store keeps the last 5 builds; roll back from the AppManager desktop app.
- Signed with the shared `~/.android/debug.keystore`, so builds from any machine
  with that keystore install over each other.
