# MetaTube Server 影片元数据 API 文档

本文档只覆盖本项目实际需要的三个接口组：

- `GET /v1/movies/search` —— 按关键词/URL 搜索影片
- `GET /v1/movies/{provider}/{id}` —— 按 provider + ID 取影片详情
- `GET /v1/images/{type}/{provider}/{id}` —— 取影片图片（主图/缩略图/背景图）

> **构建信息（随附 exe）**
>
> - 源码：`metatube-community/metatube-sdk-go` `main@59ac7f294e9c`（2026-11-02）
> - 构建：Go 1.27.0，`GOOS=windows GOARCH=amd64 GOAMD64=v1`，`CGO_ENABLED=0`，`-tags experimental`
> - 版本串：`v1.4.0-exp-59ac7f2`
> - 文件：`metatube-server-windows-amd64.exe`（48,203,264 字节）
> - SHA-256：`a037bce9201b5311e6f0ce7e22027b21f86e8490dd58b86e4fbbfa77ea37875c`

---

## 0. 通用约定

### 0.1 服务地址

默认监听 `0.0.0.0:8080`，下文用 `http://127.0.0.1:8080` 表示。

所有业务接口都在 `/v1` 前缀下。

### 0.2 认证

- 启动时 `-token` / 环境变量 `TOKEN` 为空：所有接口公开。
- 非空：`/v1/movies/*` 需要请求头

  ```
  Authorization: Bearer <token>
  ```

  `/v1/images/*` 属于公开组，**不需要** token（图片会被 Jellyfin/浏览器直接引用，所以设计成公开）。

### 0.3 响应包装

JSON 接口统一返回：

```json
{ "data": <业务数据> }
```

出错时：

```json
{ "error": { "code": 404, "message": "info not found" } }
```

`code` 与 HTTP 状态码一致。

### 0.4 通用错误码

| HTTP | message | 触发条件 |
|---|---|---|
| 400 | `invalid keyword` | 关键词为空或规范化后为空 |
| 400 | `invalid id` | 路径里的 ID 无法被该 provider 解析 |
| 400 | `invalid url` | 传入的 URL 无法解析出 ID |
| 401 | `Unauthorized` | 开启了 token 但请求头缺失/错误 |
| 404 | `info not found` | 搜索无结果、详情抓取不到 |
| 404 | `provider not found` | provider 名未注册或已被禁用 |
| 404 | `image not found` | 该条目没有可用图片 |
| 500 | `incomplete metadata` | 抓取到的元数据缺少关键字段 |
| 500 | 其他 | 上游站点抓取失败、解析异常等 |

### 0.5 时间字段

所有日期字段（`release_date`、`birthday`、`debut_date`）都是 Go 的 `datatypes.Date`，序列化为 RFC 3339：

```
"2022-11-25T00:00:00Z"
```

未知日期为零值：

```
"0001-01-01T00:00:00Z"
```

调用方判断"是否有日期"要用零值比较，不要只判 null。

### 0.6 缓存

- `/v1/images/*` 返回 `Cache-Control: public, s-maxage=15552000`（180 天），适合挂 CDN。
- `/v1/movies/*` 没有特殊缓存头，由调用方自己决定。

---

## 1. GET /v1/movies/search

按关键词搜索影片，可指定单个 provider，也可全量并发搜索。

### 1.1 请求参数（query string）

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `q` | string | 是 | — | 关键词。可以是番号（`IPX-001`）、标题片段，或一条完整的 provider 页面 URL |
| `provider` | string | 否 | 空（全部） | provider 名，大小写不敏感，如 `FANZA`、`javbus`。为空时并发搜索所有已启用 provider |
| `fallback` | bool | 否 | `true` | 是否把本地数据库里已缓存的同关键词记录合并进结果 |

### 1.2 行为说明

1. **`q` 是合法 URL 时**：直接抓取该页面解析成一条结果，`provider` 参数被忽略。返回的 `data` 是只含一个元素的数组。
2. **`q` 是关键词时**：
   - 未指定 `provider`：并发请求所有已启用 provider，对结果做合法性校验、按 `关键词与番号的相似度 × provider 优先级` 排序后合并去重。
   - 指定了 `provider`：只搜该 provider，结果按 provider 自己的搜索实现返回（不再二次过滤）。
3. **`fallback=true`**：在线搜索结果之后追加数据库中的匹配记录（同一 `provider + id` 去重）。
4. 全量搜索会并发访问几十个站点，**耗时可能到几秒甚至更久**（默认单请求超时 1 分钟）。如果已经知道来源站，优先指定 `provider`。

### 1.3 返回字段

