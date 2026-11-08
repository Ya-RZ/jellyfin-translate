# jellyfin-translate 全项目代码评审报告（全量重评）

- **评审日期**：2026-11-09
- **评审方式**：6 个并行子智能体按 crate 边界分工精读 + Lead 逐条交叉验证（含对照第三方依赖源码与测试依赖）
- **评审范围**：仓库受 git 跟踪的全部 Rust 源码（23 个 `.rs`，约 1.6 万行）+ `Cargo.toml` + `.github/workflows` + `README.md`
- **覆盖情况**：`total_files = 23`，`reviewed = 23`，`skipped = 0`，`coverage_rate = 100%`

## 结论概览

代码整体质量高：分层清晰（`jellyfin` HTTP 客户端 / `store` SQLite / `core` 编排 / `translate` 翻译适配 / `desktop` gpui-kit UI），错误处理普遍 `anyhow::Result` 带中文上下文，异步代码遵守「不跨 `await` 持锁」，写回严格遵循「取完整 DTO → 只改目标字段 → 整份 POST → 读回校验 + LockedFields」。测试覆盖到位（core 51 / store 11 / translate 13 / jellyfin 20）。

**未发现 Critical**。发现 **1 个 High**（旧报告 L3 的"防 panic 修复"实为无效的死代码）、**12 个 Medium**、**42 个 Low**。本次已修复 High + 6 个 Medium，其余在下方标注原因。

---

## Critical

（无）

---

## High

### H1. 封面 decode 的 catch_unwind 是死代码，旧「防 panic 修复」无效（bug）

- **文件**：`apps/desktop/src/covers.rs:208-236`
- **问题**：`decode()` 用 `catch_unwind` 包裹 `Image::from_bytes(format, bytes)`，注释称「解码器对损坏载荷会 panic」。经核对依赖源码（gpui-pre 0.3.11 `platform.rs:3403`），`Image::from_bytes` 只是纯构造 `Self { id: hash(&bytes), format, bytes }`，既不解码也不 panic；真正的解码发生在渲染期，返回 `Result` 且被 `.ok()` 吞掉。因此：① catch_unwind 是死代码、不起防护；② 损坏图片（sniff 命中魔数）照样构造成功标记 Ready，渲染时解码失败 → 图片区域空白，且 `begin()` 对已存在的 Ready 键返回 false，损坏图既不重试、连占位符都不显示。
- **修复**：移除死代码 catch_unwind，修正注释如实说明「from_bytes 只构造、渲染期才解码」。
- **遗留**：损坏图「渲染空白不重试」根因在 gpui 渲染管线，彻底修复需引入 image crate 做 `load_from_memory` 预解码（收益有限、暂不做）。

---

## Medium

### 数据完整性

#### M1. 无 id 演员按位置回退错赋 id，写回可能张冠李戴或丢条目（bug）

- **文件**：`crates/core/src/lib.rs:1406-1432`（merge_people）、`2086-2133`（merge_people_json）
- **问题**：`merge_people` 对「无 id 的手输演员」用 `.or_else(|| existing.get(index))` 位置回退，并把该位置既有条目的 id 回填（`id.or_else(|| base.id.clone())`）。当无 id 新人在列表中间插入时，会被错赋成该下标既有演员的 id；`merge_people_json` 同样只有 id 匹配 + 位置回退，写回时用错 id 把新人名字写到既有演员头上、或与既有 id 冲突导致新人被静默丢弃。
- **修复**：去掉位置回退与 id 回填，无 id 新人保持 `id=None`（作为新增条目），只保留 id 匹配 + 名字匹配。

#### M2. 写回「部分成功」后本地状态未标记，重同步会覆盖原文快照（bug，未修）

- **文件**：`crates/core/src/lib.rs:1964-2077`、`2153-2267`、`1012-1028`
- **问题**：POST 成功后还要 `verify_written` 的 GET 通过才 `mark_written`；若 `verify_written` 失败（服务端静默丢弃某字段，如测试 `write_back_detects_a_server_that_ignored_a_field`），已写入的其它字段不落 `written`，status 保持 Draft；Draft 状态下 store 的 `upsert_items` 用服务端已翻译的值覆盖本地原文快照，导致已写入字段的原文在本地+服务端同时丢失，切换语言重译会拿译文当原文。
- **建议（未直接修，改动核心状态机、风险高）**：把「已确认写入的字段」单独按字段粒度记录，或在 `verify_written` 失败回执里明确提示「部分字段已写入，重同步可能覆盖原文」。

