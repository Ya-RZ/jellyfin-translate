# jellyfin-translate

把 Jellyfin 媒体库的核心文字翻译成目标语言，并写回服务器的 Windows 桌面工具。

> 状态：M0 开发中（core + gpui-kit 桌面壳）。无 CLI，全部操作在 GUI 内完成。

## 范围

翻译字段（其余不动）：

- `Name`（标题）、`OriginalTitle`（原名）、`Overview`（简介）、`Tags`（标签）
- `People[].Name`（演员姓名）、`People[].Role`（角色名）

不翻译：`Genres`、`Studios`、`Tagline`、分级、评分/时长/日期、图片里的文字。

多语言策略：服务器只保留当前语言；本地 SQLite 保存原文与各语言译文，切换语言 = 重新写回并加锁。

## 架构

```text
apps/desktop      gpui-kit 桌面应用（唯一二进制）
crates/core       服务编排：同步、翻译、写回、锁定
crates/jellyfin   Jellyfin 12.x REST 客户端
crates/store      SQLite 本地镜像、译文、演员预设
crates/translate  翻译 provider trait + OpenAI 兼容实现（后续可扩展）
```

## 界面

- 媒体库：多库 tab 切换、海报网格、多选、状态徽标（未翻译/草稿/已写回/已锁定）
- 详情：右侧抽屉；编辑弹窗；单条翻译/写回/锁定
- 演员预设：演员姓名完全匹配时使用预设译名
- 设置：OpenAI 兼容接口、按字段的提示词模板、语言、并发
- 数据目录：程序目录下的 `data\`（便携，可整目录拷走）

## 构建

需要 Rust stable（1.99+）。Windows 构建用 MSVC toolchain：

```powershell
cargo build --release -p desktop
```

Linux/macOS 仅用于开发期编译检查（gpui 需要 wayland/xkbcommon 等系统库）。若环境缺 `fontconfig.pc`，可用 `RUST_FONTCONFIG_DLOPEN=1 cargo check -p desktop` 临时绕过（仅影响 Linux 编译检查，Windows 构建不需要）。

## 认证

Jellyfin 12.0 起只接受：

```http
Authorization: MediaBrowser Client="jellyfin-translate", Device="desktop", DeviceId="jellyfin-translate", Version="0.1.0", Token="<api key>"
```

API Key 在 Jellyfin 控制台 → 管理 → API 密钥创建，写元数据与图片需要管理员权限。

## 写回规则

`POST /Items/{itemId}` 是覆盖语义：`Name`/`Overview` 等标量字段不传会被写成 null。所有写回都遵循：

1. `GET /Items/{id}` 取完整 DTO；
2. 只修改目标字段，其余原样保留；
3. 整份 `POST` 回去；
4. 写入的字段加入 `LockedFields`，防止重新刮削覆盖。

应用内还有视频级锁定：锁定后不再参与翻译/写回，解锁后才能重译。

## 发布

推送到 `main` 后，GitHub Actions 在 Windows 上构建，并把 `JellyfinTranslate-windows-x64.zip` 上传到固定 tag `v0.1.0-pre` 的 **pre-release**（不使用 Actions artifact）。

## License

MIT
