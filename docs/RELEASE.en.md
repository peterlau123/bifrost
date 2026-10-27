# Build & Release Pipeline (GitHub Actions)

**Two channels — the branch is the channel.**

| Branch | What a push does | Artifacts | Official release? |
|---|---|---|---|
| `main` | gates → e2e → pack | `bifrost-latest-x86_64-linux-musl.tar.gz` + `.sha256`, `bifrost_<ver>-1_amd64.deb` + `.sha256`; also rolls the `tag=latest` prerelease | ❌ no |
| `release/x.y.z` | gates → e2e → version check → pack | `bifrost-<ver>-x86_64-linux-musl.tar.gz` + `.deb` + two `.sha256` | ✅ tag `v<x.y.z>` + Release |
| anything else | `ci.yml` runs gates/e2e (`.github/workflows/ci.yml`); `release.yml` doesn't trigger | — | — |

Merging into `main` only produces a *snapshot*. To ship a version, cut `release/x.y.z`.

Every Release page offers three forms (Linux x86_64 only):

| Form | Best for | Notes |
|---|---|---|
| `tar.gz` | any x86_64 Linux, no root, air-gapped | extract and follow the bundled `README.md` / `install.sh`; default prefix `/usr/local/bin`, override with `--prefix` |
| `.deb` | Debian / Ubuntu | `sudo dpkg -i bifrost_<ver>-1_amd64.deb`, then pick a role with `bifrost-setup` |
| Source code | auditing or self-compiling | GitHub auto-generates `Source code (zip)` / `Source code (tar.gz)`; needs Rust 1.87+ |

## When a package is built

A push to `main` publishes the `latest` snapshot only when a **package input** changes:

- `src/**`, `Cargo.toml` / `Cargo.lock`, `build.rs`, `rust-toolchain*`, `.cargo/**`
- `scripts/**` (build / pack / install scripts), `skills/**` (the `SKILL.md` bundled in the tarball), `config/**`
- `LICENSE`

These changes update the repository but do **not** trigger a package build:

- `docs/**` (including architecture and advanced-usage docs), `examples/**`, `tests/**`
- root `README.md` / `README.zh-CN.md`, `AGENTS.md`
- `.github/**` (CI configuration itself)

A `release/x.y.z` branch must contain the version bump in `Cargo.toml`; otherwise it is filtered out too.
To force a build, run the workflow manually: `gh workflow run release.yml --ref <branch>`.

To skip packaging for a source change, put `[skip release]` in the pushed commit message (for a PR,
use the merge/squash commit message): gates/e2e still run, while the `dist` job is skipped. Manual
dispatch ignores the marker and always builds.

## Shipping a version

```bash
# 1. version lives in Cargo.toml only
sed -i 's/^version = "0.1.1"/version = "0.2.0"/' Cargo.toml

# 2. branch name must equal the version — the script hard-checks it
git switch -c release/0.2.0
git commit -am "chore(release): v0.2.0"
git push origin release/0.2.0
```

Actions then runs gates → e2e → tag `v0.2.0` → Release with four assets.

## One-time setup

GitHub Actions needs neither an imported pipeline nor an extra token:

- workflows live in `.github/workflows/` and take effect on push;
- publishing uses the repository `GITHUB_TOKEN`; `release.yml` declares
  `permissions: contents: write`;
- if org/repo policy pins Actions to read-only, allow write access under
  Settings → Actions → General.

## What runs

```
ci.yml (PRs / feature pushes / reused by release.yml)
① gates    cargo fmt --check / cargo clippy -D warnings / cargo test --all
             └─ a failure stops everything downstream
② e2e      cargo build --release + tests/e2e/run_all.py
             6 scenarios: timeout / job YAML / concurrent / multi-GPU pytest /
                          robustness / MCP stdio round-trip (all simulated, no real GPU)
release.yml (main / release/** push, or manual dispatch)
③ dist     scripts/ci/release.sh (channel chosen by BIFROST_BRANCH)
             main          → pack.sh --label latest --no-deb → rolls the tag=latest prerelease
             release/x.y.z → assert Cargo.toml version == x.y.z → pack.sh (with .deb)
                             → gh release create v<ver> + upload assets
```

Build logic lives in `scripts/ci/*.sh` and `scripts/dist/*.sh`; the workflows only
trigger and inject `BIFROST_PUBLISHER=github`, `BIFROST_BRANCH`, `BIFROST_REF`.