`data` 为数组，元素结构 `MovieSearchResult`：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | provider 内部 ID（通常等于番号） |
| `number` | string | 规范化后的番号 |
| `title` | string | 标题 |
| `provider` | string | provider 名，可直接用于详情/图片接口 |
| `homepage` | string | 该条目在来源站的页面地址 |
| `thumb_url` | string | 缩略图地址（原始外链） |
| `cover_url` | string | 封面地址（原始外链） |
| `score` | number | 评分，无评分为 0 |
| `actors` | string[] | 出演者；为空时该字段**不出现**（omitempty） |
| `release_date` | string | 发行日期，RFC 3339；未知为 `0001-01-01T00:00:00Z` |

### 1.4 示例

```python
import requests

BASE = "http://127.0.0.1:8080"
HEADERS = {"Authorization": "Bearer <token>"}  # 未设置 token 时省略

def search_movies(q: str, provider: str | None = None, fallback: bool = True):
    params = {"q": q, "fallback": "true" if fallback else "false"}
    if provider:
        params["provider"] = provider
    resp = requests.get(f"{BASE}/v1/movies/search", params=params, headers=HEADERS, timeout=120)
    resp.raise_for_status()
    return resp.json()["data"]

results = search_movies("IPX-001", provider="FANZA")
first = results[0]
print(first["provider"], first["id"], first["title"], first["release_date"])
```

响应示意：

```json
{
  "data": [
    {
      "id": "IPX-001",
      "number": "IPX-001",
      "title": "示例标题",
      "provider": "FANZA",
      "homepage": "https://www.dmm.co.jp/...",
      "thumb_url": "https://pics.dmm.co.jp/...jpg",
      "cover_url": "https://pics.dmm.co.jp/...pl.jpg",
      "score": 4.52,
      "actors": ["演员A"],
      "release_date": "2022-11-25T00:00:00Z"
    }
  ]
}
```

### 1.5 错误

| 场景 | 返回 |
|---|---|
| `q` 缺失 | 400，gin 校验错误信息 |
| `q` 规范化后为空 | 400 `invalid keyword` |
| `provider` 不存在或被禁用 | 404 `provider not found` |
| 无任何结果 | 404 `info not found` |
| `q` 是 URL 但域名不属于任何 provider | 404 `provider not found` |

---

## 2. GET /v1/movies/{provider}/{id}

按 provider + ID 获取完整影片元数据。

### 2.1 请求参数

**路径参数**

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `provider` | string | 是 | provider 名，大小写不敏感（`FANZA`、`fanza` 等价） |
| `id` | string | 是 | provider 内部 ID/番号。会先经过该 provider 的 `NormalizeMovieID` 规范化（例如 FC2 会把 `FC2-PPV-738573` 归一成 `738573`；规则各 provider 不同） |

**查询参数**

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `lazy` | bool | 否 | `true` | `true`：先查本地库，命中直接返回；`false`：跳过缓存，强制回源抓取并写库 |

### 2.2 行为说明

- `lazy=true` 且库里有记录 → 直接返回，不访问源站。
- 否则调用 provider 抓取页面，校验关键字段（`id`/`number`/`title`/`cover_url`/`provider`/`homepage`）齐全后写库并返回；缺字段返回 500 `incomplete metadata`。
- 抓取过程中会用 provider 自带的重试与超时（默认 1 分钟，可启动参数调整）。

### 2.3 返回字段

`data` 为 `MovieInfo`：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | string | provider 内部 ID |
| `number` | string | 规范化番号 |
| `title` | string | 标题 |
| `summary` | string | 简介 |
| `provider` | string | provider 名 |
| `homepage` | string | 来源页面 |
| `director` | string | 导演 |
| `actors` | string[] | 出演者列表 |
| `thumb_url` | string | 缩略图 |
| `big_thumb_url` | string | 大缩略图，可能为空 |
| `cover_url` | string | 封面 |
| `big_cover_url` | string | 大封面，可能为空 |
| `preview_video_url` | string | 预览视频（mp4） |
| `preview_video_hls_url` | string | 预览视频（HLS/m3u8），可能为空 |
| `preview_images` | string[] | 预览图列表 |
| `maker` | string | 厂牌/制作商 |
| `label` | string | 发行商 |
| `series` | string | 系列 |
| `genres` | string[] | 类型标签 |
| `score` | number | 评分 |
| `runtime` | number | 时长（分钟） |
| `release_date` | string | 发行日期，RFC 3339 |

### 2.4 示例

```python
def movie_detail(provider: str, movie_id: str, lazy: bool = True):
    resp = requests.get(
        f"{BASE}/v1/movies/{provider}/{movie_id}",
        params={"lazy": "true" if lazy else "false"},
        headers=HEADERS,
        timeout=120,
    )
    resp.raise_for_status()
    return resp.json()["data"]

info = movie_detail("FANZA", "IPX-001")
print(info["title"], info["maker"], info["genres"], info["runtime"])
```

响应示意：