#### M3. JSON 列损坏时返回空值，可能把空列表写回抹掉服务器数据（bug，未修）

- **文件**：`crates/store/src/lib.rs:824-837`
- **问题**：`parse_json_column` 解析失败只 `warn` 后返回 `T::default()`（空 Vec），tags/people/genres 损坏时被读成空列表，上层翻译/写回可能把空列表写回 Jellyfin 抹掉服务器数据（注释自承「曾发生过 all tags/people disappeared」）。日志能暴露但不能阻止传播。
- **建议（未直接修，需设计哨兵值）**：损坏列让 `row_to_item` 返回 Err 阻断读，或保留「损坏」标记字段并在写回前拦截。

### 可靠性

#### M4. presets 迁移非原子，崩溃会砖库且无法自愈（bug）

- **文件**：`crates/store/src/lib.rs:311-336`
- **问题**：`migrate_presets` 用裸 `execute_batch` 依次 `DROP INDEX → CREATE → INSERT → DROP TABLE presets → RENAME`，未包事务，每条语句独立提交。若在 `DROP TABLE` 与 `RENAME` 之间崩溃，`presets` 表已消失而 `presets_new` 未改名，下次 `open()` 走迁移查不到 presets 报「no such table」，启动永久失败。
- **修复**：整个 rebuild 序列包进单个事务（SQLite DDL 事务性保证要么全成、要么全回滚）。

#### M5. 唯一索引可能在旧库上建失败，导致无法打开（bug，未修）

- **文件**：`crates/store/src/lib.rs:286-289`
- **问题**：`idx_presets_normalized ON presets(kind, lower(trim(source)))` 建在归一化后可能重复的行上（旧库允许 "Tom Hanks" 与 "tom hanks" 并存），`CREATE UNIQUE INDEX` 失败会冒泡到 `Store::open` 返回 Err，应用无法启动且无降级。
- **建议（未直接修，需去重策略）**：建索引前检测并合并归一化冲突行，或建索引失败时告警跳过而非整体失败。

#### M6. 翻译重试对 4xx 也重试且无退避（reliability）

- **文件**：`crates/translate/src/openai.rs:262-289`、`translate_once`
- **问题**：`while error_attempts < MAX_ATTEMPTS` 对 401/400/404/422 这类永久性错误也无差别重试（白打请求、延迟暴露配置错误），对 429 既不读 Retry-After 也不退避。
- **修复**：引入 `HttpError`（携带状态码）+ `is_retryable`，4xx（除 429）直接失败，网络错误/429/5xx/解析失败才重试一次。

#### M7. 封面把网络失败与「无图」统一负缓存，瞬时抖动永久空白（bug，未修）

- **文件**：`apps/desktop/src/covers.rs:186-192`、`68-83`
- **问题**：`service.cover()` 出错（网络抖动/离线）时 `load_cover` 返回 None，`request_cover` 记为 Failed，此后 `begin()` 对该 key 恒返回 false，UI 永远显示占位符；`covers.clear()` 只在连接变化/同步完成时调用，瞬时抖动无法自愈。
- **建议（未直接修，需区分错误类型）**：网络错误不进 Failed 负缓存或加 TTL/重试。

#### M8. 批量翻译/写回/锁定无 in-flight 守卫，连点绕过限流（bug，未修）

- **文件**：`apps/desktop/src/views/library.rs:364-494`
- **问题**：`translate_items`/`write_back_items`/`set_items_locked` 无 busy 状态，工具栏按钮仅由 `!has_selection` 禁用；连点会并发启动多份任务，实际并发 = N×concurrency，重复消耗 API 配额并绕过限流；写回选中对同条目并发 POST 冲突 DTO。
- **建议（未直接修）**：加 `batch_busy` 状态，进行中禁用按钮、回调复位。

### UI 一致性

#### M9. 并发设置非法输入静默回退，UI 与实际配置不一致（bug）

