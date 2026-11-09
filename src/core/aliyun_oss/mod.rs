// Aliyun OSS bridge — backend-specific implementation of the HTTP transport
// with Aliyun Object Storage Service (OSS) as the backing blob store.
//
// Submodule navigation:
//   credentials: load OSS_ACCESS_KEY_ID/SECRET with env → file fallback
//   sign.rs:     HMAC-SHA1 request signing + OSS canonicalization rules
//   client.rs:   OssClient wrapping reqwest::blocking + sign; exposes
//                Bridge-trait-compatible methods (put/get/delete/list)
//
// 骨架 todos:
//   - Day 1 (this commit): credentials.rs complete with 8 fail-fast cases
//     tested against an isolation-env env guard. Settings schema now
//     accepts `listing_style: "oss"` and `http.oss.{bucket,region,endpoint,
//     ram_user}`. config-validate rejects oss-without-bucket.
//   - Day 2: sign.rs (awaits Ax's aliyun-oss SKILL.md copy) — official
//     4-example test cases as the acceptance gate before any client work.
//   - Day 3: client.rs (Bridge impl + write_path / delete_path helpers
//     mirroring the Nexus ones); mock round-trip tests.
//   - Day 4: fetcher trait generalization (default impls for SSH/HTTP
//     filename derivation) so OssBridge plugs into the existing
//     fetcher.rs::fetch_round without duplication.

pub mod credentials;
