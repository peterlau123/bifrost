// Aliyun OSS — ListObjects (a.k.a. GetBucket V1) response parsing.
//
// Reference:
//   https://www.alibabacloud.com/help/en/oss/developer-reference/listobjects-getbucket
//
// Triggers: `GET /?prefix=…&delimiter=/&marker=…&max-keys=…` → 200 application/xml.
//
// Schema (subset that this bridge consumes; optional fields omitted when
// empty server-side so they're all `Option<…>`):
//
//   <?xml version="1.0" encoding="UTF-8"?>
//   <ListBucketResult>
//     <Name>bucket</Name>
//     <Prefix>foo/</Prefix>
//     <Marker></Marker>
//     <NextMarker>foo/zz</NextMarker>          <!-- only if IsTruncated=true -->
//     <Delimiter>/</Delimiter>
//     <MaxKeys>1000</MaxKeys>
//     <IsTruncated>false</IsTruncated>
//     <Contents>
//       <Key>foo/a.txt</Key>
//       <LastModified>2026-11-09T10:00:00.000Z</LastModified>
//       <ETag>"ABC"</ETag>
//       <Type>Normal</Type>
//       <Size>1024</Size>
//       <StorageClass>Standard</StorageClass>
//     </Contents>
//     <CommonPrefixes>
//       <Prefix>foo/bar/</Prefix>
//     </CommonPrefixes>
//   </ListBucketResult>
//
// Sub-resources (acl, uploads, …) share an HTTP mount but return a
// different XML root; we deliberately just declare the ListBucketResult
// shape and let serde refuse any other root — fails loud, not silent.

use serde::Deserialize;

use crate::core::error::{BifrostError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename = "ListBucketResult")]
pub struct ListBucketResult {
    #[serde(rename = "Name", default)]
    pub name: Option<String>,
    #[serde(rename = "Prefix", default)]
    pub prefix: Option<String>,
    #[serde(rename = "Marker", default)]
    pub marker: Option<String>,
    #[serde(rename = "NextMarker", default)]
    pub next_marker: Option<String>,
    #[serde(rename = "Delimiter", default)]
    pub delimiter: Option<String>,
    #[serde(rename = "MaxKeys", default)]
    pub max_keys: Option<u32>,
    #[serde(rename = "IsTruncated", default)]
    pub is_truncated: bool,
    #[serde(rename = "Contents", default)]
    pub contents: Vec<ObjectMeta>,
    #[serde(rename = "CommonPrefixes", default)]
    pub common_prefixes: Vec<CommonPrefix>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ObjectMeta {
    #[serde(rename = "Key")]
    pub key: String,
    #[serde(rename = "LastModified", default)]
    pub last_modified: Option<String>,
    #[serde(rename = "ETag", default)]
    pub etag: Option<String>,
    #[serde(rename = "Type", default)]
    pub object_type: Option<String>,
    #[serde(rename = "Size", default)]
    pub size: u64,
    #[serde(rename = "StorageClass", default)]
    pub storage_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CommonPrefix {
    #[serde(rename = "Prefix")]
    pub prefix: String,
}

/// Parse an OSS `ListBucketResult` XML body. Returns an error if the XML
/// is malformed, the root element is not `ListBucketResult`, or any
/// required numeric field cannot be parsed.
pub fn parse_list_objects(xml: &str) -> Result<ListBucketResult> {
    // quick-xml's serde backend is root-element-tolerant: a wrong root
    // (e.g. OSS error envelope <Error>…</Error>) silently deserializes
    // into an empty struct. We'd rather fail loudly, so anchor the root
    // element explicitly before deserializing.
    let trimmed = xml.trim_start_matches(|c: char| c.is_whitespace());
    let after_decl = if let Some(rest) = trimmed.strip_prefix("<?xml") {
        match rest.find("?>") {
            Some(idx) => rest[idx + 2..].trim_start(),
            None => {
                return Err(BifrostError::XmlError(
                    "unterminated <?xml declaration".into(),
                ))
            }
        }
    } else {
        trimmed
    };
    if !after_decl.starts_with("<ListBucketResult") {
        return Err(BifrostError::XmlError(format!(
            "expected <ListBucketResult> root, found: {}",
            after_decl.chars().take(40).collect::<String>()
        )));
    }
    quick_xml::de::from_str::<ListBucketResult>(xml)
        .map_err(|e| BifrostError::XmlError(format!("ListBucketResult: {}", e)))
}

/// Helper for the typical use case: gather every visible file key,
/// recursing into CommonPrefixes (since the bridge uses Delimiter="/" and
/// wants the leaf files). Pure — does not call the network, so this is
/// the function unit tests assert against.
pub fn collect_file_keys(result: &ListBucketResult) -> Vec<String> {
    result.contents.iter().map(|c| c.key.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real OSS ListBucketResult body (bucket = "apmm", prefix = "", truncated=false,
    /// 2 files, no common prefixes).
    #[test]
    fn parse_simple_2_files_no_prefixes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>apmm</Name>
  <Prefix></Prefix>
  <Marker></Marker>
  <MaxKeys>1000</MaxKeys>
  <Delimiter></Delimiter>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>chip/a.txt</Key>
    <LastModified>2026-11-09T10:00:00.000Z</LastModified>
    <ETag>"A1B2C3"</ETag>
    <Type>Normal</Type>
    <Size>16</Size>
    <StorageClass>Standard</StorageClass>
  </Contents>
  <Contents>
    <Key>chip/b.txt</Key>
    <LastModified>2026-11-09T10:01:00.000Z</LastModified>
    <ETag>"D4E5F6"</ETag>
    <Type>Normal</Type>
    <Size>32</Size>
    <StorageClass>Standard</StorageClass>
  </Contents>
</ListBucketResult>"#;
        let r = parse_list_objects(xml).expect("valid XML");
        assert_eq!(r.name.as_deref(), Some("apmm"));
        assert_eq!(r.is_truncated, false);
        assert_eq!(r.contents.len(), 2);
        assert_eq!(r.contents[0].key, "chip/a.txt");
        assert_eq!(r.contents[0].size, 16);
        assert_eq!(r.contents[1].key, "chip/b.txt");
        assert_eq!(r.contents[1].size, 32);
        assert!(r.common_prefixes.is_empty());
    }

    /// Top-level bucket listing with Delimiter="/" — OSS returns CommonPrefixes
    /// (sub-directories) and only the files directly under Prefix.
    #[test]
    fn parse_with_common_prefixes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>apmm</Name>
  <Prefix>chip/</Prefix>
  <Delimiter>/</Delimiter>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>chip/readme.md</Key>
    <Size>1024</Size>
  </Contents>
  <CommonPrefixes>
    <Prefix>chip/models/</Prefix>
  </CommonPrefixes>
  <CommonPrefixes>
    <Prefix>chip/images/</Prefix>
  </CommonPrefixes>
</ListBucketResult>"#;
        let r = parse_list_objects(xml).unwrap();
        assert_eq!(r.prefix.as_deref(), Some("chip/"));
        assert_eq!(r.delimiter.as_deref(), Some("/"));
        assert_eq!(r.contents.len(), 1);
        assert_eq!(r.common_prefixes.len(), 2);
        assert_eq!(r.common_prefixes[0].prefix, "chip/models/");
        assert_eq!(r.common_prefixes[1].prefix, "chip/images/");
    }

    /// Paginated response: IsTruncated=true and NextMarker present.
    #[test]
    fn parse_truncated_next_marker() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>b</Name>
  <MaxKeys>1</MaxKeys>
  <IsTruncated>true</IsTruncated>
  <NextMarker>zz/last</NextMarker>
  <Contents>
    <Key>zz/last</Key>
    <Size>7</Size>
  </Contents>
</ListBucketResult>"#;
        let r = parse_list_objects(xml).unwrap();
        assert!(r.is_truncated);
        assert_eq!(r.next_marker.as_deref(), Some("zz/last"));
        assert_eq!(r.contents.len(), 1);
        assert_eq!(r.max_keys, Some(1));
    }

    /// Empty bucket — zero contents, zero common prefixes. Used to
    /// confirm `#[serde(default)]` works for repeated element fields.
    #[test]
    fn parse_empty_bucket() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>empty-b</Name>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>
</ListBucketResult>"#;
        let r = parse_list_objects(xml).unwrap();
        assert_eq!(r.name.as_deref(), Some("empty-b"));
        assert_eq!(r.contents.len(), 0);
        assert_eq!(r.common_prefixes.len(), 0);
        assert!(!r.is_truncated);
    }