## Four hard rules (enforced in scripts, not by memory)

| Rule | Behaviour |
|---|---|
| **Official releases only from `release/x.y.z`** | `release.sh` exits non-zero on any other branch unless `--force-branch` |
| **Branch name == Cargo.toml version** | `release/0.3.0` still on 0.2.0 fails fast — no mislabelled artifacts |
| **No re-releasing a version** | an existing Release for that tag fails the build with "bump the version"; no more automatic `-b<build>` tags |
| **musl must be static** | no silent gnu fallback; pass `--allow-fallback` explicitly if you really need it |

## Assets

Each official release uploads four files:

| File | Use |
|---|---|
| `bifrost_<ver>-1_amd64.deb` | Debian/Ubuntu: `apt install ./xxx.deb` or `dpkg -i` |
| `bifrost-<ver>-x86_64-linux-musl.tar.gz` | portable package for any x86_64 Linux (contains `install.sh`) |
| two `.sha256` | checksums |

GitHub also attaches `Source code (zip)` / `Source code (tar.gz)` to every Release for source builds.

The target machine needs neither Rust nor a matching glibc:

```bash
sudo dpkg -i bifrost_<ver>-1_amd64.deb
bifrost-setup client --storage /data/liuxin/bifrost        # client
sudo bifrost-setup server --storage /data/liuxin/bifrost   # server (systemd)
```

## Snapshot channel (`latest`)

Every push to `main` rewrites the `tag=latest` prerelease, so there are stable URLs:

```bash
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz.sha256
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost_<ver>-1_amd64.deb
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost_<ver>-1_amd64.deb.sha256
```

Snapshots include the `.deb` too; the filename stays stable until the version bumps (stale files are pruned on upload).

`latest` is moved to the current `main` commit on every build (so the Release's Source code
archives stay current). The exact identity is still recorded in `BUILD_INFO.txt`:
`version`, `commit`, `branch`, `link`, `built_at`.

```bash
scripts/dist/bootstrap.sh \
  --from https://github.com/peterlau123/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz \
  --storage /data/liuxin/bifrost
```

For an official version, replace `latest` with `v<x.y.z>` and use the versioned filename.

## Manual release (when CI is unavailable)

```bash
# GitHub (gh logged in, or GH_TOKEN)
export BIFROST_PUBLISHER=github
export BIFROST_BRANCH=main            # detached HEAD / manual runs need this
export GITHUB_REPOSITORY=peterlau123/bifrost

bash scripts/ci/release.sh --dry-run              # print the plan, touch nothing
bash scripts/ci/release.sh --snapshot             # latest package + rolling release
bash scripts/ci/release.sh --release --no-publish # versioned package, no Release
BIFROST_BRANCH=release/0.2.0 bash scripts/ci/release.sh --release
bash scripts/ci/release.sh --release --force-branch --tag v0.2.0   # emergency override
```

`scripts/dist/publish-github.sh` accepts `--dry-run`. For Gitee publishing, leave
`BIFROST_PUBLISHER` unset (defaults to `gitee`) and set `GITEE_TOKEN`; that path uses
`scripts/dist/publish.sh` / `publish-latest.sh`.

## Verifying a run

```bash
gh run list --workflow release.yml
gh release view latest
gh release view v0.2.0
```

- success → pipeline worked
- `main` built but no `latest` → check the `publish-github.sh` step in the Actions log
  (snapshot publishing is best-effort and never fails `main`)
- `FAIL: gh 未登录` / `Resource not accessible by integration` → check `permissions`
  and the repo-level Actions write setting

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `当前分支 'xxx' 不允许发版` | channel rule: main → snapshot, release/x.y.z → official |
| `分支与 Cargo.toml 版本不一致` | align `Cargo.toml` `version` with `release/x.y.z` |
| `vX.Y.Z 已经发布过` | official versions are immutable: bump the version, cut a new branch |
| `Resource not accessible by integration` | repo policy overrides `permissions: contents: write` |
| `musl 静态构建失败` | intentional hard fail (no silent glibc package); use `--allow-fallback` to override |
| `latest` update failed | warned only, never fails `main` (snapshot publishing is best-effort) |
| slow/interrupted downloads | set `CARGO_REGISTRY_MIRROR=rsproxy` as an env in the workflow |
