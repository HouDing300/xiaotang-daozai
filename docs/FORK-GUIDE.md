# 岛仔自定义分支移植手册

> **用途**：把本分支的定制功能移植到更新版上游、交给他人接手维护，或准备向上游提交 PR 时，按本手册执行即可，不必重新踩一遍坑。
>
> **基线**：上游 `Renly1994/xiaotang-daozai` `v0.6.61` 起步，过程中合并了 `v0.6.62`。
> **分支**：`feat/focus-timers-data-dir`（下文称「本分支」）。
> **技术栈**：Rust + `egui/eframe`，Windows 桌面，`x86_64-pc-windows-msvc` 工具链。
>
> 本文件只描述工程做法，不含任何账号、令牌、邮箱或本机路径；命令中的路径均为相对仓库根目录或占位符。

---

## 1. 相对原项目新增了什么

### 1.1 功能总览

| 功能 | 用户入口 | 主要文件 | 持久化 |
| --- | --- | --- | --- |
| 「专注与饮水」→「专注与定时」，5 个并列子项（专注 / 饮水 / 站立 / 抽烟 / 自定义） | 设置 → 专注与定时 | `src/extras.rs`（`SETTINGS_GROUPS`、`chip_label`） | 沿用设置文件 |
| 站立提醒（久坐） | 设置子项、岛上独立页 | `src/timers.rs`、`src/pages.rs::page_stand`、`src/paint.rs::stand_person` | `timers.json` |
| 抽烟提醒 + 每日根数统计 / 7 天柱状 / 上限 | 设置子项、岛上独立页 | `src/smoke.rs`、`src/pages.rs::page_smoke`、`src/paint.rs::cigarette` | `smoke.json` |
| 自定义定时提醒：标题、正文、间隔或每日时刻、生效时段、独立开关 | 设置子项、岛上定时列表页 | `src/timers.rs`、`src/pages.rs::page_timers` | `timers.json` |
| 岛上新增 3 个页面，页面管理里各有独立上岛开关 | 设置 → 页面管理 | `src/app.rs`（`PAGE_NAMES`）、`src/model.rs`（`visible_pages` / `page_order`） | 设置文件 |
| 通知卡片快捷按钮：知道了 / 抽一支 / 稍后 / 定时设置 | 提醒弹出时 | `src/extras.rs::draw_notice`、`timer_button_row` | — |
| 数据目录可更改，默认程序目录下 `data` | 设置 → 常规 → 数据文件位置 | `src/model.rs`（`config_dir`、`set_data_dir`、`DATA_ENTRIES`） | `%APPDATA%\Daozai\data-dir.txt` 引导文件 |
| 邮件正文按 HTML 原文结构化排版 | 邮件详情、阅读全文窗口 | `src/mail_format.rs`、`src/mail_reader.rs`、`src/mail.rs`、`src/mail_fetch.rs` | HTML 原文仅内存 |
| 图标动画 60 帧调度、邮件排版缓存、统计单次扫描 | 全局 | `src/app.rs::frame`、`mail_body_lines`、`src/smoke.rs::recent_days` | — |
| WorkBuddy 改用官方授权（同步 v0.6.62） | AI 额度 → 添加账号 | 上游 `src/workbuddy_login.rs`、`src/workbuddy_quota.rs` | 加密保存的 access/refresh token |

### 1.2 与上游保持一致、明确不做的事

- **不再自研 WorkBuddy 登录态抓取**。曾经实现过「独立 Edge 配置 + DPAPI 解密 Cookie」（`src/browser_login.rs`，约 512 行），以及浏览器扩展桥（Kimi WebBridge，本机 `127.0.0.1:10086`）探测与手动粘贴 Cookie 的 UI。上游 v0.6.62 改为「官网一次性 `state` + 默认浏览器授权 + 令牌自动续期」后全部删除，避免两套逻辑并存。
- **不引入 HTML 渲染引擎**。邮件只还原影响阅读的结构层次（标题、段落、列表、引用、表格单元格、加粗、分隔线），不还原 CSS、颜色、背景图。
- **不改动上游的授权、隐私与安全模型**。新增数据只在内存或本机 `data` 目录，不上传。

---

## 2. 实现方式

### 2.1 定时模型（`src/timers.rs`）

一个 `TimerList` 管所有提醒，用 `id` 区分来源，内置两条固定 id：

```rust
pub const STAND_ID: &str = "stand-preset";
pub const SMOKE_ID: &str = "smoke-preset";
pub const STAND_TAB: usize = 12;   // 设置子页编号
pub const SMOKE_TAB: usize = 13;
pub const CUSTOM_TAB: usize = 14;
```

