# WPS 灵犀 · Agent2API 插件

Agent2API 的独立 WPS 灵犀插件。支持模型目录、流式/非流式文本对话、智点余额与每日签到。不替换「移动灵犀」插件。

### 登录和使用

在「添加账号」选择「WPS 灵犀」，只在可信的网关表单中导入自己的 `wps_sid` 值，不要粘贴整份 Cookie、转发给他人或上传到仓库。登录失效时应重新登录并更新凭证。添加后在模型管理拉取模型，在账号页点「余额」查智点。签到只处理 `daily_check_in`，按北京时间记账，不处理额外活动。上游对话可能消耗智点；不支持凭证续期、工具调用和多模态。

本插件对接的是灵犀网页服务（无公开兼容保证）。协议参考及许可见 [THIRD-PARTY-NOTICES](THIRD-PARTY-NOTICES)。

## 下载与安装

- **按版本下载**：[Releases](https://github.com/wyq2822875262/agent2api-wps-lingxi/releases)。
- **按提交下载**：[Actions](https://github.com/wyq2822875262/agent2api-wps-lingxi/actions/workflows/build.yml) → 选择成功的运行 → Artifacts。下载后解开 Actions 的外层压缩包，再把里面的 `agent2api-…-<平台>.zip` 导入 [Agent2API](https://github.com/aimod-cc/Agent2API) 的插件管理；旁边的 `.sha256` 可用于校验。
- 包按**网关所在机器**选择：Windows x64、macOS Apple Silicon（arm64）/ Intel（x64）、Linux x64 / ARM64（GNU/glibc，CI 使用 Ubuntu 22.04）。这些包未作 Agent2API 插件签名。

## 从源码构建

推荐使用与 CI 相同的 Rust 1.93.1 和 Python 3.12。在目标平台执行，`TARGET` 替换为 `pack.json` 中的目标三元组：

```sh
TARGET=x86_64-unknown-linux-gnu
cargo test --locked -p agent2api-wps-lingxi --target "$TARGET"
cargo build --locked -p agent2api-wps-lingxi --target "$TARGET"
python3 scripts/test.py --target "$TARGET"
cargo build --locked --release -p agent2api-wps-lingxi --target "$TARGET"
python3 scripts/package.py --target "$TARGET"
```

打包输出为 `dist/` 下的 ZIP 和 SHA-256 文件。测试只使用本地模拟服务，不需要真实账号；通过模拟测试不代表已与真实服务联调。

## 维护

普通 push 自动检查并为五个平台生成构建产物；也可在 Actions 手动运行。需要发布稳定下载时，同步更新 `crates/wps-lingxi/Cargo.toml`、`Cargo.lock` 和 `pack.json` 后推送 `v<版本号>` 标签。标签对应的五个平台都构建成功之后，流程才会发布 GitHub Release 并附上 ZIP 和校验文件。三个插件独立构建、独立发布。

许可：AGPL-3.0（见 [LICENSE](LICENSE)）。锁定的 SDK / Protocol 来源见 [UPSTREAM.md](UPSTREAM.md)。
