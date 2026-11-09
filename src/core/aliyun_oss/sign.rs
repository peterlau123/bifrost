// Aliyun OSS REST API — Signature Version 1 (Header Authorization).
//
// Reference:
//   https://www.alibabacloud.com/help/en/oss/developer-reference/include-signatures-in-the-authorization-header
//
// Formula:
//   Authorization = "OSS " + AccessKeyId + ":" + Signature
//   Signature     = Base64(HMAC-SHA1(AccessKeySecret, StringToSign))
//   StringToSign  = VERB + "\n"
//                 + Content-MD5 + "\n"
//                 + Content-Type + "\n"
//                 + Date + "\n"
//                 + CanonicalizedOSSHeaders
//                 + CanonicalizedResource
//
// CanonicalizedOSSHeaders
//   = concat over headers whose name starts with "x-oss-":
//       lower(name) + ":" + trim(value) + "\n"
//     sorted by lower(name) lexicographically.
//   The trailing "\n" of the LAST header is part of the value;
//   if there are no x-oss-* headers the string is empty (no extra "\n"
//   either — the final "\n" belongs to the Date field, not this one).
//   When x-oss-date is set, the Date field in StringToSign is the empty
//   string (×STS requirement), with a separate x-oss-date header added
//   to the canonical set.
//
// CanonicalizedResource
//   = "/" + bucket + "/" + key?            (bucket-level ops: "/" + bucket + "/")
//     sub-resources (whitelist only) are appended as ";key" or "?key=value"
//     pairs. The bridge only ever emits List/Get/Put/Delete — never sub-
//     resources — so this implementation passes a pre-built resource path
//     in and does not try to be clever about the whitelist.
//
// Date
//   = "Wed, 28 Dec 2022 10:27:41 GMT" (RFC1123, GMT). Must be within 15min
//     of server time or OSS returns 403.
//
//   **Note on the official documentation examples**: aliyun masks the
//   signature's tail in the worked examples (e.g. "J9Nl***…"). The real
//   AccessKeySecret used to compute them is not published. Therefore we
//   cannot byte-equal-against the documented prefix; instead we use
//   anchor-verified parameter rules + self-generated golden vectors as
//   the regression gate. Real-server "4-example" agreement is deferred
//   to the Day-5 end-to-end test against the user's apmm bucket.

use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// Inputs that define the signed string. `content_md5`, `content_type`,
/// `date`, and the header list MUST be passed in already populated; we
/// never invent canonical values because accidental "helpful" defaults
/// are exactly where signature mismatches come from.
#[derive(Debug, Clone)]
pub struct SignRequest<'a> {
    pub verb: &'a str,                 // e.g. "PUT" — uppercase, caller must verify
    pub content_md5: Option<&'a str>,  // base64(128-bit MD5) or None → ""
    pub content_type: Option<&'a str>, // MIME or None → ""
    pub date: &'a str,                 // RFC1123 GMT; empty iff x-oss-date present
    pub oss_headers: &'a [(&'a str, &'a str)], // already-prefixed x-oss-* pairs
    pub resource: &'a str,             // "/bucket/key" — fully built
}

/// Compute the StringToSign. Pure string work — kept separate so unit
/// tests can assert the exact bytes without going through HMAC.
pub fn string_to_sign(req: &SignRequest<'_>) -> String {
    let mut out = String::with_capacity(256);
    out.push_str(req.verb);
    out.push('\n');
    out.push_str(req.content_md5.unwrap_or(""));
    out.push('\n');
    out.push_str(req.content_type.unwrap_or(""));
    out.push('\n');
    // If the caller stored the canonical date under x-oss-date, the Date
    // placeholder MUST be the empty string (v1 STS rule). We accept the
    // empty date string from the caller rather than overload semantics
    // by inspecting oss_headers; the caller is responsible for setting
    // date = "" when an x-oss-date pair was passed.
    out.push_str(req.date);
    out.push('\n');
    out.push_str(&canonicalized_oss_headers(req.oss_headers));
    out.push_str(req.resource);
    out
}

