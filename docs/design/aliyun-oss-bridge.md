# Aliyun OSS Bridge 设计草案（feature/aliyun-oss-bridge）

> **状态**：草案等刘鑫 review gate。未经批准备**不动代码**。
> 已从 `feature/http-bridge @ 0e2785b` 切出；该分支以 HTTP bridge 为底座。

## 0. 用户硬约束（2026-11-09 锁定）

| 维度 | 用户指定 |
|---|---|
| 凭据接入 | **选项 C**：环境变量 + 配置文件双路，ENV 优先 |
| SDK 选择 | **自实现**（HMAC-SHA1 + ListObjectsV2 XML） |
| 工作区 | `apmm/relay/bifrost-work` |
| 区域 | cn-beijing |
| Bucket | `apmm` |
| RAM 用户 | `oss-cli` |

## 1. 模块拆分

```
src/core/aliyun_oss/
├── auth.rs           # 凭据解析（ENV → 文件 fallback）
├── sign.rs           # HMAC-SHA1 请求签名
├── list.rs           # ListObjectsV2 XML 解析
├── client.rs         # OSS 客户端骨架 (reqwest::blocking 封装)
└── error.rs          # 错误归类 → BifrostError
```

复用既有骨架：
- 沿用 `Bridge` trait（同一份生命周期语义，G1 daemon 不改）
- 沿用 `listing_style` 枚举（新增 `"oss"` 分支）
- 沿用 D9 write-ahead ledger（fetcher 同样语义）
- 沿用 D7 fetcher first-round log、D8 pagination 字段语义

## 2. 配置 schema 扩展

[http] 段新增与 OSS 用途对应字段（无冲突，nesting 风格与现有 auth 保持一致）：

```json
{
  "transport": "http",
  "http": {
    "base_url": "",
    "prefix": "relay/bifrost-work",
    "listing_style": "oss",
    "oss": {
      "bucket": "apmm",
      "region": "cn-beijing",
      "endpoint": "https://apmm.oss-cn-beijing.aliyuncs.com",
      "ram_user": "oss-cli"
    },
    "connect_timeout_secs": 10,
    "read_timeout_secs": 30,
    "poll_interval_secs": 10,
    "atomic_upload": false
  }
}
```

**约束**：

- `listing_style="oss"` + base_url 此时可不强制 `https`（OSS endpoint 本身决定 TLS 状态）
- `http.oss` 段：当 listing_style="oss" 时**必填**，缺失即配置期报错
- `http.oss.endpoint` 不出现时按 `https://<bucket>.oss-<region>.aliyuncs.com` 推导

## 3. 凭据加载（选项 C 实现）

优先级（超过配置文件的跨字段冲突直接报错而非 merge）：

1. `OSS_ACCESS_KEY_ID` + `OSS_ACCESS_KEY_SECRET` 环境变量
2. `~/.aliyun/oss.json`（如缺，则再 fallback 到 settings.json 同目录的 `oss.json`）
3. （不设 settings.json 明文字段——**不把凭据写进 repo**）

`~/.aliyun/oss.json` 形态：

```json
{
  "access_key_id": "LTAI...",
  "access_key_secret": "abcd..."
}
```

文件权限文档要求**必须 600**（与 D6 同款纪律，加 README 说明行）。

错误处理：

| 形态 | 行为 |
|---|---|
| ENV 仅设 ACCESS_KEY_ID 没 SECRET | report `ConfigInvalid` —— 先 fail 再帮用户少踩坑 |
| 文件不存在且 ENV 也没 | report `ConfigInvalid` 提示两种配置路径 |
| 文件坏 JSON | report `ConfigInvalid` + parse context |

## 4. 请求签名（自实现核心）

OSS Authorization header 格式：

```
Authorization: OSS <access_key_id>:<signature>
```

Signature = `Base64(HMAC-SHA1(secret, StringToSign))`

StringToSign =

```
VER\n
Content-MD5\n
Content-Type\n
Date\n
x-oss-header-1:value\n
x-oss-header-2:value\n
CanonicalizedResource
```

实现要点：

1. **Date** 用 RFC 1123，紧跟最新 wall-clock
2. **Content-Type 不设默认值**（PUT 时不写则视为空字符串）；GET/DELETE 也常空串
3. **CanonicalizedResource**：
   - 资源路径 = `"/${_bucket}/${_key}"`（含前导斜杠）
   - Subresource（如 `?acl, ?uploads`）：小写、字典序、按 `key=value` 拼接
   - URL 不整体 percent-encode（与 D13 Nexus Raw 同理按段）
4. CanonicalHeaders：**仅 `x-oss-` 前缀的小写 header**（按字典序）
5. 默认签名 List-Type 类重标 (`?list-type=2`) 归为 sub-resource

Packaging：
- `sign.rs::sign_request(method, key, content_md5?, content_type, oss_headers, sub_resources) -> Signature`
- **4 例对拍**：用 OSS 官方文档里的 4 个例 子验证签名输出 4 个已知 base64——保证签名算法正确后再细**跨模块**

## 5. ListObjectsV2 三态

Fetch/listing 三态与 raw REST search **形参不同但语义一一对应**：

| 语义 | Nexus Raw REST search | Aliyun OSS ListObjectsV2 |
|---|---|---|
| pagination | `continuationToken` | `NextContinuationToken` |
| item | `items[].path` | `Contents[].Key` |
| page size 上限 | 50（服务端硬上限） | 由 request（默认 1000） |

