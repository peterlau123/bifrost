// Aliyun OSS bridge — backend-specific implementation of the HTTP transport
// with Aliyun Object Storage Service (OSS) as the backing blob store.
//
// Submodule navigation:
//   credentials: load OSS_ACCESS_KEY_ID/SECRET with env → file fallback
//   sign.rs:     HMAC-SHA1 request signing + OSS canonicalization rules
//   list.rs:     ListObjects (GetBucket V1) XML response parsing
//   client.rs:   OssBridge impl of Bridge trait — PUT/GET/DELETE/LIST
//                against the OSS endpoint, signed per Signature V1. The
//                fetcher.rs' `bridge.delete_task_blob(...)` dispatch
//                works because we override `task_blob_name` to produce
//                the same `<ts>_<id>.json` shape HttpBridge uses.
//
// Day-3 status (this commit):
//   - client.rs implements the full Bridge trait surface:
//     * submit_task PUTs `<prefix>/commands/<ts>_<id>.json`
//     * query_status three-tier probe: results/ → status/ → LIST commands/
//     * get_result / write_result / write_status / remove_task /
//       cancel_task / read_task / list_pending_tasks — all wired
//     * `task_blob_name` override keeps the timestamp prefix so DELETE
//       targets the exact object PUT created (D5 fix mirroring HttpBridge)
//     * `create_bridge()` in bridge.rs now dispatches on
//       `ListingStyle::Oss` instead of always instantiating HttpBridge
//   - 3 mock HTTP end-to-end tests against an in-process TcpListener
//     OSS server (PUT→LIST→GET→DELETE round-trip; 403→AuthError path;
//     ListBucketResult parsing). No HMAC or wall-clock assertions there
//     because sign.rs owns HMAC and Date uses Utc::now(); Day 5 e2e
//     validates the real endpoint.
//   - OSS-style `Authorization: OSS <id>:<sig>` header and `Date:
//     <RFC1123 GMT>` are stamped on every request and asserted on
//     every request captured by the loopback mock.
//
// Day-2 status (prior commit):
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
//   - Day 4: fetcher trait generalization (default impls for SSH/HTTP
//     filename derivation) was already done in Day 1, so all that's
//     left is pulling the cancellation scanner forward (currently a
//     stub at fetcher.rs::cancels_for_visible_tasks returns Ok(()) —
//     OSS could trivially implement listing for `<prefix>/cancel/`).
//   - Day 5: live e2e against apmm bucket (oss-cli fixture exists at
//     /tmp/aliyun-skill/aliyun-oss/SKILL.md for environment notes).

pub mod client;
pub mod credentials;
pub mod list;
pub mod sign;