/// Build CanonicalizedOSSHeaders from a (name, value) slice. Caller may
/// pass headers in any case; we lowercase names in-place, trim value
/// whitespace, and sort lexicographically by lowercased name.
pub fn canonicalized_oss_headers(headers: &[(&str, &str)]) -> String {
    let mut items: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = String::new();
    for (k, v) in items {
        out.push_str(&k);
        out.push(':');
        out.push_str(&v);
        out.push('\n');
    }
    out
}

/// Sign the request. Returns the value of the `Authorization` HTTP
/// header in the form `"OSS <AccessKeyId>:<base64-signature>"`.
pub fn sign_v1(access_key_id: &str, access_key_secret: &str, req: &SignRequest<'_>) -> String {
    let sts = string_to_sign(req);
    let mut mac =
        HmacSha1::new_from_slice(access_key_secret.as_bytes()).expect("HMAC takes any length key");
    mac.update(sts.as_bytes());
    let digest = mac.finalize().into_bytes();
    let sig = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, digest);
    format!("OSS {}:{}", access_key_id, sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- canonicalized_oss_headers rules --------------------------------

    #[test]
    fn canon_headers_lower_sort_trim_trailing_newline() {
        // Documented recipe from the aliyun page: lowercase, sort by name,
        // strip spaces around ':', final '\n' after each header.
        let h = [
            ("X-OSS-Meta-Magic", "abracadabra"),
            ("x-oss-meta-author", "alice "),
        ];
        let s = canonicalized_oss_headers(&h);
        assert_eq!(s, "x-oss-meta-author:alice\nx-oss-meta-magic:abracadabra\n");
    }

    #[test]
    fn canon_headers_empty_when_no_x_oss_headers() {
        let s = canonicalized_oss_headers(&[]);
        assert_eq!(
            s, "",
            "CanonicalizedOSSHeaders must be empty string (no trailing \\n)"
        );
    }

    #[test]
    fn canon_headers_single_header_has_trailing_newline() {
        let s = canonicalized_oss_headers(&[("x-oss-meta-a", "1")]);
        assert_eq!(s, "x-oss-meta-a:1\n");
    }

    #[test]
    fn canon_headers_unsorted_input_is_sorted() {
        let h = [
            ("x-oss-meta-z", "z"),
            ("x-oss-meta-a", "a"),
            ("x-oss-meta-m", "m"),
        ];
        let s = canonicalized_oss_headers(&h);
        assert_eq!(s, "x-oss-meta-a:a\nx-oss-meta-m:m\nx-oss-meta-z:z\n");
    }

    // ---- string_to_sign shape -------------------------------------------

    #[test]
    fn sts_example1_full_documented_shape() {
        // Example 1 from include-signed-headers-in-the-authorization-header.
        // The real Content-MD5 used by the doc example is the Base64 of
        // md5(b"0123456789"), which the doc lists as eB5e****… (masked).
        let req = SignRequest {
            verb: "PUT",
            content_md5: Some("eB5eJF1ptWaXm4bijSPyxw=="),
            content_type: Some("text/html"),
            date: "Wed, 28 Dec 2022 10:27:41 GMT",
            oss_headers: &[
                ("x-oss-meta-author", "alice"),
                ("x-oss-meta-magic", "abracadabra"),
            ],
            resource: "/oss-example/nelson",
        };
        let sts = string_to_sign(&req);
        // Note on Rust `\<newline>` string continuation: leading whitespace
        // on the next line is stripped, so the literal below has the exact
        // bytes "PUT\neB5e...==\ntext/html\nWed, …". We do NOT trim twice.
        let expect = "PUT\neB5eJF1ptWaXm4bijSPyxw==\ntext/html\nWed, 28 Dec 2022 10:27:41 GMT\n\
                      x-oss-meta-author:alice\nx-oss-meta-magic:abracadabra\n/oss-example/nelson";
        assert_eq!(sts, expect);
    }

    #[test]
    fn sts_example2_optional_fields_blank_lines_kept() {
        // Example 2: Content-MD5 and Content-Type omitted → empty strings
        // but their "\n" separators are still present.
        let req = SignRequest {
            verb: "PUT",
            content_md5: None,
            content_type: None,
            date: "Wed, 28 Dec 2022 09:56:32 GMT",
            oss_headers: &[
                ("x-oss-meta-magic", "abracadabra"),
                ("x-oss-meta-author", "alice"),
            ],
            resource: "/oss-example/nelson",
        };
        let sts = string_to_sign(&req);
        let expect = "PUT\n\n\nWed, 28 Dec 2022 09:56:32 GMT\n\
                      x-oss-meta-author:alice\nx-oss-meta-magic:abracadabra\n/oss-example/nelson"
            .lines()
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(sts, expect);
    }

    #[test]
    fn sts_no_headers_canonicalized_oss_section_empty() {
        let req = SignRequest {
            verb: "GET",
            content_md5: None,
            content_type: None,
            date: "Wed, 11 May 2011 07:59:25 GMT",
            oss_headers: &[],
            resource: "/examplebucket/",
        };
        let sts = string_to_sign(&req);
        // "Date\n" then directly "/examplebucket/" — no extra blank line.
        assert_eq!(
            sts,
            "GET\n\n\nWed, 11 May 2011 07:59:25 GMT\n/examplebucket/"
        );
    }

    // ---- sign_v1 output format ------------------------------------------

    #[test]
    fn sign_v1_returns_oss_prefix_and_base64() {
        let req = SignRequest {
            verb: "PUT",
            content_md5: None,
            content_type: None,
            date: "Wed, 28 Dec 2022 09:56:32 GMT",
            oss_headers: &[],
            resource: "/oss-example/nelson",
        };
        let auth = sign_v1("test-ak-id", "test-ak-secret", &req);
        assert!(auth.starts_with("OSS test-ak-id:"));
        // 20 bytes SHA-1 → 28 base64 chars (with trailing padding "=").
        let b64 = auth.split(':').nth(1).unwrap();
        assert_eq!(b64.len(), 28);
        assert!(b64.ends_with('='));
        // Every char is base64 alphabet.
        assert!(b64
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c)));
    }

    #[test]
    fn sign_v1_is_deterministic() {
        let req = SignRequest {
            verb: "DELETE",
            content_md5: None,
            content_type: None,
            date: "Wed, 28 Dec 2022 09:56:32 GMT",
            oss_headers: &[("x-oss-meta-x", "1")],
            resource: "/bucket/key.txt",
        };
        let a = sign_v1("id", "sk", &req);
        let b = sign_v1("id", "sk", &req);
        assert_eq!(a, b);
    }

    #[test]
    fn sign_v1_differs_when_secret_differs() {
        let req = SignRequest {
            verb: "GET",
            content_md5: None,
            content_type: None,
            date: "Wed, 28 Dec 2022 09:56:32 GMT",
            oss_headers: &[],
            resource: "/b/",
        };
        assert_ne!(sign_v1("i", "sk-one", &req), sign_v1("i", "sk-two", &req));
    }

    // Golden anchor: re-derived using `python3 hmac` cross-check (Python
    // and Rust agree byte-for-byte on the same StringToSign).
    #[test]
    fn sign_v1_golden_anchor_python_hmac_match() {
        // Independently reproducible by running this in Python:
        //   >>> import hmac, hashlib, base64
        //   >>> sts = "PUT\n\n\nWed, 28 Dec 2022 09:56:32 GMT\n" \
        //   ...       "x-oss-meta-author:alice\nx-oss-meta-magic:abracadabra\n" \
        //   ...       "/oss-example/nelson"
        //   >>> base64.b64encode(hmac.new(b"k", sts.encode(), hashlib.sha1).digest())
        //   b'SApKVPAj5zYFinS35gNDakVgnUE='
        let req = SignRequest {
            verb: "PUT",
            content_md5: None,
            content_type: None,
            date: "Wed, 28 Dec 2022 09:56:32 GMT",
            oss_headers: &[
                ("x-oss-meta-author", "alice"),
                ("x-oss-meta-magic", "abracadabra"),
            ],
            resource: "/oss-example/nelson",
        };
        let auth = sign_v1("ak", "k", &req);
        assert_eq!(auth, "OSS ak:SApKVPAj5zYFinS35gNDakVgnUE=");
    }
}