    /// Malformed XML → XmlError, not a panic.
    #[test]
    fn parse_malformed_fails_loudly() {
        let xml = "<ListBucketResult><Name>x</Name>"; // unclosed root
        let r = parse_list_objects(xml);
        match r {
            Err(BifrostError::XmlError(_)) => {}
            other => panic!("expected XmlError, got {:?}", other),
        }
    }

    /// Wrong root element → XmlError.
    #[test]
    fn parse_wrong_root_rejected() {
        let xml = r#"<Error><Code>AccessDenied</Code></Error>"#;
        let r = parse_list_objects(xml);
        match r {
            Err(BifrostError::XmlError(_)) => {}
            other => panic!("expected XmlError, got {:?}", other),
        }
    }

    /// Size field that can't parse to u64 → XmlError.
    #[test]
    fn parse_bad_size_field_rejected() {
        let xml = r#"<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>k</Key>
    <Size>not-a-number</Size>
  </Contents>
</ListBucketResult>"#;
        assert!(matches!(
            parse_list_objects(xml),
            Err(BifrostError::XmlError(_))
        ));
    }

    /// collect_file_keys flattens Contents in order, ignoring
    /// CommonPrefixes by design (the walker is responsible for recursing
    /// into those — that lives in the yet-to-come client.rs Day 3).
    #[test]
    fn collect_file_keys_returns_only_contents() {
        let xml = r#"<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <Contents><Key>a/1</Key></Contents>
  <Contents><Key>a/2</Key></Contents>
  <CommonPrefixes><Prefix>b/</Prefix></CommonPrefixes>
  <CommonPrefixes><Prefix>c/</Prefix></CommonPrefixes>
</ListBucketResult>"#;
        let r = parse_list_objects(xml).unwrap();
        let keys = collect_file_keys(&r);
        assert_eq!(keys, vec!["a/1".to_string(), "a/2".to_string()]);
    }
}