- **文件**：`apps/desktop/src/views/settings.rs:299-307`
- **问题**：`.parse::<usize>().unwrap_or(4).max(1)` 对 "abc"/负数静默回退 4，无校验提示、无上限，保存后输入框仍显示原值，形成 UI 与实际不符。
- **修复**：保存前校验（非 1–16 整数报错并 return），合法值 `clamp(1,16)`。

#### M10. 演职员翻译预览按下标写回，忽略模型返回的 id（bug）

- **文件**：`apps/desktop/src/views/edit_dialog.rs:1267-1281`
- **问题**：`apply_preview` 的 People 分支用 `people.get(index)` 按下标对齐 rows，忽略 `PersonView.id`；模型若调整顺序/遗漏/增删，译文姓名会错配到别的 id，随后用 `row.id`（不变）+ 错配 name 写回，给错误演员写错误姓名。
- **修复**：按 id 匹配优先，无 id 行才退化位置对齐。

### 维护性

#### M11. status 用自由 String 而非枚举，可静默破坏核心语义（maintainability，未修）

- **文件**：`crates/store/src/lib.rs:63`、`178`、`624-631`
- **问题**：`ItemRecord.status`/`TranslationRecord.status` 是 String，`set_status` 不校验取值；`upsert_items` 依赖精确字符串 `'written'`，误传 "Written"/拼错会让「写回后保留源快照」保护静默失效。
- **建议（未直接修，属类型设计重构）**：定义 `enum Status`（serde snake_case + ToSql/FromSql），从类型上杜绝非法值。

#### M12. spawn_job 四变体重复 + 后台任务 panic 无日志（maintainability，未修）

- **文件**：`apps/desktop/src/app.rs:216-352`
- **问题**：`spawn_job`/`spawn_job_in`/`spawn_async_job`/`spawn_async_job_in` 四函数约 120 行几乎一致，worker 都是 `runtime.spawn(async move { let _ = tx.send(task(service)); })`；task panic 时 `tx.send` 不执行、channel 关闭，用户只见「后台任务意外结束」，而 `windows_subsystem="windows"` 无控制台，panic 信息与回溯完全丢失。
- **建议（未直接修）**：抽私有泛型内部函数承载公共逻辑；worker 内 `catch_unwind` 记录 payload + backtrace 再送回错误。

---

## Low（42 项，按 crate 分组，均未修）

### store

- `ensure_column` 的 `decl` 裸拼 SQL（security）：当前参数是编译期常量无注入面，但双引号挡不住含 `"`/`;` 输入、`decl` 完全裸拼。建议表/列名白名单或拒绝特殊字符。
- `upsert_items` 忽略入参的 status/locked/draft（maintainability）：有意设计（镜像刷新不覆盖本地状态），但签名收完整 `ItemRecord` 易误用。建议注释说明或拆 `SourceSnapshot` 入参。
- `now_unix` 用 `unwrap_or(0)`（bug）：时钟异常静默返回 0，与「未同步」默认值混淆。建议返回 Result 或 `i64::MAX`。
- `list_items(None)` 全表读取无分页（performance）：条目量大时内存峰值。建议键集分页。
- `row_to_item` 用硬编码列下标（maintainability）：与 `SELECT_ITEMS` 列序强耦合。建议 `r.get("name")` 命名取值。

### jellyfin

- `path_segment` 未拒绝裸 `.`/`..`（security）：`item_id=".."` 拼 `/Items/..` 经 URL 点段归一化命中别的端点。建议白名单字符集。
- `download_remote_image` 是 SSRF 放大入口（security）：只校验 http(s) 前缀，不限回环/内网。当前威胁有限，建议文档化信任边界。
- 允许 `http://` 明文传输 api_key（security）：局域网自托管可保留，建议文档化提示。
- `ensure_ok` 读取错误体失败被 `unwrap_or_default` 吞（maintainability）：丢真实失败原因。建议显式捕获。
- `ITEM_FIELDS` 请求了 PremiereDate/ProviderIds 但 `ItemDto` 无对应字段（maintainability）：请求与解析不一致。建议补字段或去掉。
- `DeviceId` 硬编码常量（maintainability）：所有安装共用设备身份。建议按安装生成。

