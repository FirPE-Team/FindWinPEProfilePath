# FindWinPEProfilePath

简体中文 | [English](README.md)

在 Windows PE 中按预设介质优先级查找卷根目录下的个性化标记文件或目录。

程序适合由 PECMD、批处理或其他启动脚本调用。例如，在多个磁盘中定位 `WinPE` 目录或 `Version.txt` 文件，并将第一个命中的完整 DOS 路径写入标准输出。

## 用法

```text
FindWinPEProfilePath.exe [--verbose] <relative-path>
```

示例：

```bat
FindWinPEProfilePath.exe WinPE
FindWinPEProfilePath.exe WinPE\Version.txt
FindWinPEProfilePath.exe --verbose WinPE
```

`<relative-path>` 必须是相对于候选卷根目录的路径：

- 支持文件、目录和多级路径，例如 `WinPE`、`Version.txt`、`WinPE\Version.txt`。
- 不支持绝对路径、UNC 路径、盘符、以 `\` 或 `/` 开头的路径。
- 不支持空路径、空组件、`.` 或 `..`，避免搜索越出卷根。

## 搜索顺序

程序枚举所有已分配盘符的卷，且每个分类内按盘符升序检查。找到第一个目标后立即结束。

1. `FirmwareBootDevice` 对应的启动分区。
2. `FirmwareBootDevice` 所在物理磁盘的其他分区。
3. Ventoy 数据分区，即已验证包含当前启动 ISO 的分区。
4. 其他 USB 磁盘，包括被 Windows 标记为固定磁盘的 USB 硬盘。
5. 其他可移动介质。
6. 光盘。
7. 内置固定硬盘。
8. RAM 磁盘和虚拟磁盘。
9. 网络驱动器。

> 启动分区通过注册表 `HKLM\SYSTEM\CurrentControlSet\Control\FirmwareBootDevice` 和 `\ArcName` 链接识别。启动分区本身没有盘符时，程序仍会依据物理磁盘号优先检查同盘中已有盘符的其他分区。若无法读取 `FirmwareBootDevice` 或转换 ARC 链接，程序会跳过前两项启动盘优先级，继续检查其余卷。

Ventoy 检测会读取其 UEFI 运行变量，或 ACPI `VTOY`/`iBFT` 表中的运行参数；程序同时校验 Ventoy 磁盘标识、数据分区号及当前 ISO 路径，只有三者一致的已挂载卷才进入第 3 阶段。不会为 Ventoy 盘的其他分区增加额外优先级。

## 输出与退出码

默认模式用于脚本调用：

- 找到目标：仅向 stdout 输出完整路径，例如 `F:\WinPE`，退出码为 `0`。
- 未找到目标：不输出内容，退出码为 `1`。
- 参数错误：默认向 stdout 输出用法错误信息，退出码为 `2`；传入 `--verbose` 时改为写入 stderr。
- 无法继续的系统错误：默认不输出内容，退出码为 `2`；传入 `--verbose` 时写入 stderr。

传入 `--verbose` 后，卷分类、ARC/注册表回退信息和每个检查路径会写入 stderr；命中路径仍只写入 stdout。

批处理调用示例：

```bat
setlocal EnableExtensions EnableDelayedExpansion
set "ProfileRoot="
for /f "usebackq delims=" %%P in (`FindWinPEProfilePath.exe WinPE`) do set "ProfileRoot=%%P"

if errorlevel 1 (
  echo FindWinPEProfilePath failed or the marker was not found.
) else (
  echo Found: !ProfileRoot!
)
```

## 构建

需要安装 Rust MSVC 工具链；若需要构建全部架构，还需安装对应 target。

```bat
cargo test
cargo build --release
cargo build --release --target i686-pc-windows-msvc
cargo build --release --target aarch64-pc-windows-msvc
```

也可以运行 `build.bat`：它构建 x64、x86、ARM64 版本，并在项目根目录存在 `upx.exe` 时压缩 x64 和 x86 产物。

产物位置：

```text
target\release\FindWinPEProfilePath.exe
target\i686-pc-windows-msvc\release\FindWinPEProfilePath.exe
target\aarch64-pc-windows-msvc\release\FindWinPEProfilePath.exe
```

## 限制

- 只搜索有 DOS 盘符的卷，不会为隐藏分区或无盘符分区临时分配盘符。
- 只检查卷根下的指定相对路径，不做全盘递归搜索。
- 目标是重解析点时，只要该目录项可访问就视为命中；不会验证其最终目标。
- 存储设备信息由 Windows API 提供，个别 WinPE 驱动缺失或受限时，USB/虚拟磁盘分类可能降级，但卷仍会进入后续搜索阶段。

## 许可证

MIT License

## 贡献

欢迎贡献！请随时提交问题或拉取请求。