```json
{
  "data": {
    "id": "IPX-001",
    "number": "IPX-001",
    "title": "示例标题",
    "summary": "示例简介",
    "provider": "FANZA",
    "homepage": "https://www.dmm.co.jp/...",
    "director": "导演名",
    "actors": ["演员A"],
    "thumb_url": "https://pics.dmm.co.jp/...pt.jpg",
    "big_thumb_url": "https://pics.dmm.co.jp/...pt.jpg",
    "cover_url": "https://pics.dmm.co.jp/...ps.jpg",
    "big_cover_url": "https://pics.dmm.co.jp/...pl.jpg",
    "preview_video_url": "https://.../sample.mp4",
    "preview_video_hls_url": "",
    "preview_images": ["https://.../1.jpg"],
    "maker": "厂商",
    "label": "厂牌",
    "series": "系列",
    "genres": ["类型A", "类型B"],
    "score": 4.52,
    "runtime": 120,
    "release_date": "2022-11-25T00:00:00Z"
  }
}
```

### 2.5 错误

| 场景 | 返回 |
|---|---|
| provider 未注册/被禁用 | 404 `provider not found` |
| ID 无法规范化 | 400 `invalid id` |
| 源站无此条目 | 404 `info not found` |
| 抓取到但关键字段缺失 | 500 `incomplete metadata` |
| 源站超时/反爬拦截 | 500（message 为上游错误描述） |

---

## 3. GET /v1/images/{type}/{provider}/{id}

获取影片图片，返回 JPEG 二进制（不包 `data`）。

### 3.1 路径参数

| 参数 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `type` | enum | 是 | `primary`（主图 2:3）/ `thumb`（缩略图 16:9）/ `backdrop`（背景图不裁剪） |
| `provider` | string | 是 | provider 名，大小写不敏感 |
| `id` | string | 是 | 影片 ID，规则同详情接口 |

### 3.2 查询参数

| 参数 | 类型 | 必填 | 默认 | 说明 |
|---|---|---|---|---|
| `url` | string | 否 | 空 | 直接指定图片地址，跳过元数据查询。必须是该 provider 可抓取的地址 |
| `ratio` | float | 否 | `-1` | 裁剪宽高比（宽/高）。仅 `primary` 允许覆盖；`thumb`/`backdrop` 传了也会被固定值覆盖。有效范围 0.01–100，超出**不裁剪** |
| `pos` | float | 否 | `-1` | 裁剪框中心位置，0–1 的比例。<br>· 不带 `url` 时：`primary` 默认 1.0（靠右）、`thumb` 0.5（居中）、`backdrop` 0（靠左）<br>· 带 `url` 时：默认 -1，等价于 0（靠左/靠上），建议显式传值 |
| `auto` | bool | 否 | `false` | 启用基于人脸检测的自动裁剪。仅带 `url` 时由该参数控制；不带 `url` 的 `primary` 在 `pos<0` 时会按番号自动决定是否启用 |
| `badge` | string | 否 | 空 | 角标图片的 **URL**（不是文字）。程序会下载该图、缩放到主图高度的 1/5，贴在**左上角** |
| `quality` | int | 否 | `90` | JPEG 编码质量，1–100 |

### 3.3 图片来源选择（不带 `url` 时）

| type | 取图顺序 | 默认裁剪比例 |
|---|---|---|
| `primary` | `big_thumb_url` → `cover_url` | 2/3（≈0.667） |
| `thumb` | `big_cover_url` → `cover_url` | 16/9（≈1.778） |
| `backdrop` | `big_cover_url` → `cover_url` | 不裁剪 |

### 3.4 响应

- `Content-Type: image/jpeg`
- `X-MetaTube-Image-Width`：输出宽
- `X-MetaTube-Image-Height`：输出高
- `Cache-Control: public, s-maxage=15552000`

### 3.5 示例

```python
def download_poster(provider: str, movie_id: str, out_path: str, kind: str = "primary"):
    resp = requests.get(
        f"{BASE}/v1/images/{kind}/{provider}/{movie_id}",
        params={"ratio": "0.6667", "pos": "0.5", "quality": "92"},
        timeout=120,   # 图片接口公开，不需要 Authorization
    )
    resp.raise_for_status()
    assert resp.headers["Content-Type"] == "image/jpeg"
    width = resp.headers.get("X-MetaTube-Image-Width")
    height = resp.headers.get("X-MetaTube-Image-Height")
    with open(out_path, "wb") as f:
        f.write(resp.content)
    return width, height

w, h = download_poster("FANZA", "IPX-001", "cover.jpg")
```

直接用 URL 指定图片时：

```python
resp = requests.get(
    f"{BASE}/v1/images/primary/FANZA/IPX-001",
    params={"url": "https://pics.dmm.co.jp/...pl.jpg", "badge": "https://example.com/sub.png"},
)
```