- `TimerMode` 只有两种：`Interval { minutes }` 与 `Daily { hour, minute }`；另有 `start_hour`/`end_hour` 生效时段、`snooze_until`、`fired_on` 当日去重。
- `stand()` / `smoke()` 取内置项，`custom()` 过滤掉这两个 id 后返回用户自建项。
- `take_due(now, hour, minute, today)` 是**唯一**的触发判定入口，不依赖 UI，便于纯逻辑单测；`App::tick_timers` 每帧调用它，只在真的触发时才 `save()`。
- `ack_timer`（知道了）、`smoke_from_alert`（抽一支）、`snooze_timer`（稍后）分别处理「从现在重新计时」/「记一根并重新计时」/「10 分钟后再弹」。

`src/smoke.rs` 只存 `Vec<i64>` 时间戳，`today` / `total` / `minutes_since_last` / `recent_days` 都是纯计算；日志保留 180 天。`recent_days` 用单次扫描 + `binary_search`，不要改回「每天各扫一遍全表」。

### 2.2 设置分组与页面编号

- `SETTINGS_GROUPS` 显式列出每个 tab 的编号；新增子页要同时改 `chip_label()` 的短名（5 个标签挤一行会把内容整体右推）和设置内容宽度计算（避免滚动条出现/消失时抖动）。
- 岛上页面：`PAGE_NAMES: [&str; 11]`，`STAND_PAGE = 9`、`SMOKE_PAGE = 10`，`dots: [Spring; 11]`；`model.rs` 的 `visible_pages` / `page_order` 也是 11 项，旧版 8/9 项的存档会在加载时自动补页。
- **任何一次改页数，都要同步 `src/ui_tests.rs` 里所有关于页数、翻页目标、可见页数组的断言**。这是本项目最容易漏的回归点。

### 2.3 数据目录（`src/model.rs`）

优先级：

1. 环境变量 `DAOZAI_DATA_DIR`
2. 引导文件 `%APPDATA%\Daozai\data-dir.txt`
3. `<程序目录>\data`
4. 只读回退 `%APPDATA%\Daozai`（先写探针文件并缓存结果）

迁移时按 `DATA_ENTRIES` 列出的文件复制（含 `timers.json`、`smoke.json`），**原目录保留一份**；`reset_data_dir` 只删引导文件，不删数据。新增任何持久化文件，记得加进 `DATA_ENTRIES`，否则换目录时会丢。

### 2.4 邮件排版数据流

```
parse_message(raw)            -> Parsed { text, html, calendar, ... }
  make_item(parsed, fetch)    -> Item { summary: body_of(parsed), html: parsed.html, ... }
  display_mail(item)          -> 验证码隐藏时清空 html（防原文漏码）
  mail_body_lines(item)       -> Arc<Vec<Line>>（按 summary+html 双指针缓存）
  mail_format::lines(html,text)
      ├─ from_html(html)      -> 结构行（标题/段落/列表/引用/表格/加粗/分隔线）
      └─ from_text(text)      -> 纯文本按空行分段
  mail_reader::body_ui(...)   -> LayoutJob 渲染，局部加粗
```

三个必须记住的点：

1. **HTML 原文要一路带到 `Item`**。早期版本只有 `summary`，`body_of()` 又优先取 `text/plain`，导致 multipart 邮件的结构全被压平，`from_html` 从未在真实链路上执行过 —— 排版看起来"改了没效果"。
2. `lines()` 用「有效文字量」择优：HTML 结果少于纯文本一半时才回退，避免纯图片营销邮件变成空白；不能用 `>=` 比较，因为结构化排版本身会丢掉标签与空白。
3. `Item` 没有 `Serialize` 派生，HTML 原文只在内存；`display_mail` 隐藏验证码时把 `html` 换成共享空 `Arc`，既防泄露又让排版缓存继续命中。

### 2.5 帧调度与缓存（`src/app.rs`）

- `frame()` 末尾按当前状态算 `repaint_after`：交互形变与图标动画 60 帧、媒体 30 帧、静态界面 1 秒。
- `icon_anim` 只对**真的有动画**的站立/抽烟页与站立/抽烟通知生效；自定义定时的秒表是静态的，不要把它拉到 60 帧。
- 动画时间必须统一用 `env.t`（程序启动后的秒数）。曾经把 `chrono::Local::now()`（Unix 时间戳，1.7e9 量级）和 `a.at` 相减再转 `f32`：该量级 ULP 约 256 秒，每帧 1/60 秒被完全量化，动画表现为「卡住很久突然跳一下」。
- `mail_body_lines` 用 `(summary, html, uid)` 的 `Arc::ptr_eq` 做 O(1) 命中判断；长邮件不要改成每帧哈希整篇正文，更不要每帧重跑正则。

### 2.6 UI 几何常量

几处散落的坐标已抽成常量/纯函数，改布局时优先动它们并补单测：

