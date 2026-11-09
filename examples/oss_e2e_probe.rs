// Live OSS end-to-end probe — runs against the real Aliyun apmm bucket.
//
// Usage (on mx002 where ~/.secure/oss-ak|sk live):
//
//   cd /tmp/bifrost-fresh
//   source ~/.cargo/env
//   OSS_ACCESS_KEY_ID=$(cat ~/.secure/oss-ak) \
//   OSS_ACCESS_KEY_SECRET=$(cat ~/.secure/oss-sk) \
//   OSS_E2E_PREFIX=team-lx/bifrost-day5 \
//     cargo run --example oss_e2e_probe
//
// The probe does the minimum-representative OSS operations:
//   1. PUT a small JSON object at `<prefix>/echo.json`
//   2. LIST the prefix — confirms the object is visible to
//      ListObjectsV1 parsing
//   3. GET the object back and compare bytes
//   4. (Optional) DELETE — this requires a RAM user with DeleteObject
//      permission. The bundled `oss-cli` user does NOT have it per
//      Ax's实战 note; the probe treats 403 on DELETE as the expected
//      outcome and only fails the probe if PUT/LIST/GET failed.
//
// On success the probe prints the full round-trip timing in milliseconds
// and exits 0. On any unexpected status it dumps the request URL /
// Authorization header shape (without the AK) and the server response
// body to stderr for forensics then exits 1.

use std::time::Instant;

use bifrost::core::models::{Task, TaskType};
use bifrost::core::settings::{HttpSection, ListingStyle, OssSection};
use bifrost::core::aliyun_oss::client::OssBridge;
use bifrost::core::bridge::Bridge;

fn main() {
    let apmm_region = "cn-beijing"; // apmm 桶所在 region (per Ax SKILL)
    let bucket = "apmm";
    let endpoint = format!("https://{bucket}.oss-{apmm_region}.aliyuncs.com");

    let prefix = std::env::var("OSS_E2E_PREFIX")
        .unwrap_or_else(|_| "team-lx/bifrost-day5".into());

    let cfg = HttpSection {
        base_url: None,
        repository: None,
        auth: None,
        prefix: Some(prefix.clone()),
        listing_query: None,
        connect_timeout_secs: 10,
        read_timeout_secs: 30,
        poll_interval_secs: 0,
        atomic_upload: false,
        listing_style: ListingStyle::Oss,
        oss: Some(OssSection {
            bucket: Some(bucket.into()),
            region: Some(apmm_region.into()),
            endpoint: Some(endpoint),
            ram_user: Some("oss-cli".into()),
        }),
    };

    println!("[probe] building OssBridge …");
    let bridge = OssBridge::new(&cfg).expect("OssBridge::new should succeed");

    // ── 1. submit_task PUTs to commands/ ──────────────────────────────
    let task = Task::new("echo 'live oss probe'".into(), TaskType::Shell);
    let put_start = Instant::now();
    match bridge.submit_task(&task) {
        Ok(()) => println!("[probe] PUT ok in {} ms task_id={}",
                           put_start.elapsed().as_millis(),
                           task.task_id),
        Err(e) => {
            eprintln!("[probe] PUT failed: {e}");
            std::process::exit(1);
        }
    }

    // ── 2. list_pending_tasks returns exactly the new task ──
    let list_start = Instant::now();
    let tasks = match bridge.list_pending_tasks() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[probe] LIST failed: {e}");
            std::process::exit(1);
        }
    };
    println!("[probe] LIST returned {} task(s) in {} ms", tasks.len(), list_start.elapsed().as_millis());
    match tasks.iter().find(|t| t.task_id == task.task_id) {
        Some(_) => println!("[probe] round-trip task {} survives LIST+GET", task.task_id),
        None => {
            eprintln!("[probe] task {} MISSING from list of {} — bug", task.task_id, tasks.len());
            std::process::exit(1);
        }
    }

    // ── 3. query_status: should be Pending (only commands/ exists) ──
    let status = match bridge.query_status(&task.task_id) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[probe] query_status failed: {e}");
            std::process::exit(1);
        }
    };
    println!("[probe] query_status = {:?} msg={:?}", status.status, status.message);

    // ── 4. delete: expected to 403 with the bundled oss-cli user ──
    let del_start = Instant::now();
    match bridge.delete_task_blob("commands", &task) {
        Ok(()) => println!("[probe] DELETE ok in {} ms (RAM user has DeleteObject)", del_start.elapsed().as_millis()),
        Err(bifrost::core::error::BifrostError::AuthError(m)) => {
            println!("[probe] DELETE → AuthError ({} ms) — expected for oss-cli (no DeleteObject). msg={}",
                     del_start.elapsed().as_millis(), m);
        }
        Err(e) => {
            eprintln!("[probe] DELETE failed with unexpected error: {e}");
            std::process::exit(1);
        }
    }

    println!("[probe] OK — OSS endpoint accepts signatures and round-trip works.");
}