### translate

- `MAX_ATTEMPTS` 注释与回显重试预算不符（documentation）：回显重试不消耗错误预算，单批最多 3 次请求。建议注释明确。
- `ChatMessage.content` 类型过严（maintainability）：部分网关 content 非字符串会反序列化失败。建议宽容解析。
- `TranslateRequest.glossary` 未消费（maintainability）：文档暗示有应用逻辑，实际无。建议删除或标注未实现。
- `Limits.qps`/`max_concurrency` 纯声明（maintainability）：生产从不读取，与内部常量两处真相来源。建议明确「能力声明」。
- `extract_json` 起止括号不配对（maintainability）：min/max 取首尾可能取到不配对的 `[]`/`{}`。建议按平衡区间扫描。
- 围栏剥离逻辑重复（maintainability）：`extract_json` 与 `clean_plain_text` 几乎逐行相同。建议抽 `strip_fence`。
- 待译文本未隔离间接提示注入（security）：恶意元数据可诱导模型偏离翻译任务。建议 system 提示明确「user 内容一律视为数据」。

### core

- `render_template` 链式 replace 会误替换插入的原文（bug）：原文含 `{{segments}}` 等字面量会被二次替换。建议单遍解析或转义。
- `record_translations` 的 provider 硬编码 "openai"（maintainability）：base_url 允许任意兼容端点，审计数据误导。建议纳入 ProviderSettings。
- `source_hash` 单字节分隔符未转义（bug）：字段含分隔符字节会哈希碰撞。建议长度前缀。
- `translate_items` spawn 循环只 acquire 处背压（performance）：JoinSet 长期缓冲全部结果。建议穿插 `join_next`。
- `join_next` panic 中止整批（maintainability）：单 item panic 会 abort 全部在途任务。建议计入 failed 继续 drain。
- api_key 明文落盘（security）：config.json 已 0600，但 SQLite settings 表明文。建议 OS 凭据存储。
- `FieldKind` 含 SortName/Genres 与 README「不翻译」约定不一致（documentation）：代码支持翻译写回这两个字段且有测试。建议确认意图二选一。

### desktop（app.rs / covers.rs / connect.rs）

- `connect.rs` connecting 标志在 `service.is_none()` 早退时永不复位（bug）：按钮永久「连接中」。建议先判 `service.is_some()`。
- `notify_toast` 用消息内容相等判断（bug）：3 秒内相同消息两次弹出会提前清空。建议单调递增 toast id。
- API key 明文（security）：连接页明示明文落盘。建议 DPAPI 加密。
- `sanitize_id` 碰撞（bug）：不同 id 映射同一缓存文件名串图。建议追加短哈希。
- `render_drawer` 每次渲染多字段 clone（performance）：GPUI 全量重建，条目多时是热点。建议 `Arc<str>`。
- 同步进度 `unbounded_channel` 无背压（performance）：每 Progress 全量重绘。建议有界 + 节流。

### desktop-views

- `load_items` 无重入守卫（bug）：与 `load_libraries` 不对称，快速触发会并发请求竞争 `items.insert`。建议加 `loading_items` 守卫。
- `write_field_editor` 先全量 `save_draft` 再写回当前字段（bug）：副作用超出按钮文案。建议注释或按字段保存。
- 写回失败文案把多成因归为「可能已锁定」（maintainability）：建议区分失败原因。
- API key 明文 + trim 截断（security）：同 core。
- `translate_error` 用英文子串匹配（maintainability）：依赖 openai 措辞、易失效。建议错误枚举。
- `EditSource::text` 与 `effective_text` 重复（maintainability）：七分支逻辑一致。建议抽公共函数。
- `build_edit` 空标题改写「未命名」与写回拒绝不一致（bug）：建议统一策略。
- `close_field_editor` 对 Tags/Genres/People 静默丢弃（maintainability）：建议未保存时确认。
- presets 多余 clone + `{:?}` 拼 id（performance）：建议借用或稳定索引。
- presets `to_lowercase` 每次渲染 + 三遍 filter count（performance）：建议免分配 + 一次遍历。
- `item_by_id` 全库 `flat_map` 线性查找 O(n)（performance）：批量操作 O(选中×n)。建议建索引。
- metatube 死代码 `split('T').next().unwrap_or`（maintainability）：next 恒 Some。建议简化。