| 位置 | 抽象 |  invariants |
| --- | --- | --- |
| 通知卡片三按钮 | `extras::timer_button_row()` | 右缘 318（卡片 334 − 16），与两按钮的喝水卡片共用右基线；文本宽度按按钮左缘反推 |
| 定时列表行 | `pages::timer_row` | 轨道 208..234、编辑热区 242..375、标题 244（宽 72）、状态右对齐 371（宽 48），四者不重叠 |
| 三页左栏 | `glow(61,63)`、图标中心 `(61,63)`、数值 `(90,73)`、单位基线 71 | 站立 / 抽烟 / 定时列表三页共用 |

---

## 3. 过程：这个功能是怎么长出来的

1. **需求分四轮落地**：改名与自定义定时 + 数据目录 → 站立/抽烟升为一级功能并各自上岛 → 通知快捷按钮、图标动画、邮件排版、WorkBuddy 自动获取 → 布局细节与流畅度收尾。每一轮都先离屏渲染截图核对再交付。
2. **同步上游 v0.6.62**：上游把 WorkBuddy 整套重写为官网 OAuth（`/v2/plugin/auth/state` → 默认浏览器授权 → 轮询 token → 加密保存并自动续期），同时修了毛玻璃点击穿透、微信双开与托盘唤起、日历同步。用 `git merge-tree --write-tree HEAD v0.6.62` 预演，只有 3 个文件冲突。
3. **冲突解决策略**：`quota_ui.rs` / `workbuddy_quota.rs` 取上游；`ui_tests.rs` 保留双方新增测试（手工补函数边界）；删除自建 `browser_login.rs` 并从 `lib.rs` 移除模块。
4. **上游测试假设被打破**：上游有个测试假设「日历页 7 是最后一页」（翻页期望回到 0），本分支多了 8/9/10 三页后改成断言「离开日历页」。
5. **反复用像素说话**：字体图集、留白、图标大小这类问题，靠离屏渲染导出 PNG 后放大核对，比反复猜坐标快得多。

---

## 4. 踩过的坑（按根因分类，移植时优先查这里）

| 现象 | 根因 | 修法 |
| --- | --- | --- |
| 设置控件整体右移 / 滚动条抖动 | 5 个分组标签挤一行；内容宽度随滚动条变化 | 短名 + 固定内容宽度 + `horizontal_wrapped` |
| 数值行被推到卡片右侧 | `setting_number` 用了 `right_to_left` 布局 | 改成与其它行一样贴标签列左对齐，加对齐单测 |
| 编译报闭包返回 `()` | `settings_control_row` 要求返回 `egui::Response` | 去掉末尾多余分号 |
| `cargo test` 并发写 fingerprint 报 os error 5 | 并行任务抢同一个构建缓存 | 固定加 `--jobs 1`（必要时 `--test-threads=1`） |
| 关于页新增长文案触发字体图集像素测试失败 | 字体图集变化 | 长文案改默认折叠 |
| WorkBuddy 扫码成功却提示无效 | 新版 Chromium 在 Cookie 明文前加了 32 字节 `SHA256(host_key)` 前缀，被原样当值发出 → 官网 302 | 按 host 哈希精确剥离；该方案后续已被上游官方授权整体取代 |
| 再点「自动获取」不弹窗 | 上次 Edge 残留进程占着配置目录 | 按 `SingletonLock` 的 pid 并校验映像名后结束进程 |
| 邮件格式「改了没效果」 | `Item` 没保存 HTML 原文，`from_html` 从未在真实链路执行 | 见 §2.4 |
| 站立/烟雾动画卡顿 | 时间源混用 Unix 时间戳 + 程序时钟，f32 精度不足 | 统一 `env.t`，见 §2.5 |
| 通知卡片右侧快出界 | 三按钮右缘 330，卡片 334，右留白仅 4pt（原版 16pt） | 共用右基线 318，见 §2.6 |
| 定时列表标题压在开关上 | 轨道右端与标题起点同为 x=236；编辑热区还压着开关命中区 10pt | 抽出 `timer_row` 栅格，见 §2.6 |
| 定时列表图标与数字不平行 | 图标中心 `(60,59)`、仅 1.25 倍（约 10pt），数字 24pt | 统一到 `(61,63)` + 2 倍 |

---

## 5. 标准作业流程（SOP）

### 5.1 环境

- Windows + Rust stable MSVC 工具链 + VS 2022 Build Tools；GNU 工具链可做快速类型检查，但**发布必须用 MSVC**。
- 无 `sshpass` 之类不影响本项目；所有命令用相对路径即可，不依赖任何本机绝对路径。

### 5.2 类型检查与测试

```bash
cargo check --all-targets                 # 全目标编译检查
cargo test --jobs 1                      # 全量测试，必须串行（见 §4）
cargo test --jobs 1 --lib <测试名前缀>    # 单点回归
```

