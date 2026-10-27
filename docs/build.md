# 构建 Little-Mungbean / Building Little-Mungbean

## Windows

安装 Rust 稳定版、MSVC C++ 构建工具和 Windows SDK，在项目根目录运行：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-windows.ps1
```

默认输出：`release/windows/Little-Mungbean-v1.1.exe`。脚本不会覆盖已存在的发布文件。Cargo 的原始产物为 `target/x86_64-pc-windows-msvc/release/little-mungbean.exe`；指定 `CARGO_TARGET_DIR` 时使用对应目录。接口地址初始为空，占位提示为 `https://api-endpoint`。程序不再区分公开版与定制版。

Install stable Rust, the MSVC C++ build tools, and the Windows SDK, then run the command above. The executable is placed in `release/windows/`; existing releases are never overwritten. There is one edition, with an empty initial endpoint and the `https://api-endpoint` placeholder.

## macOS

Apple Silicon、macOS 13 或更高版本，安装 Xcode Command Line Tools 和 Rust 后运行：

```sh
bash scripts/build-macos.sh --beta
```

正式版使用 `--release`，版本取自 `packaging/macos/release-version.txt`；已有正式版产物不会覆盖。完整打包与签名说明见 [macOS 正式版说明](macos-release-zh.md)。

For Apple Silicon on macOS 13 or later, use `--beta` for the next beta or `--release` for a release. See the linked macOS release notes for packaging and signing.

## 检查 / Checks

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
node --test tests/renderer-inject.test.js tests/model_menu_smoke.cjs
```

界面参数在 `src/gui_layout.rs`。macOS 参数有平台限定；两平台共用 `assets/gui/svg/` 中的图标。`release/`、本地配置和开发者恢复记录不参与 Git 提交。