---

## 正向观察（值得保持）

- **分层清晰**：`jellyfin` / `store` / `core` / `translate` / `desktop` 职责边界清楚。
- **错误处理**：生产路径几乎零 unwrap/expect/panic/unsafe，错误统一 `anyhow::Result` 带中文上下文，`ApiError` 保留 status + 截断响应片段。
- **异步卫生**：所有 `Arc<Mutex<Inner>>` 取完值立刻释放再 await，无跨 await 持锁；`translate_items` 用 `Arc<Semaphore>` + `OwnedSemaphorePermit` 限流、`JoinSet` 收敛，`concurrency.clamp(1,16)`。
- **写回语义严谨**：严格「GET 完整 DTO → 只改目标字段 → 整份 POST → 读回校验 + LockedFields」，非目标字段靠整份 DTO 往返保留，OriginalTitle/SortName 锁 400 回退 + 会话级记忆。
- **防刮削/防误写**：`field_has_content` 区分「显式空草稿」与「未知空源字段」；`upsert_items` 在 written 状态保留原文快照、不覆盖 draft。
- **输入校验**：认证头防注入（拒绝控制字符/引号）、`path_segment` 防路径穿越（含 `%` 编码）、SQL 全面参数化。
- **快照与审计**：`source_hash`（blake3）保证二次翻译从原语言出发；`record_translations` 逐字段审计。
- **测试充分**：core 51（并发峰值、锁回退、字段级翻译/写回、preset 作用域）、store 11（迁移幂等、旧库升级、written 快照）、translate 13（重试/回显/多 JSON 形状）、jellyfin 20（wire format、认证头、base64、路径穿越）。
- **并发模型干净**：UI 线程独占状态、后台 tokio 任务只持 `Arc<AppService>` 经 channel 回传，无跨线程裸共享状态。

---

## 修复优先级建议

| 优先级 | 项 | 状态 |
|---|---|---|
| 1 | M1 无 id 演员错赋 id | 已修 |
| 2 | M4 presets 迁移非原子 | 已修 |
| 3 | M6 重试 4xx 不重试 | 已修 |
| 4 | M10 apply_preview 按 id 匹配 | 已修 |
| 5 | M9 并发设置校验 | 已修 |
| 6 | H1 covers 死代码 | 已修 |
| 7 | M2 写回部分写入标记 | 未修（需按字段粒度状态设计） |
| 8 | M3 JSON 损坏哨兵值 | 未修（需设计） |
| 9 | M5 唯一索引去重 | 未修（需去重策略） |
| 10 | M7/M8 封面负缓存 / 批量防重入 | 未修（需错误分类 / 状态） |
| 11 | M11/M12 枚举化 / spawn 去重 | 未修（重构） |

---

## 本次修复记录（2026-11-09）

| 编号 | 状态 | 修复方式 |
|---|---|---|
| H1 | 已修复 | 移除 `covers.rs` `decode()` 的死代码 catch_unwind，修正注释 |
| M1 | 已修复 | `merge_people` 去掉位置回退与 id 回填，`merge_people_json` 去掉位置回退，无 id 新人保持 `id=None` |
| M4 | 已修复 | `migrate_presets` 整个 rebuild 包进 `transaction`（`migrate`/`migrate_presets` 改 `&mut self`） |
| M6 | 已修复 | 新增 `HttpError` + `is_retryable`，4xx（除 429）直接失败 |
| M9 | 已修复 | `save_settings` 校验并发数为 1–16 整数，非法报错 return，合法 `clamp` |
| M10 | 已修复 | `apply_preview` People 按 id 匹配优先，无 id 才位置回退 |
| M2/M3/M5/M7/M8/M11/M12 | 未修 | 见「修复优先级建议」，需设计决策或重构，风险高于收益 |

**验证**：`cargo check -p core -p store -p translate -p jellyfin` 与 `RUST_FONTCONFIG_DLOPEN=1 cargo check -p desktop` 均通过；`cargo test -p core -p store -p translate -p jellyfin` 全通过（core 51 / store 11 / translate 13 / jellyfin 20）。
