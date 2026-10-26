# Build & Release Pipeline (Gitee)

Every push to `main` → quality gates → static musl build → tag → Gitee Release with the
install package and its checksum.

## One-time setup (3 steps)

1. **Import the pipeline**: repo → Pipelines (Gitee Go) → New pipeline → "Import .workflow config" →
   pick `release.yml`. Build logic then lives in that file plus `scripts/ci/*.sh`, not in the UI.
2. **Add a token**: pipeline → Parameters → add an encrypted variable `GITEE_TOKEN`
   (Gitee → Settings → [personal access token](https://gitee.com/profile/personal_access_tokens),
   scope `projects`). It creates the tag, the release and uploads assets; without it CI fails
   with an explicit message.
3. **Delete the auto-generated pipeline**: `.workflow/流水线-202610221113.yml` came from a Gitee
   template (Rust 1.69 + deploy to a host group) and conflicts with this flow.

## What runs

```
stage gates (fmt / clippy -D warnings / cargo test --all)
  └─ failure blocks the release stage
stage release (scripts/ci/release.sh)
  ├─ scripts/dist/pack.sh        static musl binary → bifrost-<ver>-x86_64-linux-musl.tar.gz + .sha256
  ├─ POST /repos/{repo}/tags     tag = v<Cargo.toml version>; if already released → v<ver>-b<build>
  └─ POST /repos/{repo}/releases + attach_files
```

- **Single source of version** is `Cargo.toml`. Releasing = bump that line and merge to `main`.
- Rebuilding an existing version never overwrites it; it produces a `v0.1.0-b12`-style tag.
- If musl is unavailable the build falls back to gnu; the package name and `BUILD_INFO.txt` say so.

## Package contents

`bifrost` (static binary), `install.sh` (`--role client|server|both`), `bootstrap.sh`,
`README.md`, `SKILL.md`, `settings.example.json`, `LICENSE`, `BUILD_INFO.txt`.

Target machines need neither Rust nor a matching glibc:

```bash
tar xzf bifrost-<ver>-x86_64-linux-musl.tar.gz && cd bifrost-<ver>-x86_64-linux-musl
./install.sh --role client --storage /data/liuxin/bifrost       # client
sudo ./install.sh --role server --storage /data/liuxin/bifrost  # server (systemd)
```

Install straight from the release (client side):

```bash
scripts/dist/bootstrap.sh --from https://gitee.com/peterlau/bifrost/releases/download/<tag>/bifrost-<ver>-x86_64-linux-musl.tar.gz --storage /data/liuxin/bifrost
```

## Manual release (when CI is unavailable)

```bash
export GITEE_TOKEN=<token>
bash scripts/ci/release.sh                 # build + pack + tag + release
bash scripts/ci/release.sh --no-publish    # package only
bash scripts/dist/publish.sh --dist dist --tag v0.1.0 --dry-run   # inspect API calls
```

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `缺 GITEE_TOKEN` | encrypted pipeline variable missing or lacking `projects` scope |
| tag already exists | expected: an already-released version switches to `v<ver>-b<build>` |
| rustc too old | `scripts/ci/toolchain.sh` probes and installs stable (Gitee templates ship 1.69) |
| musl target unavailable | build falls back to gnu, package name carries `-gnu` |
| fmt/clippy fails | run `cargo fmt && cargo clippy --all-targets -- -D warnings` locally first |
| slow dependency fetch | add `CARGO_REGISTRY_MIRROR=rsproxy` as a pipeline parameter |
