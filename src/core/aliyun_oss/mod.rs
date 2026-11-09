// Aliyun OSS bridge — backend-specific implementation of the HTTP transport
// with Aliyun Object Storage Service (OSS) as the backing blob store.
//
// Submodule navigation:
//   credentials: load OSS_ACCESS_KEY_ID/SECRET with env → file fallback
//   sign.rs:     HMAC-SHA1 request signing + OSS canonicalization rules
//   list.rs:     ListObjects (GetBucket V1) XML response parsing
//   client.rs:   OssClient wrapping reqwest::blocking + sign; exposes
//                Bridge-trait-compatible methods (put/get/delete/list)
//
// Day-2 status (this commit):
//   - sign.rs: V1 StringToSign + canonical-x-oss-headers per contract
//     (lowercase names, sorted, no spaces around ':', trailing "\n").
//     Official-doc examples mask the signature AND the AccessKeySecret,
//     so a literal byte-equal prefix test is impossible; we instead
//     assert the StringToSign byte-shape against the doc examples and
//     cross-check the HMAC path with a Python-computed golden anchor
//     for one full request. Real-server "4-example" agreement is
//     deferred to the Day-5 end-to-end test against the apmm bucket.
//   - list.rs: ListBucketResult / ObjectMeta / CommonPrefix types.
//     quick-xml tolerates a wrong root element so we anchor-examine
//     the first opening tag before deserializing — turns silent
//     `<Error>` mis-parses into BifrostError::XmlError.
//   - error.rs: +XmlError variant; this was the first non-IO, non-
//     Config error surfaced from a transport parser.
//
// Remaining skeleton:
//   - Day 3: client.rs (Bridge impl + write_path / delete_path helpers
//     mirroring the Nexus ones); mock round-trip tests.
//   - Day 4: fetcher trait generalization (default impls for SSH/HTTP
//     filename derivation) so OssBridge plugs into the existing
//     fetcher.rs::fetch_round without duplication.
//   - Day 5: live e2e against apmm bucket (oss-cli fixture exists at
//     /tmp/aliyun-skill/aliyun-oss/SKILL.md for environment notes).

pub mod credentials;
pub mod list;
pub mod sign;
