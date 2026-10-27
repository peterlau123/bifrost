# Build & Release Pipeline (Gitee)

**Two channels — the branch is the channel.**

| Branch | What a push does | Artifacts | Official release? |
|---|---|---|---|
| `main` | gates → e2e → pack | `bifrost-latest-x86_64-linux-musl.tar.gz` + `.sha256`; also rolls the `tag=latest` prerelease | ❌ no |
| `release/x.y.z` | gates → e2e → version check → pack | `bifrost-<ver>-x86_64-linux-musl.tar.gz` + `.deb` + two `.sha256` | ✅ tag `v<x.y.z>` + Release |
| anything else | pipeline doesn't trigger; `release.sh` refuses anyway | — | — |

Merging into `main` only produces a *snapshot*. To ship a version, cut `release/x.y.z`.

## Shipping a version

```bash
# 1. version lives in Cargo.toml only
sed -i 's/^version = "0.1.0"/version = "0.2.0"/' Cargo.toml

# 2. branch name must equal the version — the pipeline hard-checks it
git checkout -b release/0.2.0
git commit -am "chore(release): v0.2.0"
git push origin release/0.2.0
```

## One-time setup (3 steps)

1. **Import the pipeline**: repo → Pipelines (Gitee Go) → New pipeline → "Import .workflow config" →
   pick `release.yml`. Build logic then lives in that file plus `scripts/ci/*.sh`, not in the UI.
2. **Add a token**: pipeline → Parameters → encrypted variable `GITEE_TOKEN`
   (Gitee → Settings → [personal access token](https://gitee.com/profile/personal_access_tokens),
   scope `projects`). It creates the tag, the release and uploads assets; without it the release
   channel fails with an explicit message.
3. **Delete the auto-generated pipeline**: `.workflow/流水线-202610221113.yml` came from a Gitee
   template (Rust 1.69 + deploy to a host group) and conflicts with this flow.

## What runs

```
① gates    cargo fmt --check / cargo clippy -D warnings / cargo test --all
             └─ a failure stops everything downstream
② e2e      cargo build --release + tests/e2e/run_all.py
             6 scenarios: timeout / job YAML / concurrent / multi-GPU pytest /
                          robustness / MCP stdio round-trip (all simulated, no real GPU)
③ dist     scripts/ci/release.sh (channel chosen by GITEE_BRANCH)
             main          → pack.sh --label latest --no-deb → rolls the tag=latest prerelease
             release/x.y.z → assert Cargo.toml version == x.y.z → pack.sh (with .deb)
                             → POST /tags (v<ver>) → POST /releases + attach_files
```

## Four hard rules (enforced in scripts, not by memory)

| Rule | Behaviour |
|---|---|
| **Official releases only from `release/x.y.z`** | `release.sh` exits non-zero on any other branch unless `--force-branch` |
| **Branch name == Cargo.toml version** | `release/0.3.0` still on 0.2.0 fails fast — no mislabelled artifacts |
| **No re-releasing a version** | an existing Release for that tag fails the build with "bump the version"; no more automatic `-b<build>` tags |
| **musl must be static** | no silent gnu fallback; pass `--allow-fallback` explicitly if you really need it |

## Assets

| File | Use |
|---|---|
| `bifrost_<ver>-1_amd64.deb` | Debian/Ubuntu: `apt install ./xxx.deb` or `dpkg -i` |
| `bifrost-<ver>-x86_64-linux-musl.tar.gz` | portable package for any x86_64 Linux (contains `install.sh`) |
| two `.sha256` | checksums |

The target machine needs neither Rust nor a matching glibc:

```bash
sudo dpkg -i bifrost_<ver>-1_amd64.deb
bifrost-setup client --storage /data/liuxin/bifrost        # client
sudo bifrost-setup server --storage /data/liuxin/bifrost   # server (systemd)
```

## Snapshot channel (`latest`)

Every push to `main` rewrites the `tag=latest` prerelease, so there is one stable URL:

```bash
https://gitee.com/peterlau/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz
```

`latest` is a rolling tag (it does not pin a commit). The real identity is inside `BUILD_INFO.txt`:
`version`, `commit`, `branch`, `link`, `built_at`.

```bash
scripts/dist/bootstrap.sh \
  --from https://gitee.com/peterlau/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz \
  --storage /data/liuxin/bifrost
```

For an official version, replace `latest` with `v<x.y.z>` and use the versioned filename.

## Manual release (when CI is unavailable)

```bash
export GITEE_TOKEN=<token>

bash scripts/ci/release.sh --dry-run              # print the plan, touch nothing
bash scripts/ci/release.sh --snapshot --no-publish   # latest package only (must be on main)
bash scripts/ci/release.sh --release              # official release (must be on release/x.y.z)
bash scripts/ci/release.sh --release --no-publish  # versioned package, no Release
bash scripts/ci/release.sh --release --force-branch --tag v0.2.0   # emergency override
```

Both `scripts/dist/publish.sh` and `publish-latest.sh` accept `--dry-run`.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `当前分支 'xxx' 不允许发版` | channel rule: main → snapshot, release/x.y.z → official |
| `分支与 Cargo.toml 版本不一致` | align `Cargo.toml` `version` with `release/x.y.z` |
| `vX.Y.Z 已经发布过` | official versions are immutable: bump the version, cut a new branch |
| `缺 GITEE_TOKEN` | missing encrypted variable, or the token lacks `projects` scope |
| `musl 静态构建失败` | intentional hard fail (no silent glibc package); use `--allow-fallback` to override |
| `latest` update failed | warned only, never fails `main` (snapshot publishing is best-effort) |
| slow/interrupted downloads | add pipeline parameter `CARGO_REGISTRY_MIRROR=rsproxy` |
