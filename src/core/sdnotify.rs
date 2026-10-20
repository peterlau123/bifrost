//! sd_notify —— 对 systemd 的 就绪/状态通知（`--systemd` 时才发）。
//!
//! 为什么需要：unit 用 `Type=simple` 时，`systemctl start/restart` 的语义只是
//! "fork 成功"，daemon 可能还在建目录、探测时钟、还没开始 watch commands 就返回了。
//! 调用方只能自己轮询 heartbeat 来猜就绪。用 `Type=notify` + 这里的 `READY=1`，
//! systemd 会等真正就绪才认为启动完成，卡死则由 `TimeoutStartSec` 兜住。
//!
//! 不引依赖：sd_notify 协议就是往 `$NOTIFY_SOCKET`（一个 AF_UNIX datagram socket，
//! 路径名或 `@` 开头的抽象命名空间）发一行行 `KEY=VALUE`。

use std::io;

/// 一次通知的协议载荷（每行一个 `KEY=VALUE`）。
pub fn notify(payload: &str) -> Result<(), String> {
    let target = std::env::var("NOTIFY_SOCKET").map_err(|_| "NOTIFY_SOCKET 未设置".to_string())?;
    if target.is_empty() {
        return Err("NOTIFY_SOCKET 为空".to_string());
    }
    // systemd 用 '@' 表示抽象命名空间（socket 里是前导 NUL 而非文件路径）
    let (name, is_abstract) = match target.strip_prefix('@') {
        Some(rest) => (rest, true),
        None => (target.as_str(), false),
    };
    let name_bytes = name.as_bytes();
    if name_bytes.len() + 1 >= SUN_PATH_LEN {
        return Err(format!(
            "NOTIFY_SOCKET 过长（{} 字节，上限 {}）",
            name_bytes.len(),
            SUN_PATH_LEN - 1
        ));
    }

    unsafe {
        let fd = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(format!("socket(): {}", io::Error::last_os_error()));
        }

        let mut addr: libc::sockaddr_un = std::mem::zeroed();
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;

        // sun_path 在 sockaddr_un 里的偏移（不写死 2，避免平台假设）
        let base = &addr as *const libc::sockaddr_un as usize;
        let path_off = &addr.sun_path as *const _ as usize - base;
        let dst = addr.sun_path.as_mut_ptr() as *mut u8;

        let addr_len = if is_abstract {
            *dst = 0; // 抽象命名空间：首字节为 NUL，后接名字
            std::ptr::copy_nonoverlapping(name_bytes.as_ptr(), dst.add(1), name_bytes.len());
            path_off + 1 + name_bytes.len()
        } else {
            std::ptr::copy_nonoverlapping(name_bytes.as_ptr(), dst, name_bytes.len());
            *dst.add(name_bytes.len()) = 0; // 路径名需要 NUL 结尾
            path_off + name_bytes.len() + 1
        };

        // 载荷按行组织；systemd 不要求结尾换行，但补上更稳
        let mut msg = String::with_capacity(payload.len() + 1);
        msg.push_str(payload);
        if !msg.ends_with('\n') {
            msg.push('\n');
        }

        let rc = libc::sendto(
            fd,
            msg.as_ptr() as *const libc::c_void,
            msg.len(),
            0,
            &addr as *const libc::sockaddr_un as *const libc::sockaddr,
            addr_len as libc::socklen_t,
        );
        let err = io::Error::last_os_error();
        libc::close(fd);

        if rc < 0 {
            Err(format!("sendto({}): {}", target, err))
        } else {
            Ok(())
        }
    }
}

/// `sun_path` 的容量（Linux 上是 108）。
const SUN_PATH_LEN: usize = 108;

/// 就绪上报：`READY=1` + `STATUS=`（STATUS 会显示在 `systemctl status` 里）。
pub fn ready(status: &str) -> Result<(), String> {
    notify(&format!("READY=1\nSTATUS={}", sanitize(status)))
}

/// 正在停止：让 systemd 知道这是优雅退出，而不是崩溃。
pub fn stopping() -> Result<(), String> {
    notify("STOPPING=1")
}

