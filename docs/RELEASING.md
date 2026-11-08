# Releasing aun-core

Releases are cut **only** by pushing a `vX.Y.Z` tag on a commit that is already on `main`.
`.github/workflows/release.yml` does everything else. Nobody builds or publishes by hand, and the
workflow **never pushes to `main` or any other branch**, so `main` stays protected (PR + owner
approval) with no bypass for the workflow.

## Steps (maintainer)

1. In a PR, move the **Unreleased** entries in `CHANGELOG.md` under `## [X.Y.Z] - YYYY-MM-DD` and
   set `version = "X.Y.Z"` in `[workspace.package]` in `Cargo.toml`. Get it approved and merged.
2. Tag the merge commit on `main` and push the tag:
   ```sh
   git fetch origin && git tag -a vX.Y.Z -m "aun-core vX.Y.Z" origin/main && git push origin vX.Y.Z
   ```
   Pre-releases use `vX.Y.Z-rc.N` (the GitHub release is marked as a pre-release).
3. Watch the `release` workflow. When it succeeds, `X.Y.Z` is available on all three platforms.

## What the workflow does

| Job | Does |
|---|---|
| `verify` | Checks the tag is on `main`, matches `workspace.package.version`, and that `CHANGELOG.md` has a `[X.Y.Z]` section |
| `build` | Builds every target (`build.yml`), runs the binding smoke tests, publishes the AAR to GitHub Packages Maven and the NuGet package to GitHub Packages |
| `github-release` | Creates the GitHub release `vX.Y.Z` with `AunCoreFFI.xcframework.zip`, Swift sources, the AAR, the `.nupkg`, the bindings sources and `SHA256SUMS.txt`, then runs `scripts/swiftpm-release-tag.sh` |

### The SwiftPM tag

SwiftPM resolves packages **by tag** and reads `Package.swift` at the tagged commit. The binary
target's URL and checksum must name the release's own zip, which only exists after the build.
So, for `vX.Y.Z`, `scripts/swiftpm-release-tag.sh`:

1. checks out the tagged commit (`vX.Y.Z`) detached;
2. makes a **release-only commit** on top of it that sets `releaseVersion = "X.Y.Z"` and
   `releaseChecksum` (from `swift package compute-checksum`) in `Package.swift` and refreshes
   `apple/Sources/AunCore/AunCore.swift`. This commit is not on any branch;
3. tags it **`X.Y.Z`** (no `v`) and pushes **only that tag** (`git push origin refs/tags/X.Y.Z`).

```
main ── A ── B (vX.Y.Z)
               \
                R  ← tag X.Y.Z: Package.swift → releases/download/vX.Y.Z/AunCoreFFI.xcframework.zip
```

Consumers depend on the version, never on `main`:

```swift
.package(url: "https://github.com/Al-Ummah-Now/AlUmmahNowCore", from: "X.Y.Z")
```

SwiftPM fetches tags along with the commits they point at, so it resolves `R` even though no
branch contains it. When both `vX.Y.Z` and `X.Y.Z` exist, SwiftPM prefers the plain `X.Y.Z` (this
was checked locally with a file:// package). `Package.swift` on `main` keeps pointing at an older
release; that is expected. To build against local changes, use `scripts/build-xcframework.sh`
and `AUN_LOCAL_XCFRAMEWORK=1`.

### Re-runs and safety

- The release commit reuses the tagged commit's dates, so re-running with the same artifact makes
  the same commit and the script does nothing.
- If a re-run produced a different XCFramework (different checksum), the script moves `X.Y.Z`
  to the new release commit, but **only** if the existing `X.Y.Z` points at a release commit whose
  parent is the same `vX.Y.Z` commit. Any other existing `X.Y.Z` tag is refused, and the job fails.
- Published `vX.Y.Z` tags are never moved or deleted. A bad release is fixed with a new patch
  version.

## Android and .NET

- **Android:** `implementation("net.alummahnow:aun-core:X.Y.Z")` from
  `https://maven.pkg.github.com/Al-Ummah-Now/AlUmmahNowCore` (a token with `read:packages` is
  needed even though the repo is public; that is a GitHub Packages rule).
- **.NET:** `dotnet add package AlUmmahNow.Core --version X.Y.Z` from
  `https://nuget.pkg.github.com/Al-Ummah-Now/index.json` (same token rule).

## History

`v0.1.0` predates this scheme: its SwiftPM tag `0.1.0` points at a release commit that was pushed
to `main` (a920a8f). From the next release on, nothing is pushed to `main`.
