# URWTest-RS

[![Windows Build](https://github.com/Kazusa1085/URWTest-RS/actions/workflows/windows-build.yml/badge.svg)](https://github.com/Kazusa1085/URWTest-RS/actions/workflows/windows-build.yml)
[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)

跨平台、CLI 的已挂载卷读写测试工具，用于检测 U 盘/SSD 的真实容量、写入可靠性和冷数据保持能力。

当前版本：`0.1.0-Alpha1`

## 安全警告

这个工具会向目标卷写入大量数据，直到写满可用空间。

- 只支持**已挂载的卷根目录**，例如 `E:\` 或 `/mnt/usb`。
- 默认不会写裸设备，不会碰分区表或未挂载分区。
- **不要对系统盘运行**，否则会填满系统盘并影响系统运行。
- 测试文件会直接写入目标卷根目录；目标卷上的空闲空间会被大量占用。
- 测试结束后是否删除测试文件由 `--cleanup` 或交互式选项决定。

## 功能

- Windows / Linux 卷枚举
- 标记可移动盘
- 交互式跑圈流程
- 非交互式 `list` / `run` / `verify` / `status`
- 固定块大小写入
  - FAT32 / vfat：每个文件最多 2 GiB
  - 其他文件系统：默认单文件写到满
  - 文件过大时自动降级到 1 GiB 分块
- 确定性伪随机数据，seed 写入文件名，校验时可重建
- 写入完成后立即校验 / 延迟校验 / 下次插入校验
- 实时速度、平均速度、写入量显示
- 失败后继续或立即停止
- 测试完成后删除或保留测试文件
- 测试状态存放在用户配置目录，不占用被测卷

## 快速开始

```bash
# 查看可测试的卷
urwtest-rs list

# 交互式运行
urwtest-rs

# 写入后立即校验
urwtest-rs run --target /mnt/usb --passes 1 --verify immediate

# 只写入，下次插入后再校验
urwtest-rs run --target /mnt/usb --passes 3 --verify later

# 对已有测试数据执行校验
urwtest-rs verify --target /mnt/usb

# 查看测试状态
urwtest-rs status --target /mnt/usb
```

Windows 示例：

```powershell
urwtest-rs run --target E:\ --passes 1 --verify immediate
urwtest-rs verify --target E:\
urwtest-rs status --target E:\
```

## 常用参数

| 参数 | 说明 |
| --- | --- |
| `--target <PATH>` | 目标盘符或挂载点，必须是卷根目录 |
| `--passes <N>` | 跑圈数，默认 1 |
| `--verify immediate` | 写入完成后立即校验 |
| `--verify later` | 只写入，退出后等待下次校验 |
| `--verify delay` | 写入后等待指定秒数再校验 |
| `--delay <SECONDS>` | 配合 `--verify delay` 使用 |
| `--stop-on-fail` | 第一个文件失败就停止 |
| `--cleanup always` | 无论成功失败都删除测试文件 |
| `--cleanup on-success` | 只在成功时删除测试文件（默认） |
| `--cleanup never` | 保留测试文件 |
| `--force` | 丢弃已有测试状态并重新开始 |
| `--no-progress` | 关闭实时速度刷新 |
| `--json` | 输出机器可读 JSON |
| `--color auto\|always\|never` | 控制终端颜色 |

## 测试文件命名

测试文件直接写入目标卷根目录，命名格式为：

```text
urwtest_rs_p001of003_f00000_s1234567890123456789.bin
```

含义：

- `p001of003`：第 1 圈 / 共 3 圈
- `f00000`：文件序号
- `s...`：随机数据 seed

因此即使本机状态文件丢失，程序也可以通过扫描目标卷根目录恢复校验所需的信息。

## 状态文件位置

状态文件不写入被测卷，而是保存在用户配置目录：

- Windows：`%APPDATA%\urwtest-rs\`
- Linux：`$XDG_STATE_HOME/urwtest-rs/` 或 `~/.local/state/urwtest-rs/`

## 构建

```bash
cargo build --release
```

Windows MSVC 目标建议使用 GitHub Actions 的 `Windows Build` 工作流。也可以本地安装：

```bash
rustup target add x86_64-pc-windows-msvc
cargo install cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
```

## 开发检查

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

GitHub Actions 会在 Windows 上执行：

```text
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --release --target x86_64-pc-windows-msvc
cargo build --release --target x86_64-pc-windows-msvc
```

## 当前限制

- macOS 目前尚未实现。
- benchmark / 性能基准测试暂未实现。
- Windows 和 Linux 还需要真实 U 盘/SSD 验证。
- Linux 上部分 USB 硬盘的“可移动”识别可能不准确。
- 被测卷上的测试文件本身仍会占用目录项和文件系统元数据；程序通过容量容差避免误判。

## License

GPL-3.0-or-later，详见 [LICENSE](LICENSE)。