### 3.6 错误

| 场景 | 返回 |
|---|---|
| provider 未注册 | 404 `provider not found` |
| 条目无图片 | 404 `image not found` |
| 演员 provider 请求 `thumb`/`backdrop` | 400 `unsupported image type` |
| `type` 不是三个合法值之一 | 404 `Not Found`（无匹配路由） |
| `url` 下载失败或不是图片 | 500（上游错误描述） |
| ID 无效 | 400 `invalid id` |

---

## 4. Provider 清单

`/v1/providers` 可以实时查当前实际可用的列表。本构建（含 experimental）注册了 **35 个影片 provider**：

```
10musume, 1Pondo, AVBASE*, AVE, C0930, Caribbeancom, CaribbeancomPR,
DAHLIA, DUGA, FALENO, FANZA, FC2, FC2DB, fc2hub, FC2PPVDB, Gcolle,
Getchu, H0930, H4610, HeyDouga, HEYZO, JAV321, JavBus, JAVFREE, KIN8,
MadouQu*, MGS, MURAMURA, MYWIFE, PACOPACOMAMA, Pcolle, SOD,
ThePornDBMovie*, ThePornDBScene*, TOKYO-HOT
```

带 `*` 的只有 `experimental` 构建才有。其中：

| provider | 说明 | 启用条件 |
|---|---|---|
| `AVBASE` | 聚合检索站点 | 带 `experimental` 编译即启用 |
| `MadouQu` | 中文源 | 默认 `priority=0` 被跳过，需设 `MT_MOVIE_PROVIDER_MADOUQU__PRIORITY=1000` |
| `ThePornDBMovie` / `ThePornDBScene` | 英文源 | 需要 API Token：`MT_MOVIE_PROVIDER_THEPORNDBMOVIE__ACCESS_TOKEN=<token>`、`MT_MOVIE_PROVIDER_THEPORNDBSCENE__ACCESS_TOKEN=<token>`；不配 token 时调用会失败 |

**演员 provider**：`Gfriends`（默认）、`AV-LEAGUE*`、`ThePornDBActor*`（后两者需要 `experimental`，ThePornDBActor 还需要 `MT_ACTOR_PROVIDER_THEPORNDBACTOR__ACCESS_TOKEN`）。

注意：`provider` 名是大小写不敏感的，但返回体里的 `provider` 字段是它的规范写法，回传时直接用返回值即可。

---

## 5. 典型调用流程

```python
# 1) 搜索候选
candidates = search_movies("IPX-001")          # 全量并发，慢
candidates = search_movies("IPX-001", "FANZA") # 已知来源，快

# 2) 取详情（lazy=False 可强制刷新，缓存脏了用）
info = movie_detail(candidates[0]["provider"], candidates[0]["id"], lazy=True)

# 3) 取图片（公开接口，可直接写进 <img src>）
poster_url = f"{BASE}/v1/images/primary/{info['provider']}/{info['id']}"
```

给 Jellyfin 类客户端用时的建议：

1. 搜索阶段缓存 `(provider, id)`，不要每次全量搜索；
2. 详情接口 `lazy=true` 走本地库，首次抓取后很快；
3. 图片 URL 直接交给播放端/CDN，不要由你的服务中转二进制；
4. 日期字段注意 `0001-01-01T00:00:00Z` 零值。

---

## 6. 启动参数速查

```
metatube-server-windows-amd64.exe
  -port 8080                    监听端口
  -bind ""                      绑定地址（默认全部接口）
  -token ""                     API token，空=不鉴权
  -dsn metatube.db              数据库；空=内存 SQLite（重启丢数据）
  -db-auto-migrate              SQLite 下会自动开启，PostgreSQL 需显式加
  -db-prepared-stmt             PostgreSQL 建议开启
  -request-timeout 1m           单请求超时
  -db-max-idle-conns 0
  -db-max-open-conns 0
  -version                      打印版本后退出
```

启动参数对应的环境变量**没有前缀**（由 `ff.WithEnvVars` 直接按 flag 名匹配）：`PORT`、`BIND`、`TOKEN`、`DSN`、`REQUEST_TIMEOUT`、`DB_AUTO_MIGRATE`、`DB_PREPARED_STMT` 等。

provider 级配置才用 `MT_` 前缀，格式为 `MT_<ACTOR|MOVIE>_PROVIDER_<PROVIDERNAME>__<KEY>`，支持的 key 有 `PRIORITY`、`PROXY`、`TIMEOUT`，以及各 provider 自定义项（如 `ACCESS_TOKEN`）。

最小可用启动（Windows 命令提示符，注意 `data` 目录要先存在，SQLite 不会自动建目录）：

```bat
metatube-server-windows-amd64.exe -port 8080 -token mytoken -dsn data\metatube.db
```