/// STATUS= 是单行字段，任务名/路径里的换行会把协议撑破。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixDatagram;
    use std::sync::Mutex;

    // NOTIFY_SOCKET 是进程级环境变量，测试之间必须串行；某个测试 panic 后
    // 锁会被 poison，后面用 into_inner 兜住，否则一个失败会连坐其余全部。
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn recv_one(sock: &UnixDatagram) -> String {
        sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut buf = [0u8; 512];
        let n = sock.recv(&mut buf).expect("recv 超时/失败");
        String::from_utf8_lossy(&buf[..n]).to_string()
    }

    #[test]
    fn test_ready_sends_datagram() {
        let _g = guard();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("notify.sock");
        let sock = UnixDatagram::bind(&path).unwrap();

        std::env::set_var("NOTIFY_SOCKET", &path);
        let res = ready("watching /data/liuxin/bifrost/commands");
        std::env::remove_var("NOTIFY_SOCKET");
        res.unwrap();

        let msg = recv_one(&sock);
        assert!(msg.contains("READY=1"), "payload: {msg}");
        assert!(
            msg.contains("STATUS=watching /data/liuxin/bifrost/commands"),
            "payload: {msg}"
        );
    }

    #[test]
    fn test_stopping_sends_datagram() {
        let _g = guard();
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("notify2.sock");
        let sock = UnixDatagram::bind(&path).unwrap();

        std::env::set_var("NOTIFY_SOCKET", &path);
        let res = stopping();
        std::env::remove_var("NOTIFY_SOCKET");
        res.unwrap();

        assert_eq!(recv_one(&sock), "STOPPING=1\n");
    }

    /// 抽象命名空间（'@' 前缀）：std 的 UnixDatagram 不允许路径里带 NUL，
    /// 所以这里直接用 libc 建 socket，顺便覆盖 notify() 的抽象分支。
    #[test]
    fn test_abstract_namespace_socket() {
        let _g = guard();
        let name = format!("bifrost-test-{}", uuid::Uuid::new_v4());
        let fd = bind_abstract(&name);

        std::env::set_var("NOTIFY_SOCKET", format!("@{name}"));
        let res = ready("abstract");
        std::env::remove_var("NOTIFY_SOCKET");
        let payload = recv_fd(fd);
        unsafe { libc::close(fd) };
        res.unwrap();

        assert!(payload.contains("READY=1"), "payload: {payload}");
        assert!(payload.contains("STATUS=abstract"), "payload: {payload}");
    }

    #[test]
    fn test_no_socket_is_an_error_not_a_panic() {
        let _g = guard();
        std::env::remove_var("NOTIFY_SOCKET");
        assert!(ready("x").is_err());
    }

    #[test]
    fn test_status_newlines_are_flattened() {
        assert_eq!(sanitize("a\nb\r\nc"), "a b  c");
    }

    /// 绑定一个抽象命名空间 datagram socket，返回 fd。
    fn bind_abstract(name: &str) -> libc::c_int {
        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_DGRAM, 0);
            assert!(fd >= 0, "socket: {}", io::Error::last_os_error());
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            let base = &addr as *const libc::sockaddr_un as usize;
            let off = &addr.sun_path as *const _ as usize - base;
            let dst = addr.sun_path.as_mut_ptr() as *mut u8;
            *dst = 0;
            std::ptr::copy_nonoverlapping(name.as_ptr(), dst.add(1), name.len());
            let len = (off + 1 + name.len()) as libc::socklen_t;
            let rc = libc::bind(
                fd,
                &addr as *const libc::sockaddr_un as *const libc::sockaddr,
                len,
            );
            assert_eq!(rc, 0, "bind abstract: {}", io::Error::last_os_error());
            fd
        }
    }

    fn recv_fd(fd: libc::c_int) -> String {
        unsafe {
            let tv = libc::timeval {
                tv_sec: 5,
                tv_usec: 0,
            };
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                &tv as *const libc::timeval as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
            let mut buf = [0u8; 512];
            let n = libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0);
            assert!(n > 0, "recv: {}", io::Error::last_os_error());
            String::from_utf8_lossy(&buf[..n as usize]).to_string()
        }
    }
}