新增 parser `try_from_xml::<SearchPage>(body_xml: &str)`：手写 ~100 行 XML parser（不需要 heavy xml-rs crate，OSS 列表响应是规范结构）。

构造 URL：
```
GET /?list-type=2&prefix=relay/bifrost-work/commands/&continuation-token=<tok>
```

签名后即发包。

## 6. PUT / DELETE

标准 PUT：`<endpoint>/<key>`；Content-Length = body len。OSS 也是 2xx 全谱判定（PUT 新对象或更新都是 200）。

DELETE：`<endpoint>/<key>`；204 与 404 与 D3 同语义。

写 wrapper 复用 HttpBridge-like `pub fn put_path(subdir, filename, body)` 直接复用骨架（双实现保持对称）。

## 7. fetcher/pusher 复用

`fetch_round` 已经定义在 `src/core/fetcher.rs`，签名 `fetch_round(&HttpBridge, ..)`。

选项：
- A. **bridge 抽象**：重新画 fetch_round `<B: Bridge + ?Sized>(bridge: &B)` + trait default method `delete_task_blob(task)`，HttpBridge 重写之。这样 fetcher 自动跨 backend 工作。
- B. OSS 另写 fetch_round 复制一份。

**建议 A**（无损共享，trait default method 保持向后-compat）：

```rust
// new trait default method
fn delete_task_blob(&self, #[allow] subdir: &str, task_id: &Uuid) -> Result<()> {
    self.remove_task(task_id) // 默认走 trait 原有行为（SSH 用旧 path）
}
```

HttpBridge 重写：`self.delete_path(subdir, &task_filename_from_id(task_id))`。

代价：Bridge trait 加一个 default method——**默认方法 + trait object safety** 不破坏现有实现，仅需各 Bridge impl 现有代码不变。

**问题点**：`task_filename_from_id(task_id)` 没法直接从 uuid 推回原 timestamp——所以需要一个最小的 trait extension `task_blob_name(task: &Task) -> String` 默认 `<id>.json`，HttpBridge 重写加时间戳。

最终哈希：trait 多加两个 default method，不影响已有实现。

## 8. 测试策略

| 类型 | 内容 |
|---|---|
| 签名对拍 | OSS 官方文档 4 例验证 HMAC-SHA1 输出一致（防线状态失配） |
| ListObjectsV2 XML 解析 | 4 个 fixture（含 continuation、空 items、嵌套 sub-resource）验证字段提取 |
| mock server round-trip | 复用现有 std::net tiny server 模式：put/get/list/delete 全谱 |
| ENV > 文件优先级 | 临时设置 ENV，读取；清掉 ENV 后回退文件 |
| 凭据缺失诊断 | 没 ENV 也没文件时报错精确，ENV half-set 报错 |
| OSS 真实 e2e | 由用户提供 access key 触发 1 次跨链 e2e（不 commit secrets） |

## 9. 实施批次（无 review 不写代码）

预测工作量：
- Day 1: schema + 凭据加载（含 6 个单测） + sign skeleton
- Day 2: sign 实现 + OSS 官方文档 4 例 + List-XML parser + 4 fixture
- Day 3: client 骨架（PUT/GET/DELETE/ListObjects）+ mock 全谱
- Day 4: fetcher/pusher trait 抽象（议题7 A 选项）+ OSS fetcher 端到端 mock
- Day 5+: 真实 OSS e2e + 文档 + review

每 batch push 一次的整洁 commit history（同 http-bridge 风格），config 期 & 签名 & parser 是 review 时重点。

## 10. 已知坑与预防

- OSS 与 Nexus Raw **域模式不同**：OSS virtual-hosted = bucket.oss-region.aliyuncs.com；path-style = oss-region.aliyuncs.com/<bucket>/。**默认 virtual-hosted**，文档明示。
- **签名空 Content-Type 陷阱**：OSS `Content-Type` 缺失等价于空串而非整行不存在——`StringToSign` 那一行必须是 `\n\n`。文档要求严格。
- **Sub-resource 的 key=value 严格**：`?acl` 的 value 是 Empty，**看重首字母大小写**（永远是小写 canonical 排序）。
- **文件名与 task_id 的关系**：trait 浅层化了 Uuid → filename 这层抽象我需要新加两条 default method。详见 §7。
- **HTTP 域名内网 vs 公网双栈**：默认公网 endpoint，需要可以配 `http://apmm.oss-cn-beijing-internal.aliyuncs.com` 时通过显式 endpoint 覆盖（OSS 内部区域 endpoint 存在）。

## 11. 答疑需要的输入

这份草案在**没真正读 `chip-fae/skills-you-need/aliyun-oss/SKILL.md`** 的前提下写成——无 honored 签名校验要求时，**签名细节以 Aliyun OpenAPI 文档为准**。请任一：

- A. 授权我 `git clone http://10.20.30.25:8080/chip-fae/skills-you-need.git`（liux PAT + .git-credentials）把 aliyun-oss/SKILL.md 读出来 review 终末坑位
- B. 把 `chip-fae/skills-you-need/aliyun-oss/SKILL.md` 全文 copy-paste 进对话

review 后我再动 Day 1 完整代码。请明示 gate-in。