### 5.3 离屏渲染截图

`src/ui_tests.rs` 里带 `#[ignore]` 的 `timers_layout_gallery` 会导出定时三页、四种通知卡片和邮件阅读窗口的 PNG：

```bash
DAOZAI_GALLERY_DIR=<输出目录> cargo test --jobs 1 --lib timers_layout_gallery -- --ignored
```

用它核对图标基线、留白、按钮间距、邮件排版。没有图像库时，可用 Python 标准库 `zlib + struct` 手写 PNG 解码（处理 5 种 filter 反变换）后裁剪放大，不需要额外依赖。

### 5.4 打包

```bash
# 发布构建
cargo build --release --bin daozai --target x86_64-pc-windows-msvc

# 运行时依赖自检（无外部 C/C++ 运行库依赖）
powershell -ExecutionPolicy Bypass -File scripts/check-runtime.ps1 -Executable <exe>

# 安装包 + 免安装 ZIP + SHA256SUMS（脚本会自己重新构建并做路径脱敏）
powershell -ExecutionPolicy Bypass -File scripts/package.ps1 `
  -NsisCompiler <makensis.exe> -OutputDirectory dist
```

`package.ps1` 的关键点：

- 用 `CARGO_ENCODED_RUSTFLAGS` 传 `--remap-path-prefix`，把 cargo 目录与项目目录替换成中性前缀，**避免把构建机路径嵌进 exe**；它会覆盖 `.cargo/config.toml` 的 rustflags，所以必须一并带上 `crt-static`。
- 打包前会再次做运行时检查、校验 exe 版本与 `Cargo.toml` 的 `version` 一致、并扫描 exe 里是否出现构建机的用户目录 / 项目路径。
- NSIS 编译器不在 PATH 时用 `-NsisCompiler` 指定；官方 zip 里真正的编译器是 `Bin/makensis.exe`（根目录那个是转发器）。
- 产物：安装程序 `Daozai-Setup-v<版本>-x64.exe`、免安装 `Daozai-v<版本>-windows-x64.zip`、`SHA256SUMS.txt`。单文件版直接取 release 目录的 `daozai.exe`。

### 5.5 发布

- 二进制**不要提交进代码仓库**（`.gitignore` 已忽略 `/dist/`）；用 GitHub Release 资产发布，tag 建议 `v<上游版本>-custom.<序号>`。
- 上传安装包、单文件、ZIP、`SHA256SUMS.txt` 四件，Release 说明里写明基线版本、功能摘要与校验值。
- 凭据用平台提供的凭据管理器或 CI Secret，绝不写进脚本、文档或提交信息。

---

## 6. 移植到新上游版本的检查清单

按顺序执行，每步都跑一次 `cargo check --all-targets`：

1. `git fetch origin --tags --prune`，确认新 tag 与上游默认分支的净改动文件。
2. `git merge-tree --write-tree HEAD <新tag>` 预演冲突，列出冲突文件后再决定取哪边。
3. 若上游又改了 WorkBuddy / 配额模块，优先取上游，删除本分支的对应自研代码。
4. 逐项核对：`SETTINGS_GROUPS` 与 tab 编号、`PAGE_NAMES` 长度、`visible_pages` / `page_order`、`dots` 数组、`DATA_ENTRIES`。
5. 同步 `src/ui_tests.rs` 里所有页数 / 翻页 / 可见页断言。
6. `cargo test --jobs 1` 全绿；再用 §5.3 导出截图人工核对三页与通知卡片。
7. 更新 `CHANGELOG.md` 的基线版本号、`docs/USAGE.md` 与 `docs/PRIVACY.md` 中对应的功能描述。
8. 按 §5.4 / §5.5 重新打包发布。

---

## 7. 脱敏与隐私边界

提交到公开仓库或向上游提 PR 前，确认以下内容**不出现**：

- 邮箱授权码、Cookie、访问/刷新令牌、Authorization 头里的真实值；
- 真实邮箱地址、真实邮件正文、私人日程、验证码；
- 本机绝对路径、用户名、fork 所有者名、任何形如 `ghp_` / `github_pat_` 的令牌；
- `data/` 目录里的任何文件（含 `mail-accounts.json`、`timers.json`、`smoke.json`、凭据二进制）。

检查手段：

```bash
git ls-files | grep -E '\.json$|data/|\.env|\.pem|\.key$'   # 被跟踪的敏感文件
git log --all -p --format='' | grep -iE 'token|password|@[a-z0-9.-]+\.(com|cn)'  # 历史泄露
```

邮箱凭据由操作系统凭据管理器保存（keyring，UTF-16LE blob），代码只用 `secrets::load()` 读取；`Item` 不派生 `Serialize`，邮件原文不落盘；安装包脚本会主动扫描 exe 中的构建机路径。文档与提交信息里一律使用占位符。
